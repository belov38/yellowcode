use super::{BuildCtx, BuildProblem, BuildResult, hash};
use crate::managed::{Index, NameKey, NativeOwned, ScratchVec};
use crate::{QuerySchema, ir, tir};
use query_syntax::ast::JoinKind;
mod autoorder;
mod nested;
mod presentation;

pub(crate) fn build<'a, 'r, 'w>(
    ctx: &BuildCtx<'a, 'r, 'w>,
    statement: &crate::managed::statement::Statement,
    schema: &QuerySchema,
) -> BuildResult<NativeOwned<'a, ir::StmtS>> {
    ctx.checkpoint()?;
    Builder::new(ctx, statement, schema)?.statement(statement)
}

struct Builder<'c, 'a, 'r, 'w, 't> {
    ctx: &'c BuildCtx<'a, 'r, 'w>,
    schema: &'t QuerySchema,
    sources: ScratchVec<NativeOwned<'a, ir::SourceS>>,
    levels: ScratchVec<ScratchVec<ir::SrcId>>,
    params: ScratchVec<NativeOwned<'a, String>>,
    param_slot: Index<NameKey, u16>,
    values: ScratchVec<NativeOwned<'a, crate::ValuePath>>,
    tables: ScratchVec<NativeOwned<'a, crate::QTableId>>,
    seen_tables: Index<crate::QTableId, ()>,
    nested: ScratchVec<NativeOwned<'a, ir::NestedS>>,
    present: ScratchVec<NativeOwned<'a, u16>>,
    aggs: ScratchVec<NativeOwned<'a, ir::AggCall>>,
    agg_index: Index<u64, ScratchVec<u16>>,
    allowed: bool,
    query_depth: u32,
    temp_out: bool,
    single_part: bool,
}
struct Declared<'a, 't> {
    rel: NativeOwned<'a, ir::Rel>,
    joins: ScratchVec<(JoinKind, Declared<'a, 't>, &'t tir::TExpr)>,
}
impl<'c, 'a, 'r, 'w, 't> Builder<'c, 'a, 'r, 'w, 't> {
    fn new(
        ctx: &'c BuildCtx<'a, 'r, 'w>,
        statement: &crate::managed::statement::Statement,
        schema: &'t QuerySchema,
    ) -> BuildResult<Self> {
        ctx.checkpoint()?;
        let mut value = Self {
            ctx,
            schema,
            sources: ctx.scratch(0)?,
            levels: ctx.scratch(0)?,
            params: ctx.scratch(0)?,
            param_slot: Index::new(),
            values: ctx.scratch(0)?,
            tables: ctx.scratch(0)?,
            seen_tables: Index::new(),
            nested: ctx.scratch(0)?,
            present: ctx.scratch(0)?,
            aggs: ctx.scratch(0)?,
            agg_index: Index::new(),
            allowed: false,
            query_depth: 0,
            temp_out: statement.temp_out.is_some(),
            single_part: matches!(&statement.typed.kind, tir::StmtKind::Query(query) if query.parts.len() == 1),
        };
        for param in &statement.params {
            value.param(&param.name)?;
        }
        Ok(value)
    }
    fn statement(
        mut self,
        statement: &'t crate::managed::statement::Statement,
    ) -> BuildResult<NativeOwned<'a, ir::StmtS>> {
        let ctx = self.ctx;
        let (root, columns, allowed) = match &statement.typed.kind {
            tir::StmtKind::Drop(_) => (
                ctx.empty_rel()?,
                ctx.vector::<crate::ColumnInfo>(0)?.finish(),
                false,
            ),
            tir::StmtKind::Query(query) => {
                let (root, types) = self.query(query)?;
                let Some(first) = query.parts.first() else {
                    return Err(BuildProblem::Internal("a query without parts"));
                };
                let columns = if statement.temp_out.is_some() {
                    let mut columns = ctx.vector(first.items.len())?;
                    for (item, ty) in first.items.iter().zip(types.trusted_borrow()) {
                        let name = ctx.copy(&item.name)?;
                        let ty = ctx.copy(ty)?;
                        let column =
                            ctx.allocated(name.zip(ty))?
                                .map(|(name, ty)| crate::ColumnInfo {
                                    name,
                                    width: ty.width(),
                                    ty,
                                    nested: None,
                                });
                        ctx.push(&mut columns, column)?;
                    }
                    columns.finish()
                } else {
                    ctx.copy(&statement.columns)?
                };
                (root, columns, self.allowed)
            }
        };
        let auto_number = match &statement.typed.kind {
            tir::StmtKind::Query(query) if statement.temp_out.is_some() => {
                match query.parts.first() {
                    Some(first) => {
                        let is_auto = |item: &&tir::Item| {
                            matches!(
                                item.expr.kind,
                                tir::TKind::Func {
                                    f: query_syntax::Func::RecordAutoNumber,
                                    ..
                                }
                            )
                        };
                        if first.items.iter().filter(is_auto).count() > 1 {
                            return Err(BuildProblem::Later {
                                what: "несколько АВТОНОМЕРЗАПИСИ() в одном запросе",
                                stage: "Q3",
                            });
                        }
                        first
                            .items
                            .iter()
                            .position(|item| is_auto(&item))
                            .and_then(|i| u16::try_from(i).ok())
                    }
                    None => None,
                }
            }
            _ => None,
        };
        let static_analysis = statement.typed.static_analysis;
        let sources = ctx.finish(&mut self.sources)?;
        let values = ctx.finish(&mut self.values)?;
        let tables = ctx.finish(&mut self.tables)?;
        let nested = ctx.finish(&mut self.nested)?;
        let present = ctx.finish(&mut self.present)?;
        let params = ctx.finish(&mut self.params)?;
        let parts = ctx.allocated(root.zip(columns))?;
        let parts = ctx.allocated(parts.zip(sources))?;
        let parts = ctx.allocated(parts.zip(params))?;
        let parts = ctx.allocated(parts.zip(values))?;
        let parts = ctx.allocated(parts.zip(tables))?;
        let parts = ctx.allocated(parts.zip(nested))?;
        Ok(ctx.allocated(parts.zip(present))?.map(
            |(((((((root, columns), sources), params), values), tables), nested), present)| {
                ir::StmtS {
                    qr8: Vec::new(),
                    static_analysis,
                    root,
                    columns,
                    sources,
                    params,
                    values,
                    tables,
                    allowed,
                    auto_number,
                    nested,
                    present,
                    totals: None,
                    max_depth: 0,
                }
            },
        ))
    }
    fn param(&mut self, name: &str) -> BuildResult<u16> {
        let ctx = self.ctx;
        let key = ctx.managed(NameKey::folded(Some(ctx.arena), name))?;
        if let Some(slot) = self.param_slot.get(key.as_str()) {
            return Ok(*slot);
        }
        let slot = u16::try_from(self.params.len())
            .map_err(|_| BuildProblem::Internal("too many parameters"))?;
        ctx.scratch_push(&mut self.params, ctx.string(name)?)?;
        ctx.managed(self.param_slot.insert(Some(ctx.arena), key, slot))?;
        Ok(slot)
    }
    fn touch(&mut self, table: crate::QTableId) -> BuildResult<()> {
        let ctx = self.ctx;
        if ctx
            .managed(self.seen_tables.insert(Some(ctx.arena), table, ()))?
            .is_none()
        {
            ctx.scratch_push(&mut self.tables, ctx.scalar(table))?;
        }
        Ok(())
    }
    fn query(
        &mut self,
        query: &'t tir::Query,
    ) -> BuildResult<(NativeOwned<'a, ir::Rel>, NativeOwned<'a, Vec<crate::QType>>)> {
        self.query_depth = self
            .query_depth
            .checked_add(1)
            .ok_or_else(|| self.ctx.refuse(memory_budget::BudgetError::Overflow))?;
        let result = self.query_inner(query);
        self.query_depth -= 1;
        result
    }
    fn query_inner(
        &mut self,
        query: &'t tir::Query,
    ) -> BuildResult<(NativeOwned<'a, ir::Rel>, NativeOwned<'a, Vec<crate::QType>>)> {
        let ctx = self.ctx;
        if query.autoorder && query.parts.len() > 1 {
            return Err(BuildProblem::Later {
                what: "АВТОУПОРЯДОЧИВАНИЕ объединения",
                stage: "Q3",
            });
        }
        if query.totals.is_some() {
            return Err(BuildProblem::Later {
                what: "ИТОГИ",
                stage: "Q4",
            });
        }
        let Some(first) = query.parts.first() else {
            return Err(BuildProblem::Internal("a query without parts"));
        };
        let mut types = ctx.vector(first.items.len())?;
        for item in &first.items {
            ctx.push(&mut types, ctx.copy(&item.expr.ty)?)?;
        }
        if query.parts.len() == 1 {
            return Ok((
                self.select(first, Some(&query.order), query.autoorder)?,
                types.finish(),
            ));
        }
        if query
            .parts
            .iter()
            .any(|part| part.items.iter().any(|item| item.nested.is_some()))
        {
            return Err(BuildProblem::Later {
                what: "вложенная таблица в объединении",
                stage: "Q3",
            });
        }
        let mut types = types.finish();
        let mut parts = ctx.vector(query.parts.len())?;
        for (index, part) in query.parts.iter().enumerate() {
            let rel = self.select(part, None, false)?;
            ctx.parent_depth(std::iter::once(rel.depth()))?;
            ctx.push(&mut parts, rel)?;
            if index > 0 {
                let mut merged = ctx.vector(types.trusted_borrow().len())?;
                for (old, item) in types.trusted_borrow().iter().zip(&part.items) {
                    ctx.push(&mut merged, ctx.merge_type(old, &item.expr.ty)?)?;
                }
                types = merged.finish();
            }
        }
        let mut root = ctx.union(parts.finish(), ctx.copy(&query.unions)?)?;
        if !query.order.is_empty() {
            let mut keys = ctx.vector(query.order.len())?;
            for key in &query.order {
                let desc = match key.dir {
                    query_syntax::ast::OrderDir::Asc => false,
                    query_syntax::ast::OrderDir::Desc => true,
                    _ => {
                        return Err(BuildProblem::Later {
                            what: "ИЕРАРХИЯ в объединении",
                            stage: "Q3",
                        });
                    }
                };
                let column = key.col.ok_or(BuildProblem::Internal(
                    "an order key of a union outside the select list",
                ))?;
                let ty = match types.trusted_borrow().get(column) {
                    Some(ty) => ctx.copy(ty)?,
                    None => ctx.copy(&crate::QType::default())?,
                };
                let expr = self.out(column, ty)?;
                ctx.parent_depth(std::iter::once(expr.depth()))?;
                ctx.push(&mut keys, expr.map(|expr| ir::SortKey { expr, desc }))?;
            }
            root = ctx.sort(root, keys.finish(), None)?;
        }
        Ok((root, types))
    }
    fn out(
        &self,
        column: usize,
        ty: NativeOwned<'a, crate::QType>,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let index =
            u16::try_from(column).map_err(|_| BuildProblem::Internal("too many columns"))?;
        self.ctx
            .expr(self.ctx.scalar(index).map(ir::ExprKind::Out), ty)
    }
    fn select(
        &mut self,
        select: &'t tir::Select,
        order: Option<&'t [tir::OrderKey]>,
        auto: bool,
    ) -> BuildResult<NativeOwned<'a, ir::Rel>> {
        let ctx = self.ctx;
        self.allowed |= select.allowed;
        if select.items.iter().any(|item| item.nested.is_some()) {
            if self.query_depth != 1 {
                return Err(BuildProblem::Later {
                    what: "вложенная таблица в подзапросе",
                    stage: "Q3",
                });
            }
            if self.temp_out {
                return Err(BuildProblem::Later {
                    what: "вложенная таблица во временной таблице",
                    stage: "Q3",
                });
            }
            if select.distinct || select.group.is_some() || select.having.is_some() {
                return Err(BuildProblem::Later {
                    what: "вложенная таблица в запросе с группировкой",
                    stage: "Q3",
                });
            }
        }
        let mut ids = ctx.scratch(0)?;
        let mut declared = ctx.scratch(select.sources.len())?;
        for source in &select.sources {
            let source = self.declare(source, &mut ids)?;
            ctx.scratch_push(&mut declared, source)?;
        }
        ctx.scratch_push(&mut self.levels, ids)?;
        let outer_aggs = std::mem::replace(&mut self.aggs, ctx.scratch(0)?);
        let outer_index = std::mem::take(&mut self.agg_index);
        let result = self.select_body(select, order, auto, declared);
        self.aggs = outer_aggs;
        self.agg_index = outer_index;
        self.levels.pop();
        result
    }
    fn select_body(
        &mut self,
        select: &'t tir::Select,
        order: Option<&'t [tir::OrderKey]>,
        auto: bool,
        mut declared: ScratchVec<Declared<'a, 't>>,
    ) -> BuildResult<NativeOwned<'a, ir::Rel>> {
        let ctx = self.ctx;
        declared.reverse();
        let mut from = None;
        while let Some(source) = declared.pop() {
            let r = self.assemble(source)?;
            from = Some(match from {
                None => r,
                Some(l) => {
                    let on = ctx.expr(
                        ctx.scalar(true)
                            .map(crate::QValue::Bool)
                            .map(ir::ExprKind::Lit),
                        ctx.copy(&crate::QType::boolean())?,
                    )?;
                    ctx.join(JoinKind::Inner, l, r, on)?
                }
            });
        }
        let mut root = match from {
            Some(root) => root,
            None => ctx.empty_rel()?,
        };
        if let Some(input) = &select.where_ {
            let pred = self.expr(input)?;
            root = ctx.filter(root, pred)?;
        }
        let mut exprs = ctx.vector(select.items.len())?;
        for (col, item) in select.items.iter().enumerate() {
            let expr = if item.nested.is_some() {
                self.nested_item(item, col)?
            } else if let Some(expr) = self.presentation_item(select, item, col)? {
                expr
            } else {
                self.expr(&item.expr)?
            };
            ctx.parent_depth(std::iter::once(expr.depth()))?;
            ctx.push(&mut exprs, expr)?;
        }
        let mut exprs = exprs.finish();
        let mut having = match &select.having {
            Some(h) => Some(self.expr(h)?),
            None => None,
        };
        let aggregated = select.group.is_some() || select.having.is_some() || !self.aggs.is_empty();
        let mut hidden = ctx.scratch(0)?;
        let mut sort = ctx.vector(0)?;
        let mut hier = None;
        let mut items_index: Option<Index<u64, ScratchVec<usize>>> = None;
        if let Some(order) = order.filter(|order| !order.is_empty()) {
            let hierarchy = order.iter().any(|key| {
                matches!(
                    key.dir,
                    query_syntax::ast::OrderDir::Hierarchy
                        | query_syntax::ast::OrderDir::HierarchyDesc
                )
            });
            if hierarchy && (order.len() != 1 || aggregated || select.distinct) {
                return Err(BuildProblem::Later {
                    what: "ИЕРАРХИЯ с несколькими ключами или группировкой",
                    stage: "Q3",
                });
            }
            for key in order {
                let desc = matches!(
                    key.dir,
                    query_syntax::ast::OrderDir::Desc | query_syntax::ast::OrderDir::HierarchyDesc
                );
                let (base, col) = match key.col {
                    Some(col) => {
                        let item = select.items.get(col).ok_or(BuildProblem::Internal(
                            "an order key names a missing column",
                        ))?;
                        if item.nested.is_some() {
                            return Err(BuildProblem::Later {
                                what: "сортировка по вложенной таблице",
                                stage: "Q3",
                            });
                        }
                        if u16::try_from(col).is_ok_and(|col| {
                            self.present.iter().any(|c| *c.trusted_borrow() == col)
                        }) {
                            return Err(BuildProblem::Later {
                                what: "сортировка по представлению",
                                stage: "Q3",
                            });
                        }
                        let e = exprs
                            .trusted_borrow()
                            .get(col)
                            .ok_or(BuildProblem::Internal(
                                "an order key names a missing column",
                            ))?;
                        (self.copy_expr(e)?, Some(col))
                    }
                    None => {
                        let e = self.expr(&key.expr)?;
                        if items_index.is_none() {
                            let mut index = Index::new();
                            for (slot, item) in exprs.trusted_borrow().iter().enumerate() {
                                bucket_push(ctx, &mut index, expression_hash(item), slot)?;
                            }
                            items_index = Some(index);
                        }
                        let wanted = expression_hash(e.trusted_borrow());
                        let same = items_index
                            .as_ref()
                            .and_then(|index| index.get(&wanted))
                            .and_then(|bucket| {
                                bucket.iter().copied().find(|&slot| {
                                    exprs.trusted_borrow().get(slot).map(|item| &item.kind)
                                        == Some(&e.trusted_borrow().kind)
                                })
                            });
                        (e, same)
                    }
                };
                if hierarchy {
                    if autoorder::last_deref_targets(base.trusted_borrow())
                        .iter()
                        .any(|(table, _)| {
                            self.role_field(*table, crate::schema::StdRole::Parent)
                                .is_some()
                        })
                    {
                        return Err(BuildProblem::Later {
                            what: "ИЕРАРХИЯ по полю через разыменование",
                            stage: "Q3",
                        });
                    }
                    let source = autoorder::first_src(base.trusted_borrow());
                    let pair = match source {
                        Some(src) => self.hierarchy_of(src)?,
                        None => None,
                    };
                    match pair {
                        Some((id, parent)) => {
                            hier = Some((
                                push_hidden(ctx, &mut hidden, select.items.len(), id)?,
                                push_hidden(ctx, &mut hidden, select.items.len(), parent)?,
                            ));
                        }
                        None if base
                            .trusted_borrow()
                            .ty
                            .refs
                            .iter()
                            .any(|a| self.schema.hierarchical_table(*a).is_some()) =>
                        {
                            return Err(BuildProblem::Later {
                                what: "ИЕРАРХИЯ по ссылке вне таблицы объекта",
                                stage: "Q3",
                            });
                        }
                        None => {}
                    }
                }
                if auto && !base.trusted_borrow().ty.refs.is_empty() {
                    let mut keys = self.auto_keys(base.trusted_borrow())?;
                    keys.reverse();
                    while let Some(key) = keys.pop() {
                        self.hidden_sort(&mut hidden, &mut sort, select.items.len(), key, desc)?;
                    }
                    continue;
                }
                let ty = ctx.copy(&base.trusted_borrow().ty)?;
                let col = match col {
                    Some(col) => col,
                    None => usize::from(push_hidden(ctx, &mut hidden, select.items.len(), base)?),
                };
                let e = self.out(col, ty)?;
                ctx.parent_depth(std::iter::once(e.depth()))?;
                ctx.push(&mut sort, e.map(|expr| ir::SortKey { expr, desc }))?;
            }
        } else if auto && !aggregated && !select.distinct {
            let source = self.levels.last().and_then(|ids| ids.first()).copied();
            if let Some(src) = source
                && let Some(mut main) = self.main_order(src)?
            {
                if let Some((id, parent)) = main.hier {
                    hier = Some((
                        push_hidden(ctx, &mut hidden, select.items.len(), id)?,
                        push_hidden(ctx, &mut hidden, select.items.len(), parent)?,
                    ));
                }
                main.keys.reverse();
                while let Some(key) = main.keys.pop() {
                    let mut keys = self.auto_keys(key.trusted_borrow())?;
                    keys.reverse();
                    while let Some(key) = keys.pop() {
                        self.hidden_sort(&mut hidden, &mut sort, select.items.len(), key, false)?;
                    }
                }
            }
        } else if auto && aggregated {
            if let Some(sets) = &select.group {
                if sets.len() > 1 {
                    return Err(BuildProblem::Later {
                        what: "АВТОУПОРЯДОЧИВАНИЕ с несколькими наборами группировки",
                        stage: "Q3",
                    });
                }
                for group in sets.iter().flatten() {
                    let key = self.expr(group)?;
                    let refs = !key.trusted_borrow().ty.refs.is_empty();
                    let mut keys = self.auto_keys(key.trusted_borrow())?;
                    keys.reverse();
                    while let Some(key) = keys.pop() {
                        self.hidden_sort(&mut hidden, &mut sort, select.items.len(), key, false)?;
                    }
                    if refs {
                        self.hidden_sort(&mut hidden, &mut sort, select.items.len(), key, false)?;
                    }
                }
            }
        } else if auto {
            return Err(BuildProblem::Later {
                what: "АВТОУПОРЯДОЧИВАНИЕ с РАЗЛИЧНЫЕ",
                stage: "Q3",
            });
        }
        if aggregated && select.items.iter().any(|item| item.nested.is_some()) {
            return Err(BuildProblem::Later {
                what: "вложенная таблица в запросе с группировкой",
                stage: "Q3",
            });
        }
        if aggregated {
            let mut keys = ctx.scratch(0)?;
            let mut index = Index::new();
            let mut sets = ctx.vector(select.group.as_ref().map_or(0, Vec::len))?;
            if let Some(group) = &select.group {
                for set in group {
                    let mut slots = ctx.vector(set.len())?;
                    for key in set {
                        let e = self.expr(key)?;
                        let slot = push_key(ctx, &mut keys, &mut index, e)?;
                        ctx.push(&mut slots, ctx.scalar(slot))?;
                    }
                    ctx.push(&mut sets, slots.finish())?;
                }
            }
            let own = self.levels.last().map_or(&[][..], |ids| &ids[..]);
            let rekey = super::rebuild::Rebuild {
                ctx,
                keys: &keys,
                index: &index,
                own,
                anchor: own.first().copied(),
                in_sub: false,
                replace: true,
            };
            exprs = rekey.exprs(exprs.trusted_borrow())?;
            if let Some(old) = &having {
                having = Some(rekey.expr(old.trusted_borrow())?);
            }
            for item in &mut hidden[..] {
                *item = rekey.expr(item.trusted_borrow())?;
            }
            if exprs
                .trusted_borrow()
                .iter()
                .chain(having.iter().map(NativeOwned::trusted_borrow))
                .chain(hidden.iter().map(NativeOwned::trusted_borrow))
                .any(|e| rekey.correlated(e))
            {
                return Err(BuildProblem::Later {
                    what: "коррелированный подзапрос над группировкой",
                    stage: "Q3",
                });
            }
            for agg in &mut self.aggs[..] {
                let old = agg.trusted_borrow();
                if old.f != query_syntax::Func::GroupedBy {
                    continue;
                }
                let slot = old.arg.as_ref().and_then(|arg| rekey.find(arg)).ok_or(
                    BuildProblem::Later {
                        what: "СГРУППИРОВАНОПО по выражению вне группировки",
                        stage: "Q3",
                    },
                )?;
                let arg = ctx
                    .expr(
                        ctx.scalar(()).map(|()| {
                            ir::ExprKind::Lit(crate::QValue::Number(bsl_prim::Number::from_u64(
                                u64::from(slot),
                            )))
                        }),
                        ctx.copy(&crate::QType::num(crate::types::NumQ {
                            digits: 5,
                            scale: 0,
                            nonneg: true,
                        }))?,
                    )?
                    .map(Some);
                let ty = ctx.copy(&old.ty)?;
                *agg = ctx.allocated(arg.zip(ty))?.map(|(arg, ty)| ir::AggCall {
                    f: query_syntax::Func::GroupedBy,
                    distinct: false,
                    arg,
                    ty,
                });
            }
            let sets = if select.group.as_ref().is_some_and(|group| group.len() > 1) {
                sets.finish().map(Some)
            } else {
                drop(sets);
                ctx.scalar(()).map(|()| None)
            };
            let keys = ctx.finish(&mut keys)?;
            let aggs = ctx.finish(&mut self.aggs)?;
            root = ctx.aggregate(root, keys, sets, aggs)?;
            if let Some(having) = having {
                root = ctx.filter(root, having)?;
            }
        }
        if !hidden.is_empty() {
            let capacity = exprs
                .trusted_borrow()
                .len()
                .checked_add(hidden.len())
                .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?;
            let mut output = ctx.vector(capacity)?;
            for e in exprs.trusted_borrow() {
                let e = self.copy_expr(e)?;
                ctx.parent_depth(std::iter::once(e.depth()))?;
                ctx.push(&mut output, e)?;
            }
            hidden.reverse();
            while let Some(e) = hidden.pop() {
                ctx.parent_depth(std::iter::once(e.depth()))?;
                ctx.push(&mut output, e)?;
            }
            exprs = output.finish();
        }
        root = ctx.project(root, exprs)?;
        if select.distinct {
            let depth = ctx.parent_depth(std::iter::once(root.depth()))?;
            root = ctx.boxed(root)?.map(ir::Rel::Distinct).set_depth(depth);
        }
        if !sort.items().is_empty() {
            root = ctx.sort(root, sort.finish(), hier)?;
        }
        if let Some(n) = select.top {
            let depth = ctx.parent_depth(std::iter::once(root.depth()))?;
            root = ctx
                .boxed(root)?
                .map(|input| ir::Rel::Top { input, n })
                .set_depth(depth);
        }
        Ok(root)
    }
    fn hidden_sort(
        &self,
        hidden: &mut ScratchVec<NativeOwned<'a, ir::Expr>>,
        sort: &mut crate::managed::NativeVec<'a, ir::SortKey>,
        visible: usize,
        key: NativeOwned<'a, ir::Expr>,
        desc: bool,
    ) -> BuildResult<()> {
        let ctx = self.ctx;
        let ty = ctx.copy(&key.trusted_borrow().ty)?;
        let slot = push_hidden(ctx, hidden, visible, key)?;
        let expr = self.out(usize::from(slot), ty)?;
        ctx.parent_depth(std::iter::once(expr.depth()))?;
        ctx.push(sort, expr.map(|expr| ir::SortKey { expr, desc }))
    }
    fn declare(
        &mut self,
        source: &'t tir::Source,
        ids: &mut ScratchVec<ir::SrcId>,
    ) -> BuildResult<Declared<'a, 't>> {
        let ctx = self.ctx;
        let slot = self.sources.len();
        let id =
            ir::SrcId(u32::try_from(slot).map_err(|_| BuildProblem::Internal("too many sources"))?);
        ctx.scratch_push(ids, id)?;
        let placeholder = ctx
            .vector::<crate::QType>(0)?
            .finish()
            .map(|columns| ir::SourceS {
                id,
                all_fields: source.all_fields,
                kind: ir::SrcKind::Subquery { columns },
            });
        ctx.scratch_push(&mut self.sources, placeholder)?;
        let (kind, rel) = match &source.from {
            tir::From::Table { id: table, .. } => {
                match self.schema.table(*table).kind {
                    crate::TableKind::Virtual(
                        crate::VtKind::TaskByPerformer | crate::VtKind::FilterCriterion,
                    ) => {
                        return Err(BuildProblem::Later {
                            what: "Q7d-ManagedExpansion",
                            stage: "Q7d",
                        });
                    }
                    crate::TableKind::Virtual(v) if v != crate::VtKind::BusinessProcessPoints => {
                        return Err(BuildProblem::Later {
                            what: "виртуальные таблицы",
                            stage: "Q5",
                        });
                    }
                    crate::TableKind::Changes => {
                        return Err(BuildProblem::Later {
                            what: ".Изменения",
                            stage: "Q5",
                        });
                    }
                    _ => {}
                }
                self.touch(*table)?;
                let kind = ctx.scalar(*table).map(ir::SrcKind::Table);
                let depth = ctx.parent_depth(std::iter::empty())?;
                let rel = ctx
                    .scalar(*table)
                    .map(|table| ir::Rel::Scan { src: id, table })
                    .set_depth(depth);
                (kind, rel)
            }
            tir::From::Temp(table) => {
                let columns = self.temp_columns(table)?;
                let stored = self.temp_stored(table)?;
                let kind = ctx
                    .allocated(columns.zip(stored))?
                    .map(|(columns, stored)| ir::SrcKind::Temp { columns, stored });
                let depth = ctx.parent_depth(std::iter::empty())?;
                let rel = ctx
                    .copy(&table.name)?
                    .map(|name| ir::Rel::TempScan { src: id, name })
                    .set_depth(depth);
                (kind, rel)
            }
            tir::From::Param { name, schema } => {
                let param = self.param(name)?;
                let columns = self.temp_columns(schema)?;
                let stored = self.temp_stored(schema)?;
                let kind = ctx
                    .allocated(columns.zip(stored))?
                    .map(|(columns, stored)| ir::SrcKind::Temp { columns, stored });
                let depth = ctx.parent_depth(std::iter::empty())?;
                let rel = ctx
                    .scalar(param)
                    .map(|param| ir::Rel::ParamScan { src: id, param })
                    .set_depth(depth);
                (kind, rel)
            }
            tir::From::Subquery(query) => {
                let (input, columns) = self.query(query)?;
                let kind = columns.map(|columns| ir::SrcKind::Subquery { columns });
                (kind, ctx.subquery(id, input)?)
            }
        };
        let replacement = kind.map(|kind| ir::SourceS {
            id,
            kind,
            all_fields: source.all_fields,
        });
        let entry = self
            .sources
            .get_mut(slot)
            .ok_or(BuildProblem::Internal("too many sources"))?;
        *entry = replacement;
        let mut joins = ctx.scratch(source.joins.len())?;
        for join in &source.joins {
            let right = self.declare(&join.right, ids)?;
            ctx.scratch_push(&mut joins, (join.kind, right, &join.on))?;
        }
        Ok(Declared { rel, joins })
    }
    fn temp_columns(
        &self,
        schema: &crate::TempSchema,
    ) -> BuildResult<NativeOwned<'a, Vec<crate::QType>>> {
        let ctx = self.ctx;
        let mut columns = ctx.vector(schema.columns.len())?;
        for column in &schema.columns {
            let ty = ctx.copy(&column.ty)?.map(Option::unwrap_or_default);
            ctx.push(&mut columns, ty)?;
        }
        Ok(columns.finish())
    }
    fn temp_stored(
        &self,
        schema: &crate::TempSchema,
    ) -> BuildResult<NativeOwned<'a, Vec<Option<crate::NumQ>>>> {
        let ctx = self.ctx;
        let mut stored = ctx.vector(schema.columns.len())?;
        for column in &schema.columns {
            ctx.push(&mut stored, ctx.scalar(column.stored))?;
        }
        Ok(stored.finish())
    }
    fn assemble(&mut self, source: Declared<'a, 't>) -> BuildResult<NativeOwned<'a, ir::Rel>> {
        let Declared { mut rel, mut joins } = source;
        joins.reverse();
        while let Some((kind, right, on)) = joins.pop() {
            let right = self.assemble(right)?;
            let on = self.expr(on)?;
            rel = self.ctx.join(kind, rel, right, on)?;
        }
        Ok(rel)
    }
    fn field(
        &mut self,
        ty: &crate::QType,
        level: u16,
        source: u16,
        col: tir::Col,
        derefs: &[tir::Deref],
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        let ids = self
            .levels
            .len()
            .checked_sub(1 + usize::from(level))
            .and_then(|i| self.levels.get(i))
            .ok_or(BuildProblem::Internal(
                "a field of a level that is not visible",
            ))?;
        let src = ids
            .get(usize::from(source))
            .copied()
            .ok_or(BuildProblem::Internal("a field of a missing source"))?;
        let col = match col {
            tir::Col::Field(id) => ir::ColSel::Field(id),
            tir::Col::Column(index) => ir::ColSel::Column(index),
        };
        let base_ty = if derefs.is_empty() {
            ctx.copy(ty)?
        } else {
            self.column_type(src, col)?
        };
        let base = ctx.expr(
            ctx.scalar((src, col))
                .map(|(src, col)| ir::ExprKind::Col { src, col }),
            base_ty,
        )?;
        if derefs.is_empty() {
            return Ok(base);
        }
        let mut steps = ctx.vector(derefs.len())?;
        for deref in derefs {
            for &(table, _) in &deref.targets {
                self.touch(table)?;
            }
            ctx.push(&mut steps, ctx.copy(deref)?)?;
        }
        ctx.parent_depth(std::iter::once(base.depth()))?;
        let base = ctx.boxed(base)?;
        let kind = ctx
            .allocated(base.zip(steps.finish()))?
            .map(|(base, steps)| ir::ExprKind::Deref { base, steps });
        ctx.expr(kind, ctx.copy(ty)?)
    }
    fn column_type(
        &self,
        src: ir::SrcId,
        col: ir::ColSel,
    ) -> BuildResult<NativeOwned<'a, crate::QType>> {
        let ctx = self.ctx;
        let ty = self
            .sources
            .get(src.0 as usize)
            .map(|v| v.trusted_borrow())
            .and_then(|source| match (&source.kind, col) {
                (ir::SrcKind::Table(table), ir::ColSel::Field(field)) => {
                    self.schema.field(*table, field).map(|field| &field.ty)
                }
                (
                    ir::SrcKind::Subquery { columns } | ir::SrcKind::Temp { columns, .. },
                    ir::ColSel::Column(index),
                ) => columns.get(usize::from(index)),
                _ => None,
            });
        match ty {
            Some(ty) => ctx.copy(ty),
            None => ctx.copy(&crate::QType::default()),
        }
    }
    fn aggregate_call(
        &self,
        f: query_syntax::Func,
        distinct: bool,
        arg: Option<NativeOwned<'a, ir::Expr>>,
        ty: &crate::QType,
    ) -> BuildResult<NativeOwned<'a, ir::AggCall>> {
        let ctx = self.ctx;
        let arg = match arg {
            Some(arg) => arg.map(Some),
            None => ctx.scalar(()).map(|()| None),
        };
        Ok(ctx
            .allocated(arg.zip(ctx.copy(ty)?))?
            .map(|(arg, ty)| ir::AggCall {
                f,
                distinct,
                arg,
                ty,
            }))
    }
    fn agg(&mut self, call: NativeOwned<'a, ir::AggCall>) -> BuildResult<u16> {
        use std::hash::{Hash, Hasher};
        let ctx = self.ctx;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let borrowed = call.trusted_borrow();
        std::mem::discriminant(&borrowed.f).hash(&mut hasher);
        borrowed.distinct.hash(&mut hasher);
        if let Some(arg) = &borrowed.arg {
            hash::shallow(arg, &mut hasher);
        }
        let hash = hasher.finish();
        if let Some(bucket) = self.agg_index.get(&hash) {
            for &index in bucket {
                if self
                    .aggs
                    .get(usize::from(index))
                    .map(NativeOwned::trusted_borrow)
                    == Some(borrowed)
                {
                    return Ok(index);
                }
            }
        }
        let index = u16::try_from(self.aggs.len())
            .map_err(|_| BuildProblem::Internal("too many aggregates"))?;
        ctx.parent_depth(std::iter::once(call.depth()))?;
        ctx.scratch_push(&mut self.aggs, call)?;
        if self.agg_index.get(&hash).is_none() {
            let bucket = ctx.scratch(0)?;
            ctx.managed(self.agg_index.insert(Some(ctx.arena), hash, bucket))?;
        }
        let bucket = self
            .agg_index
            .get_mut(&hash)
            .ok_or(BuildProblem::Internal("too many aggregates"))?;
        ctx.scratch_push(bucket, index)?;
        Ok(index)
    }
    fn exprs(&mut self, inputs: &'t [tir::TExpr]) -> BuildResult<NativeOwned<'a, Vec<ir::Expr>>> {
        let ctx = self.ctx;
        let mut values = ctx.vector(inputs.len())?;
        for input in inputs {
            let value = self.expr(input)?;
            ctx.parent_depth(std::iter::once(value.depth()))?;
            ctx.push(&mut values, value)?;
        }
        Ok(values.finish())
    }
    fn maybe_box(
        &mut self,
        input: Option<&'t tir::TExpr>,
    ) -> BuildResult<NativeOwned<'a, Option<Box<ir::Expr>>>> {
        let ctx = self.ctx;
        match input {
            None => Ok(NativeOwned::none(Some(ctx.arena))),
            Some(input) => {
                let value = self.expr(input)?;
                ctx.parent_depth(std::iter::once(value.depth()))?;
                Ok(ctx.boxed(value)?.into_some())
            }
        }
    }
    fn touch_hierarchy(&mut self, inputs: &[tir::TExpr]) -> BuildResult<()> {
        for input in inputs {
            for atom in &input.ty.refs {
                if let Some(table) = self.schema.hierarchical_table(*atom) {
                    self.touch(table)?;
                }
            }
        }
        Ok(())
    }
    fn wrap_deref(
        &mut self,
        base: NativeOwned<'a, ir::Expr>,
        derefs: &[tir::Deref],
        ty: &crate::QType,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        ctx.parent_depth(std::iter::once(base.depth()))?;
        let mut steps = ctx.vector(derefs.len())?;
        for deref in derefs {
            for &(table, _) in &deref.targets {
                self.touch(table)?;
            }
            ctx.push(&mut steps, ctx.copy(deref)?)?;
        }
        let base = ctx.boxed(base)?;
        let kind = ctx
            .allocated(base.zip(steps.finish()))?
            .map(|(base, steps)| ir::ExprKind::Deref { base, steps });
        ctx.expr(kind, ctx.copy(ty)?)
    }
    fn expr(&mut self, input: &'t tir::TExpr) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        let kind = match &input.kind {
            tir::TKind::Const(value) => ctx.copy(value)?.map(ir::ExprKind::Lit),
            tir::TKind::Param(name) => ctx.scalar(self.param(name)?).map(ir::ExprKind::Param),
            tir::TKind::Value(path) => {
                let slot = u16::try_from(self.values.len())
                    .map_err(|_| BuildProblem::Internal("too many ЗНАЧЕНИЕ slots"))?;
                ctx.scratch_push(&mut self.values, ctx.copy(path)?)?;
                ctx.scalar(slot).map(ir::ExprKind::Value)
            }
            tir::TKind::Field {
                level,
                source,
                col,
                derefs,
            } => return self.field(&input.ty, *level, *source, *col, derefs),
            tir::TKind::Neg(value) | tir::TKind::Not(value) => {
                let value = self.expr(value)?;
                ctx.parent_depth(std::iter::once(value.depth()))?;
                let value = ctx.boxed(value)?;
                if matches!(input.kind, tir::TKind::Neg(_)) {
                    value.map(ir::ExprKind::Neg)
                } else {
                    value.map(ir::ExprKind::Not)
                }
            }
            tir::TKind::Arith { first, steps } => {
                let first = self.expr(first)?;
                ctx.parent_depth(std::iter::once(first.depth()))?;
                let first = ctx.boxed(first)?;
                let mut values = ctx.vector(steps.len())?;
                for step in steps {
                    let rhs = self.expr(&step.rhs)?;
                    ctx.parent_depth(std::iter::once(rhs.depth()))?;
                    let pair = ctx.allocated(ctx.scalar(step.op).zip(rhs))?;
                    let value = ctx
                        .allocated(pair.zip(ctx.copy(&step.ty)?))?
                        .map(|((op, rhs), ty)| (op, rhs, ty));
                    ctx.push(&mut values, value)?;
                }
                ctx.allocated(first.zip(values.finish()))?
                    .map(|(first, steps)| ir::ExprKind::Arith { first, steps })
            }
            tir::TKind::And(values) => self.exprs(values)?.map(ir::ExprKind::And),
            tir::TKind::Or(values) => self.exprs(values)?.map(ir::ExprKind::Or),
            tir::TKind::Compare(op, l, r) => {
                let l = self.expr(l)?;
                let r = self.expr(r)?;
                ctx.parent_depth([l.depth(), r.depth()].into_iter())?;
                let l = ctx.boxed(l)?;
                let r = ctx.boxed(r)?;
                ctx.allocated(l.zip(r))?
                    .map(|(l, r)| ir::ExprKind::Compare(*op, l, r))
            }
            tir::TKind::InList {
                l,
                not,
                hierarchy,
                rhs,
            } => {
                if *hierarchy {
                    self.touch_hierarchy(l)?;
                }
                let l = self.exprs(l)?;
                let rhs = self.exprs(rhs)?;
                ctx.allocated(l.zip(rhs))?
                    .map(|(l, rhs)| ir::ExprKind::InList {
                        l,
                        not: *not,
                        hierarchy: *hierarchy,
                        rhs,
                    })
            }
            tir::TKind::InQuery {
                l,
                not,
                hierarchy,
                q,
            } => {
                if *hierarchy {
                    self.touch_hierarchy(l)?;
                }
                let l = self.exprs(l)?;
                let (q, _) = self.query(q)?;
                ctx.parent_depth([l.depth(), q.depth()].into_iter())?;
                let q = ctx.boxed(q)?;
                ctx.allocated(l.zip(q))?
                    .map(|(l, q)| ir::ExprKind::InQuery {
                        l,
                        not: *not,
                        hierarchy: *hierarchy,
                        q,
                    })
            }
            tir::TKind::Between { l, not, lo, hi } => {
                let l = self.expr(l)?;
                let lo = self.expr(lo)?;
                let hi = self.expr(hi)?;
                ctx.parent_depth([l.depth(), lo.depth(), hi.depth()].into_iter())?;
                let l = ctx.boxed(l)?;
                let lo = ctx.boxed(lo)?;
                let hi = ctx.boxed(hi)?;
                let pair = ctx.allocated(l.zip(lo))?;
                ctx.allocated(pair.zip(hi))?
                    .map(|((l, lo), hi)| ir::ExprKind::Between {
                        l,
                        not: *not,
                        lo,
                        hi,
                    })
            }
            tir::TKind::IsNull { l, not } => {
                let l = self.expr(l)?;
                ctx.parent_depth(std::iter::once(l.depth()))?;
                ctx.boxed(l)?.map(|l| ir::ExprKind::IsNull { l, not: *not })
            }
            tir::TKind::Refs { l, table } => {
                let l = self.expr(l)?;
                ctx.parent_depth(std::iter::once(l.depth()))?;
                ctx.boxed(l)?
                    .map(|l| ir::ExprKind::Refs { l, table: *table })
            }
            tir::TKind::Like {
                l,
                not,
                pattern,
                escape,
            } => {
                let l = self.expr(l)?;
                let pat = self.expr(pattern)?;
                ctx.parent_depth([l.depth(), pat.depth()].into_iter())?;
                let l = ctx.boxed(l)?;
                let pat = ctx.boxed(pat)?;
                ctx.allocated(l.zip(pat))?
                    .map(|(l, pat)| ir::ExprKind::Like {
                        l,
                        not: *not,
                        pat,
                        esc: *escape,
                    })
            }
            tir::TKind::Case {
                operand,
                whens,
                else_,
            } => {
                let operand = self.maybe_box(operand.as_deref())?;
                let mut values = ctx.vector(whens.len())?;
                for (condition, result) in whens {
                    let condition = self.expr(condition)?;
                    let result = self.expr(result)?;
                    ctx.parent_depth([condition.depth(), result.depth()].into_iter())?;
                    ctx.push(&mut values, ctx.allocated(condition.zip(result))?)?;
                }
                let else_ = self.maybe_box(else_.as_deref())?;
                let pair = ctx.allocated(operand.zip(values.finish()))?;
                ctx.allocated(pair.zip(else_))?
                    .map(|((operand, whens), else_)| ir::ExprKind::Case {
                        operand,
                        whens,
                        else_,
                    })
            }
            tir::TKind::Cast { e, to, derefs, num } => {
                let x = self.expr(e)?;
                ctx.parent_depth(std::iter::once(x.depth()))?;
                let x = ctx.boxed(x)?;
                let kind = ctx
                    .allocated(x.zip(ctx.copy(to)?))?
                    .map(|(x, to)| ir::ExprKind::Cast { x, to, num: *num });
                let cast_ty = if derefs.is_empty() { &input.ty } else { to };
                let cast = ctx.expr(kind, ctx.copy(cast_ty)?)?;
                if derefs.is_empty() {
                    return Ok(cast);
                }
                return self.wrap_deref(cast, derefs, &input.ty);
            }
            tir::TKind::Func { f, args } => {
                match f {
                    query_syntax::Func::Presentation | query_syntax::Func::RefPresentation => {
                        return Err(BuildProblem::Later {
                            what: "ПРЕДСТАВЛЕНИЕ",
                            stage: "Q3",
                        });
                    }
                    query_syntax::Func::StoredDataSize => {
                        return Err(BuildProblem::Later {
                            what: "РАЗМЕРХРАНИМЫХДАННЫХ",
                            stage: "Q3",
                        });
                    }
                    query_syntax::Func::RecordAutoNumber => {
                        return ctx.expr(
                            ctx.scalar(())
                                .map(|()| ir::ExprKind::Lit(crate::QValue::Null)),
                            ctx.copy(&input.ty)?,
                        );
                    }
                    query_syntax::Func::GroupedBy => {
                        let arg = match args.as_slice() {
                            [arg] => Some(self.expr(arg)?),
                            _ => {
                                return Err(BuildProblem::Internal(
                                    "СГРУППИРОВАНОПО without exactly one argument",
                                ));
                            }
                        };
                        let call = self.aggregate_call(*f, false, arg, &input.ty)?;
                        let index = self.agg(call)?;
                        return ctx.expr(
                            ctx.scalar(index).map(ir::ExprKind::AggRef),
                            ctx.copy(&input.ty)?,
                        );
                    }
                    _ => {}
                }
                self.exprs(args)?
                    .map(|args| ir::ExprKind::Func { f: *f, args })
            }
            tir::TKind::Unit(unit) => ctx.scalar(*unit).map(ir::ExprKind::Unit),
            tir::TKind::Agg { nested: true, .. } => {
                return Err(BuildProblem::Later {
                    what: "агрегат по табличной части в оплачиваемом построении",
                    stage: "Q3a",
                });
            }
            tir::TKind::Agg {
                f,
                distinct,
                arg,
                nested: false,
            } => {
                let arg = match arg {
                    Some(arg) => Some(self.expr(arg)?),
                    None => None,
                };
                let call = self.aggregate_call(*f, *distinct, arg, &input.ty)?;
                ctx.scalar(self.agg(call)?).map(ir::ExprKind::AggRef)
            }
        };
        ctx.expr(kind, ctx.copy(&input.ty)?)
    }
}

fn push_key<'a>(
    ctx: &BuildCtx<'a, '_, '_>,
    keys: &mut ScratchVec<NativeOwned<'a, ir::Expr>>,
    index: &mut Index<u64, ScratchVec<u16>>,
    e: NativeOwned<'a, ir::Expr>,
) -> BuildResult<u16> {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash::shallow(e.trusted_borrow(), &mut h);
    let hash = h.finish();
    if let Some(bucket) = index.get(&hash) {
        for &i in bucket {
            if keys.get(usize::from(i)).map(|k| &k.trusted_borrow().kind)
                == Some(&e.trusted_borrow().kind)
            {
                return Ok(i);
            }
        }
    }
    let slot =
        u16::try_from(keys.len()).map_err(|_| BuildProblem::Internal("too many grouping keys"))?;
    ctx.parent_depth(std::iter::once(e.depth()))?;
    ctx.scratch_push(keys, e)?;
    if index.get(&hash).is_none() {
        let bucket = ctx.scratch(0)?;
        ctx.managed(index.insert(Some(ctx.arena), hash, bucket))?;
    }
    let bucket = index
        .get_mut(&hash)
        .ok_or(BuildProblem::Internal("too many grouping keys"))?;
    ctx.scratch_push(bucket, slot)?;
    Ok(slot)
}

fn expression_hash(e: &ir::Expr) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash::shallow(e, &mut h);
    h.finish()
}
fn push_hidden<'a>(
    ctx: &BuildCtx<'a, '_, '_>,
    hidden: &mut ScratchVec<NativeOwned<'a, ir::Expr>>,
    visible: usize,
    e: NativeOwned<'a, ir::Expr>,
) -> BuildResult<u16> {
    let index = visible
        .checked_add(hidden.len())
        .and_then(|i| u16::try_from(i).ok())
        .ok_or(BuildProblem::Internal("too many columns"))?;
    ctx.parent_depth(std::iter::once(e.depth()))?;
    ctx.scratch_push(hidden, e)?;
    Ok(index)
}

fn bucket_push<T: Copy>(
    ctx: &BuildCtx<'_, '_, '_>,
    index: &mut Index<u64, ScratchVec<T>>,
    hash: u64,
    slot: T,
) -> BuildResult<()> {
    if index.get(&hash).is_none() {
        let bucket = ctx.scratch(0)?;
        ctx.managed(index.insert(Some(ctx.arena), hash, bucket))?;
    }
    let bucket = index
        .get_mut(&hash)
        .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?;
    ctx.scratch_push(bucket, slot)
}
