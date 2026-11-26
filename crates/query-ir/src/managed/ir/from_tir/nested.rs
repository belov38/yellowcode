use super::*;
use crate::schema::{FieldRole, QFieldId, QTableId, StdRole};

#[derive(Clone, Copy)]
pub(super) struct NestedOwner {
    owner_src: ir::SrcId,
    owner_table: Option<QTableId>,
    owner_field: QFieldId,
    section_table: QTableId,
    section_ref_field: QFieldId,
    section_depth: usize,
}

impl<'c, 'a, 'r, 'w, 't> Builder<'c, 'a, 'r, 'w, 't> {
    pub(super) fn forward_nested(
        &mut self,
        item: &'t tir::Item,
        col: usize,
    ) -> BuildResult<Option<NativeOwned<'a, ir::Expr>>> {
        let Some(column) = tir::forwarded_nested_column(item) else {
            return Ok(None);
        };
        let Some((source, recipes)) = &self.nested_source else {
            return Ok(None);
        };
        if self.query_depth != 1 || self.levels.last().and_then(|ids| ids.first()) != Some(source) {
            return Ok(None);
        }
        let Some(recipe) = recipes
            .iter()
            .find(|recipe| recipe.trusted_borrow().col == column)
        else {
            return Ok(None);
        };
        let ctx = self.ctx;
        let col = u16::try_from(col).map_err(|_| BuildProblem::Internal("too many columns"))?;
        let recipe = copy_forwarded_recipe(ctx, recipe.trusted_borrow(), col)?;
        let expr = self.expr(&item.expr)?;
        ctx.scratch_push(&mut self.nested, recipe)?;
        Ok(Some(expr))
    }

    pub(super) fn nested_item(
        &mut self,
        item: &'t tir::Item,
        col: usize,
        retain: bool,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let ctx = self.ctx;
        let col = u16::try_from(col).map_err(|_| BuildProblem::Internal("too many columns"))?;
        if matches!(item.expr.kind, tir::TKind::Const(_)) {
            ctx.scratch_push(
                &mut self.nested,
                ctx.scalar(()).map(|()| ir::NestedS { col, rows: None }),
            )?;
            return ctx.expr(
                ctx.scalar(())
                    .map(|()| ir::ExprKind::Lit(crate::QValue::Null)),
                ctx.copy(&item.expr.ty)?,
            );
        }
        if item.unknown {
            return Err(BuildProblem::Internal(
                "a nested table of an unknown type in Execute",
            ));
        }
        let (owner_ref, tab) = self.owner_of(&item.expr)?;
        if self.query_depth == 1
            && self
                .nested
                .iter()
                .any(|nested| nested.trusted_borrow().col == col)
        {
            return Ok(owner_ref);
        }
        let sources_before = self.sources.len();
        let src = self.section_source(tab, item.nested.iter().flatten().any(|item| item.star))?;
        let owner = self.section_col(src, tab, StdRole::Ref)?;
        let number = self.section_col(src, tab, StdRole::LineNumber)?;
        let mut outer = ctx.scratch(0)?;
        for level in &self.levels {
            for &id in level {
                ctx.scratch_push(&mut outer, id)?;
            }
        }
        let outer_aggs = std::mem::replace(&mut self.aggs, ctx.scratch(0)?);
        let outer_index = std::mem::take(&mut self.agg_index);
        let mut ids = ctx.scratch(1)?;
        ctx.scratch_push(&mut ids, src)?;
        ctx.scratch_push(&mut self.levels, ids)?;
        let next_nested_owner =
            self.nested_owner_context(owner_ref.trusted_borrow(), owner.trusted_borrow(), tab);
        let prev_nested_owner = std::mem::replace(&mut self.nested_owner, next_nested_owner);
        let result = (|| {
            let items = item.nested.as_deref().unwrap_or_default();
            let mut output = ctx.vector(items.len())?;
            for item in items {
                if item.nested.is_some() {
                    return Err(BuildProblem::Later {
                        what: "вложенная таблица во вложенной",
                        stage: "Q3",
                    });
                }
                let e = self.expr(&item.expr)?;
                ctx.parent_depth(std::iter::once(e.depth()))?;
                ctx.push(&mut output, e)?;
            }
            Ok(output.finish())
        })();
        self.nested_owner = prev_nested_owner;
        self.levels.pop();
        let had_aggs = !self.aggs.is_empty();
        self.aggs = outer_aggs;
        self.agg_index = outer_index;
        let exprs = result?;
        if had_aggs {
            return Err(BuildProblem::Later {
                what: "агрегат во вложенной таблице",
                stage: "Q3",
            });
        }
        let mut reads_owner = false;
        for e in exprs.trusted_borrow() {
            ir::visit_expr_in_place(e, true, &mut |x| {
                if let ir::ExprKind::Col { src, .. } = &x.kind {
                    reads_owner |= outer.contains(src);
                }
            });
        }
        if reads_owner {
            return Err(BuildProblem::Later {
                what: "поле владельца во вложенной таблице",
                stage: "Q3",
            });
        }
        let depth = ctx.parent_depth(std::iter::empty())?;
        let scan = ctx
            .scalar((src, tab))
            .map(|(src, table)| ir::Rel::Scan { src, table })
            .set_depth(depth);
        let parts = ctx.allocated(scan.zip(owner))?;
        let parts = ctx.allocated(parts.zip(number))?;
        let rows = ctx
            .allocated(parts.zip(exprs))?
            .map(|(((scan, owner), number), exprs)| ir::NestedRows {
                scan,
                owner,
                number,
                exprs,
            });
        ctx.parent_depth(std::iter::once(rows.depth()))?;
        ctx.scratch_push(
            &mut self.nested,
            rows.map(|rows| ir::NestedS {
                col,
                rows: Some(rows),
            }),
        )?;
        if !retain {
            drop(self.nested.pop());
            while self.sources.len() > sources_before {
                drop(self.sources.pop());
            }
        }
        Ok(owner_ref)
    }

    fn nested_owner_context(
        &self,
        owner_ref: &ir::Expr,
        section_owner: &ir::Expr,
        section_table: QTableId,
    ) -> Option<NestedOwner> {
        let (
            ir::ExprKind::Col {
                src: owner_src,
                col: ir::ColSel::Field(owner_field),
            },
            ir::ExprKind::Col {
                col: ir::ColSel::Field(section_ref_field),
                ..
            },
        ) = (&owner_ref.kind, &section_owner.kind)
        else {
            return None;
        };
        let owner_table = match self
            .sources
            .get(owner_src.0 as usize)
            .map(|source| &source.trusted_borrow().kind)
        {
            Some(ir::SrcKind::Table(table))
                if self
                    .schema
                    .field(*table, *owner_field)
                    .is_some_and(|field| field.role == FieldRole::Std(StdRole::Ref)) =>
            {
                Some(*table)
            }
            _ => None,
        };
        Some(NestedOwner {
            owner_src: *owner_src,
            owner_table,
            owner_field: *owner_field,
            section_table,
            section_ref_field: *section_ref_field,
            section_depth: self.levels.len(),
        })
    }

    pub(super) fn rewrite_nested_owner_field(
        &mut self,
        ty: &crate::QType,
        level: u16,
        source: u16,
        col: tir::Col,
        derefs: &[tir::Deref],
    ) -> BuildResult<Option<NativeOwned<'a, ir::Expr>>> {
        let Some(no) = self.nested_owner else {
            return Ok(None);
        };
        let tir::Col::Field(field) = col else {
            return Ok(None);
        };
        let resolved_src = self
            .levels
            .len()
            .checked_sub(1 + usize::from(level))
            .and_then(|index| self.levels.get(index))
            .and_then(|ids| ids.get(usize::from(source)))
            .copied();
        if resolved_src != Some(no.owner_src) {
            return Ok(None);
        }
        let Some(section_level) = self
            .levels
            .len()
            .checked_sub(no.section_depth)
            .and_then(|delta| u16::try_from(delta).ok())
        else {
            return Ok(None);
        };
        let has_nested_deref = |steps: &[tir::Deref]| {
            steps.iter().flat_map(|step| &step.targets).any(|(t, f)| {
                self.schema
                    .field(*t, *f)
                    .is_none_or(|field| field.nested.is_some())
            })
        };
        if field == no.owner_field {
            if let Some((tab_step, after_tab)) = derefs.split_first()
                && !tab_step.targets.is_empty()
                && tab_step.targets.iter().all(|(t, f)| {
                    self.schema
                        .field(*t, *f)
                        .is_some_and(|fi| fi.nested == Some(no.section_table))
                })
            {
                let Some((sec_step, rest)) = after_tab.split_first() else {
                    return Ok(None);
                };
                let &[(t, sec_field)] = sec_step.targets.as_slice() else {
                    return Ok(None);
                };
                if t != no.section_table || has_nested_deref(rest) {
                    return Ok(None);
                }
                let saved = self.nested_owner.take();
                let result = self.field(ty, section_level, 0, tir::Col::Field(sec_field), rest);
                self.nested_owner = saved;
                return result.map(Some);
            }
            if has_nested_deref(derefs) {
                return Ok(None);
            }
            let saved = self.nested_owner.take();
            let result = self.field(
                ty,
                section_level,
                0,
                tir::Col::Field(no.section_ref_field),
                derefs,
            );
            self.nested_owner = saved;
            return result.map(Some);
        }
        if has_nested_deref(derefs) {
            return Ok(None);
        }
        let Some(owner_table) = no.owner_table else {
            return Ok(None);
        };
        let Some(field_info) = self.schema.field(owner_table, field) else {
            return Ok(None);
        };
        if field_info.nested == Some(no.section_table) {
            let Some((first, rest)) = derefs.split_first() else {
                return Ok(None);
            };
            let &[(t, sec_field)] = first.targets.as_slice() else {
                return Ok(None);
            };
            if t != no.section_table {
                return Ok(None);
            }
            let saved = self.nested_owner.take();
            let result = self.field(ty, section_level, 0, tir::Col::Field(sec_field), rest);
            self.nested_owner = saved;
            return result.map(Some);
        }
        if field_info.nested.is_some() {
            return Ok(None);
        }
        let ctx = self.ctx;
        let empty_ty = crate::QType::default();
        let owner_ref_ty = self
            .schema
            .field(owner_table, no.owner_field)
            .map_or(&empty_ty, |f| &f.ty);
        if derefs.is_empty() && self.schema.is_presentation_field(owner_table, field) {
            if !self.in_select_item {
                return Ok(None);
            }
            let saved = self.nested_owner.take();
            let ref_expr = self.field(
                owner_ref_ty,
                section_level,
                0,
                tir::Col::Field(no.section_ref_field),
                &[],
            );
            self.nested_owner = saved;
            let ref_expr = ref_expr?;
            let mut args = ctx.vector(1)?;
            ctx.parent_depth(std::iter::once(ref_expr.depth()))?;
            ctx.push(&mut args, ref_expr)?;
            let kind = args.finish().map(|args| ir::ExprKind::Func {
                f: query_syntax::Func::Presentation,
                args,
            });
            return ctx.expr(kind, ctx.copy(ty)?).map(Some);
        }
        let step_name = if !field_info.name_en.is_empty() {
            field_info.name_en.as_str()
        } else {
            field_info.name_ru.as_str()
        };
        let trailing_presentation = self.in_select_item
            && derefs.last().is_some_and(|last| {
                !last.targets.is_empty()
                    && last
                        .targets
                        .iter()
                        .all(|(t, f)| self.schema.is_presentation_field(*t, *f))
            });
        let saved = self.nested_owner.take();
        let base = self.field(
            owner_ref_ty,
            section_level,
            0,
            tir::Col::Field(no.section_ref_field),
            &[],
        );
        self.nested_owner = saved;
        let base = base?;
        let make_first_step = |this: &mut Self| -> BuildResult<NativeOwned<'a, tir::Deref>> {
            this.touch(owner_table)?;
            let name = ctx.string(step_name)?;
            let mut targets = ctx.vector(1)?;
            ctx.push(&mut targets, ctx.scalar((owner_table, field)))?;
            ctx.allocated(name.zip(targets.finish()))
                .map(|pair| pair.map(|(name, targets)| tir::Deref { name, targets }))
        };
        if trailing_presentation {
            let Some((last, head)) = derefs.split_last() else {
                return Ok(None);
            };
            let mut pres_ty = ctx.copy(&crate::QType::default())?;
            for &(table, _) in &last.targets {
                if let Some(atom) = self.schema.table(table).reference {
                    let mut refs = ctx.vector(1)?;
                    ctx.push(&mut refs, ctx.scalar(atom))?;
                    let reference = refs.finish().map(|refs| crate::QType {
                        refs,
                        ..crate::QType::default()
                    });
                    pres_ty =
                        ctx.merge_type(pres_ty.trusted_borrow(), reference.trusted_borrow())?;
                }
            }
            let cap = 1usize
                .checked_add(head.len())
                .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?;
            let mut steps = ctx.vector(cap)?;
            ctx.push(&mut steps, make_first_step(self)?)?;
            for deref in head {
                for &(table, _) in &deref.targets {
                    self.touch(table)?;
                }
                ctx.push(&mut steps, ctx.copy(deref)?)?;
            }
            ctx.parent_depth(std::iter::once(base.depth()))?;
            let base = ctx.boxed(base)?;
            let ref_kind = ctx
                .allocated(base.zip(steps.finish()))?
                .map(|(base, steps)| ir::ExprKind::Deref { base, steps });
            let ref_expr = ctx.expr(ref_kind, pres_ty)?;
            let mut args = ctx.vector(1)?;
            ctx.parent_depth(std::iter::once(ref_expr.depth()))?;
            ctx.push(&mut args, ref_expr)?;
            let kind = args.finish().map(|args| ir::ExprKind::Func {
                f: query_syntax::Func::Presentation,
                args,
            });
            return ctx.expr(kind, ctx.copy(ty)?).map(Some);
        }
        let cap = 1usize
            .checked_add(derefs.len())
            .ok_or_else(|| ctx.refuse(memory_budget::BudgetError::Overflow))?;
        let mut steps = ctx.vector(cap)?;
        ctx.push(&mut steps, make_first_step(self)?)?;
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
        ctx.expr(kind, ctx.copy(ty)?).map(Some)
    }

    fn owner_of(
        &mut self,
        e: &'t tir::TExpr,
    ) -> BuildResult<(NativeOwned<'a, ir::Expr>, QTableId)> {
        let ctx = self.ctx;
        match &e.kind {
            tir::TKind::Field {
                level,
                source,
                col,
                derefs,
            } => {
                let src = self
                    .levels
                    .len()
                    .checked_sub(1 + usize::from(*level))
                    .and_then(|i| self.levels.get(i))
                    .and_then(|ids| ids.get(usize::from(*source)))
                    .copied()
                    .ok_or(BuildProblem::Internal(
                        "a nested table of a source that is not visible",
                    ))?;
                let col = match col {
                    tir::Col::Field(f) => ir::ColSel::Field(*f),
                    tir::Col::Column(i) => ir::ColSel::Column(*i),
                };
                let base = ctx.expr(
                    ctx.scalar((src, col))
                        .map(|(src, col)| ir::ExprKind::Col { src, col }),
                    self.column_type(src, col)?,
                )?;
                match derefs.split_last() {
                    None => {
                        let (ir::ColSel::Field(field), Some(ir::SrcKind::Table(table))) = (
                            col,
                            self.sources
                                .get(src.0 as usize)
                                .map(|s| &s.trusted_borrow().kind),
                        ) else {
                            return Err(BuildProblem::Later {
                                what: "вложенная таблица не из таблицы",
                                stage: "Q3",
                            });
                        };
                        let table = *table;
                        let tab = self
                            .schema
                            .field(table, field)
                            .and_then(|f| f.nested)
                            .ok_or(BuildProblem::Internal(
                                "a nested table of a field that is not a section",
                            ))?;
                        Ok((self.section_col(src, table, StdRole::Ref)?, tab))
                    }
                    Some((last, init)) => {
                        let tab = self.step_section(last)?;
                        Ok((self.owner_deref(base, init)?, tab))
                    }
                }
            }
            tir::TKind::Cast {
                e: x,
                to,
                derefs,
                num,
            } => {
                let (last, init) = derefs.split_last().ok_or(BuildProblem::Internal(
                    "a nested table of a cast without a dereference",
                ))?;
                let tab = self.step_section(last)?;
                for step in init {
                    for &(table, _) in &step.targets {
                        self.touch(table)?;
                    }
                }
                let x = self.expr(x)?;
                ctx.parent_depth(std::iter::once(x.depth()))?;
                let x = ctx.boxed(x)?;
                let kind = ctx
                    .allocated(x.zip(ctx.copy(to)?))?
                    .map(|(x, to)| ir::ExprKind::Cast { x, to, num: *num });
                let cast = ctx.expr(kind, ctx.copy(to)?)?;
                Ok((self.owner_deref(cast, init)?, tab))
            }
            _ => Err(BuildProblem::Later {
                what: "вложенная таблица из выражения",
                stage: "Q3",
            }),
        }
    }
    fn owner_deref(
        &mut self,
        base: NativeOwned<'a, ir::Expr>,
        steps: &[tir::Deref],
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        if steps.is_empty() {
            return Ok(base);
        }
        let ctx = self.ctx;
        for step in steps {
            for &(table, _) in &step.targets {
                self.touch(table)?;
            }
        }
        ctx.parent_depth(std::iter::once(base.depth()))?;
        let base = ctx.boxed(base)?;
        let mut copied = ctx.vector(steps.len())?;
        for step in steps {
            ctx.push(&mut copied, ctx.copy(step)?)?;
        }
        let kind = ctx
            .allocated(base.zip(copied.finish()))?
            .map(|(base, steps)| ir::ExprKind::Deref { base, steps });
        ctx.expr(kind, ctx.copy(&crate::QType::default())?)
    }
    fn step_section(&self, step: &tir::Deref) -> BuildResult<QTableId> {
        let mut found = None;
        for &(table, field) in &step.targets {
            if let Some(tab) = self.schema.field(table, field).and_then(|f| f.nested) {
                if found.is_some_and(|old| old != tab) {
                    return Err(BuildProblem::Later {
                        what: "вложенная таблица составного ссылочного типа",
                        stage: "Q3",
                    });
                }
                found = Some(tab);
            }
        }
        found.ok_or(BuildProblem::Internal(
            "a nested table step without a section",
        ))
    }
    pub(super) fn section_source(
        &mut self,
        table: QTableId,
        all_fields: bool,
    ) -> BuildResult<ir::SrcId> {
        let ctx = self.ctx;
        let id = ir::SrcId(
            u32::try_from(self.sources.len())
                .map_err(|_| BuildProblem::Internal("too many sources"))?,
        );
        ctx.scratch_push(
            &mut self.sources,
            ctx.scalar((id, table, all_fields))
                .map(|(id, table, all_fields)| ir::SourceS {
                    id,
                    all_fields,
                    kind: ir::SrcKind::Table(table),
                }),
        )?;
        self.touch(table)?;
        Ok(id)
    }
    pub(super) fn section_col(
        &self,
        src: ir::SrcId,
        table: QTableId,
        role: StdRole,
    ) -> BuildResult<NativeOwned<'a, ir::Expr>> {
        let field = self
            .schema
            .table(table)
            .fields
            .iter()
            .position(|f| f.role == FieldRole::Std(role))
            .and_then(|i| u16::try_from(i).ok())
            .map(QFieldId)
            .ok_or(BuildProblem::Internal(
                "a table without a standard field of a nested table",
            ))?;
        let ty = self.schema.field(table, field).map(|f| &f.ty);
        let ty = match ty {
            Some(ty) => self.ctx.copy(ty)?,
            None => self.ctx.copy(&crate::QType::default())?,
        };
        self.ctx.expr(
            self.ctx
                .scalar((src, field))
                .map(|(src, f)| ir::ExprKind::Col {
                    src,
                    col: ir::ColSel::Field(f),
                }),
            ty,
        )
    }
}

fn copy_forwarded_recipe<'a>(
    ctx: &BuildCtx<'a, '_, '_>,
    recipe: &ir::NestedS,
    col: u16,
) -> BuildResult<NativeOwned<'a, ir::NestedS>> {
    let Some(rows) = &recipe.rows else {
        return Ok(ctx.scalar(col).map(|col| ir::NestedS { col, rows: None }));
    };
    let keys = ctx.scratch(0)?;
    let index = Index::new();
    let ref_index = Index::new();
    let copier = super::super::rebuild::Rebuild {
        ctx,
        keys: &keys,
        index: &index,
        ref_index: &ref_index,
        own: &[],
        anchor: None,
        in_sub: false,
        replace: false,
        allow_prefix: false,
        explicit_keys: 0,
    };
    let scan = copier.rel(&rows.scan)?;
    let owner = copier.expr(&rows.owner)?;
    let number = copier.expr(&rows.number)?;
    let exprs = copier.exprs(&rows.exprs)?;
    let parts = ctx.allocated(scan.zip(owner))?;
    let parts = ctx.allocated(parts.zip(number))?;
    let rows = ctx
        .allocated(parts.zip(exprs))?
        .map(|(((scan, owner), number), exprs)| ir::NestedRows {
            scan,
            owner,
            number,
            exprs,
        });
    ctx.parent_depth(std::iter::once(rows.depth()))?;
    Ok(rows.map(|rows| ir::NestedS {
        col,
        rows: Some(rows),
    }))
}
