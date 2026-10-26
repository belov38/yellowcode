use super::{
    AggCall, BuildError, ColSel, Expr, ExprKind, NestedRows, NestedS, Rel, SortKey, SourceS, SrcId,
    SrcKind, StmtS,
};
use crate::schema::{
    FieldRole, QFieldId, QTableId, StdRole, TableKind, ValuePath, VtKind, VtParamKind,
};
use crate::tir::{self, Col, From, Query, Select, Source, TExpr, TKind};
use crate::types::QType;
use crate::value::QValue;
use crate::{ColumnInfo, CompiledStmt, QuerySchema};
use bsl_prim::fold_name;
use query_syntax::ast::{JoinKind, OrderDir};
use query_syntax::{Func, with_deep_stack};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

mod autoorder;
mod qr8;
mod totals;

type R<T> = Result<T, BuildError>;

fn internal<T>(s: &str) -> R<T> {
    Err(BuildError::Internal(s.to_string()))
}

pub fn build_stmt(c: &CompiledStmt, schema: &QuerySchema) -> Result<StmtS, BuildError> {
    let depth = c.typed.max_depth;
    let tir::StmtKind::Query(q) = &c.typed.kind else {
        return Ok(StmtS {
            qr8: Vec::new(),
            static_analysis: c.typed.static_analysis,
            root: Rel::Empty,
            columns: Vec::new(),
            sources: Vec::new(),
            params: Vec::new(),
            values: Vec::new(),
            tables: Vec::new(),
            allowed: false,
            auto_number: None,
            nested: Vec::new(),
            present: Vec::new(),
            totals: None,
            max_depth: depth,
        });
    };
    with_deep_stack(depth, || {
        let mut b = Builder::new(c, schema);
        let (root, types) = b.query(q)?;
        let columns = match (&c.temp_out, q.parts.first()) {
            (Some(_), Some(first)) => first
                .items
                .iter()
                .zip(&types)
                .map(|(it, ty)| ColumnInfo {
                    name: it.name.clone(),
                    ty: ty.clone(),
                    width: ty.width(),
                    nested: None,
                })
                .collect(),
            _ => c.columns.clone(),
        };
        let auto_number = match q.parts.first() {
            Some(first) if c.temp_out.is_some() => {
                let is_auto = |i: &&tir::Item| {
                    matches!(
                        &i.expr.kind,
                        TKind::Func {
                            f: Func::RecordAutoNumber,
                            ..
                        }
                    )
                };
                if first.items.iter().filter(is_auto).count() > 1 {
                    return Err(BuildError::Later {
                        what: "несколько АВТОНОМЕРЗАПИСИ() в одном запросе",
                        stage: "Q3",
                    });
                }
                first
                    .items
                    .iter()
                    .position(|i| is_auto(&i))
                    .and_then(|p| u16::try_from(p).ok())
            }
            _ => None,
        };
        Ok(StmtS {
            qr8: std::mem::take(&mut b.qr8),
            static_analysis: c.typed.static_analysis,
            root,
            columns,
            sources: std::mem::take(&mut b.sources),
            params: std::mem::take(&mut b.params),
            values: std::mem::take(&mut b.values),
            tables: std::mem::take(&mut b.tables),
            allowed: b.allowed,
            auto_number,
            nested: std::mem::take(&mut b.nested),
            present: std::mem::take(&mut b.present),
            totals: b.totals.take(),
            max_depth: depth.saturating_add(b.joins),
        })
    })
}

struct Declared<'a> {
    rel: Rel,
    joins: Vec<(JoinKind, Declared<'a>, &'a TExpr)>,
}

struct Builder<'a> {
    qr8: Vec<super::Qr8Param>,
    schema: &'a QuerySchema,
    sources: Vec<SourceS>,
    levels: Vec<Vec<SrcId>>,
    params: Vec<String>,
    param_slot: HashMap<String, u16>,
    values: Vec<ValuePath>,
    tables: Vec<QTableId>,
    seen_tables: HashSet<QTableId>,
    aggs: Vec<AggCall>,
    agg_index: HashMap<u64, Vec<u16>>,
    joins: u32,
    allowed: bool,
    nested: Vec<NestedS>,
    present: Vec<u16>,
    single_part: bool,
    query_depth: u32,
    pending_auto: bool,
    temp_out: bool,
    section: Option<SectionMap>,
    totals: Option<crate::totals::TotalsS>,
    totals_open: bool,
    totals_extra: Vec<&'a TExpr>,
    totals_extra_cols: Vec<u16>,
    totals_extra_built: Vec<Expr>,
    root_sort: Vec<SortMeta>,
    col_names: Vec<String>,
}

struct SortMeta {
    col: usize,
    desc: bool,
    base_col: Option<usize>,
    base: Expr,
    key: Expr,
}

#[derive(Clone, Copy)]
struct SectionMap {
    owner_src: SrcId,
    field: QFieldId,
    src: SrcId,
}

impl<'a> Builder<'a> {
    fn new(c: &CompiledStmt, schema: &'a QuerySchema) -> Builder<'a> {
        let mut b = Builder {
            qr8: Vec::new(),
            schema,
            sources: Vec::new(),
            levels: Vec::new(),
            params: Vec::new(),
            param_slot: HashMap::new(),
            values: Vec::new(),
            tables: Vec::new(),
            seen_tables: HashSet::new(),
            aggs: Vec::new(),
            agg_index: HashMap::new(),
            joins: 0,
            allowed: false,
            nested: Vec::new(),
            present: Vec::new(),
            single_part: false,
            query_depth: 0,
            pending_auto: false,
            temp_out: c.temp_out.is_some(),
            section: None,
            totals: None,
            totals_open: false,
            totals_extra: Vec::new(),
            totals_extra_cols: Vec::new(),
            totals_extra_built: Vec::new(),
            root_sort: Vec::new(),
            col_names: c.columns.iter().map(|c| c.name.clone()).collect(),
        };
        for p in &c.params {
            let _ = b.param(&p.name);
        }
        b
    }

    fn param(&mut self, name: &str) -> R<u16> {
        let key = fold_name(name);
        if let Some(i) = self.param_slot.get(&key) {
            return Ok(*i);
        }
        let Ok(i) = u16::try_from(self.params.len()) else {
            return internal("too many parameters");
        };
        self.params.push(name.to_string());
        self.param_slot.insert(key, i);
        Ok(i)
    }

    fn touch(&mut self, t: QTableId) {
        if self.seen_tables.insert(t) {
            self.tables.push(t);
        }
    }

    fn touch_hierarchy(&mut self, l: &[TExpr]) {
        let mut tables = Vec::new();
        for x in l {
            tables.extend(
                x.ty.refs
                    .iter()
                    .filter_map(|a| self.schema.hierarchical_table(*a)),
            );
        }
        for t in tables {
            self.touch(t);
        }
    }

    fn query(&mut self, q: &'a Query) -> R<(Rel, Vec<QType>)> {
        self.query_depth += 1;
        let r = self.query_inner(q);
        self.query_depth -= 1;
        r
    }

    fn query_inner(&mut self, q: &'a Query) -> R<(Rel, Vec<QType>)> {
        if q.autoorder && q.parts.len() > 1 {
            return later("АВТОУПОРЯДОЧИВАНИЕ объединения", "Q3");
        }
        let Some(first) = q.parts.first() else {
            return internal("a query without parts");
        };
        if let Some(t) = &q.totals {
            if self.query_depth != 1 || self.temp_out {
                return later("ИТОГИ вне запроса оператора", "Q4");
            }
            self.totals_prepare(q, t)?;
            let (rel, types) = self.query_rows(q, first)?;
            self.totals = Some(self.totals_finish(q, t, &types)?);
            self.totals_open = false;
            return Ok((rel, types));
        }
        self.query_rows(q, first)
    }

    fn query_rows(&mut self, q: &'a Query, first: &'a Select) -> R<(Rel, Vec<QType>)> {
        if self.query_depth == 1 {
            self.single_part = q.parts.len() == 1;
        }
        let mut types: Vec<QType> = first.items.iter().map(|i| i.expr.ty.clone()).collect();
        if q.parts.len() == 1 {
            self.pending_auto = q.autoorder;
            let rel = self.select(first, Some(&q.order))?;
            return Ok((rel, types));
        }
        if q.parts
            .iter()
            .any(|p| p.items.iter().any(|i| i.nested.is_some()))
        {
            return later("вложенная таблица в объединении", "Q3");
        }
        let mut parts = Vec::with_capacity(q.parts.len());
        for (i, p) in q.parts.iter().enumerate() {
            parts.push(self.select(p, None)?);
            if i > 0 {
                for (t, it) in types.iter_mut().zip(&p.items) {
                    *t = t.merge(&it.expr.ty);
                }
            }
        }
        let mut rel = Rel::Union {
            parts,
            all: q.unions.clone(),
        };
        if !q.order.is_empty() {
            let mut keys = Vec::with_capacity(q.order.len());
            for k in &q.order {
                let desc = order_desc(k.dir)?;
                let Some(col) = k.col else {
                    return internal("an order key of a union outside the select list");
                };
                let ty = types.get(col).cloned().unwrap_or_default();
                keys.push(SortKey {
                    expr: out(col, ty)?,
                    desc,
                });
            }
            rel = Rel::Sort {
                input: Box::new(rel),
                keys,
            };
        }
        Ok((rel, types))
    }

    fn select(&mut self, sel: &'a Select, order: Option<&'a [tir::OrderKey]>) -> R<Rel> {
        let auto = std::mem::take(&mut self.pending_auto);
        self.allowed |= sel.allowed;
        if sel.items.iter().any(|i| i.nested.is_some()) {
            if self.query_depth != 1 {
                return later("вложенная таблица в подзапросе", "Q3");
            }
            if self.temp_out {
                return later("вложенная таблица во временной таблице", "Q3");
            }
            if sel.distinct || sel.group.is_some() || sel.having.is_some() {
                return later("вложенная таблица в запросе с группировкой", "Q3");
            }
        }
        let outer_aggs = std::mem::take(&mut self.aggs);
        let outer_index = std::mem::take(&mut self.agg_index);
        let res = self.select_inner(sel, order, auto);
        self.aggs = outer_aggs;
        self.agg_index = outer_index;
        res
    }

    fn select_inner(
        &mut self,
        sel: &'a Select,
        order: Option<&'a [tir::OrderKey]>,
        auto: bool,
    ) -> R<Rel> {
        let mut ids = Vec::new();
        let mut declared = Vec::with_capacity(sel.sources.len());
        for s in &sel.sources {
            declared.push(self.declare(s, &mut ids)?);
        }
        self.levels.push(ids);
        let res = self.select_body(sel, order, auto, declared);
        self.levels.pop();
        res
    }

    fn select_body(
        &mut self,
        sel: &'a Select,
        order: Option<&'a [tir::OrderKey]>,
        auto: bool,
        declared: Vec<Declared<'a>>,
    ) -> R<Rel> {
        let mut from = None;
        for d in declared {
            let r = self.assemble(d)?;
            from = Some(match from {
                None => r,
                Some(l) => Rel::Join {
                    kind: JoinKind::Inner,
                    l: Box::new(l),
                    r: Box::new(r),
                    on: Expr {
                        kind: ExprKind::Lit(QValue::Bool(true)),
                        ty: QType::boolean(),
                    },
                },
            });
        }
        let mut rel = from.unwrap_or(Rel::Empty);
        if let Some(w) = &sel.where_ {
            let pred = self.expr(w)?;
            rel = Rel::Filter {
                input: Box::new(rel),
                pred,
            };
        }
        let mut items = Vec::with_capacity(sel.items.len());
        for i in &sel.items {
            let e = if i.nested.is_some() {
                self.nested_item(i, items.len())?
            } else if let Some(arg) = self.presentation_item(sel, i, items.len())? {
                arg
            } else {
                self.expr(&i.expr)?
            };
            items.push(e);
        }
        let mut having = match &sel.having {
            Some(h) => Some(self.expr(h)?),
            None => None,
        };
        let aggregated = sel.group.is_some() || sel.having.is_some() || !self.aggs.is_empty();
        let mut sort: Vec<(Expr, bool)> = Vec::new();
        let mut sort_meta: Vec<SortMeta> = Vec::new();
        let mut hidden: Vec<Expr> = Vec::new();
        let mut hier: Option<(u16, u16)> = None;
        let mut items_index: Option<HashMap<u64, Vec<usize>>> = None;
        if let Some(order) = order.filter(|o| !o.is_empty()) {
            let h_at = order
                .iter()
                .position(|k| matches!(k.dir, OrderDir::Hierarchy | OrderDir::HierarchyDesc));
            if h_at.is_some() && (order.len() != 1 || aggregated || sel.distinct) {
                return later("ИЕРАРХИЯ с несколькими ключами или группировкой", "Q3");
            }
            for k in order {
                let desc = matches!(k.dir, OrderDir::Desc | OrderDir::HierarchyDesc);
                let (base, col) = match k.col {
                    Some(c) => {
                        let Some(it) = sel.items.get(c) else {
                            return internal("an order key names a missing column");
                        };
                        if it.nested.is_some() {
                            return later("сортировка по вложенной таблице", "Q3");
                        }
                        if u16::try_from(c).is_ok_and(|c| self.present.contains(&c)) {
                            return later("сортировка по представлению", "Q3");
                        }
                        let Some(x) = items.get(c) else {
                            return internal("an order key names a missing column");
                        };
                        (x.clone(), Some(c))
                    }
                    None => {
                        let e = self.expr(&k.expr)?;
                        let index = items_index.get_or_insert_with(|| {
                            let mut m: HashMap<u64, Vec<usize>> = HashMap::new();
                            for (i, it) in items.iter().enumerate() {
                                let mut h = std::collections::hash_map::DefaultHasher::new();
                                shallow(it, &mut h);
                                m.entry(h.finish()).or_default().push(i);
                            }
                            m
                        });
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        shallow(&e, &mut h);
                        let same = index.get(&h.finish()).and_then(|c| {
                            c.iter()
                                .copied()
                                .find(|&i| items.get(i).map(|x| &x.kind) == Some(&e.kind))
                        });
                        (e, same)
                    }
                };
                if h_at.is_some() {
                    if autoorder::last_deref_tables(&base)
                        .into_iter()
                        .any(|t| self.role_field(t, StdRole::Parent).is_some())
                    {
                        return later("ИЕРАРХИЯ по полю через разыменование", "Q3");
                    }
                    match autoorder::first_src(&base).and_then(|src| self.hierarchy_of(src)) {
                        Some((id, parent)) => {
                            hier = Some((
                                push_hidden(&mut hidden, items.len(), id)?,
                                push_hidden(&mut hidden, items.len(), parent)?,
                            ));
                        }
                        None if base
                            .ty
                            .refs
                            .iter()
                            .any(|a| self.schema.hierarchical_table(*a).is_some()) =>
                        {
                            return later("ИЕРАРХИЯ по ссылке вне таблицы объекта", "Q3");
                        }
                        None => {}
                    }
                }
                if auto && !base.ty.refs.is_empty() {
                    for key in self.auto_keys(&base)? {
                        let ty = key.ty.clone();
                        let meta_key = key.clone();
                        let i = push_hidden(&mut hidden, items.len(), key)?;
                        sort.push((out(usize::from(i), ty)?, desc));
                        sort_meta.push(SortMeta {
                            col: usize::from(i),
                            desc,
                            base_col: col,
                            base: base.clone(),
                            key: meta_key,
                        });
                    }
                    continue;
                }
                let ty = base.ty.clone();
                let meta_base = base.clone();
                let c = match col {
                    Some(c) => c,
                    None => usize::from(push_hidden(&mut hidden, items.len(), base)?),
                };
                sort.push((out(c, ty)?, desc));
                sort_meta.push(SortMeta {
                    col: c,
                    desc,
                    base_col: col,
                    base: meta_base.clone(),
                    key: meta_base,
                });
            }
        } else if auto && !aggregated && !sel.distinct {
            let first = self.levels.last().and_then(|ids| ids.first()).copied();
            if let Some(main) = match first {
                Some(src) => self.main_order(src)?,
                None => None,
            } {
                if let Some((id, parent)) = main.hier {
                    hier = Some((
                        push_hidden(&mut hidden, items.len(), id)?,
                        push_hidden(&mut hidden, items.len(), parent)?,
                    ));
                }
                for key in main.keys {
                    for k in self.auto_keys(&key)? {
                        let ty = k.ty.clone();
                        let meta_key = k.clone();
                        let i = push_hidden(&mut hidden, items.len(), k)?;
                        sort.push((out(usize::from(i), ty)?, false));
                        sort_meta.push(SortMeta {
                            col: usize::from(i),
                            desc: false,
                            base_col: None,
                            base: key.clone(),
                            key: meta_key,
                        });
                    }
                }
            }
        } else if auto && aggregated {
            if let Some(sets) = &sel.group {
                if sets.len() > 1 {
                    return later(
                        "АВТОУПОРЯДОЧИВАНИЕ с несколькими наборами группировки",
                        "Q3",
                    );
                }
                for g in sets.iter().flatten() {
                    let key = self.expr(g)?;
                    let refs = !key.ty.refs.is_empty();
                    for k in self.auto_keys(&key)? {
                        let ty = k.ty.clone();
                        let meta_key = k.clone();
                        let i = push_hidden(&mut hidden, items.len(), k)?;
                        sort.push((out(usize::from(i), ty)?, false));
                        sort_meta.push(SortMeta {
                            col: usize::from(i),
                            desc: false,
                            base_col: None,
                            base: key.clone(),
                            key: meta_key,
                        });
                    }
                    if refs {
                        let ty = key.ty.clone();
                        let meta_key = key.clone();
                        let i = push_hidden(&mut hidden, items.len(), key)?;
                        sort.push((out(usize::from(i), ty)?, false));
                        sort_meta.push(SortMeta {
                            col: usize::from(i),
                            desc: false,
                            base_col: None,
                            base: meta_key.clone(),
                            key: meta_key,
                        });
                    }
                }
            }
        } else if auto {
            return later("АВТОУПОРЯДОЧИВАНИЕ с РАЗЛИЧНЫЕ", "Q3");
        }
        if self.query_depth == 1 && self.totals_open {
            let extra = std::mem::take(&mut self.totals_extra);
            let mut cols = Vec::with_capacity(extra.len());
            let mut built = Vec::with_capacity(extra.len());
            for e in &extra {
                let x = self.expr(e)?;
                built.push(x.clone());
                cols.push(push_hidden(&mut hidden, items.len(), x)?);
            }
            self.totals_extra = extra;
            self.totals_extra_cols = cols;
            self.totals_extra_built = built;
            self.root_sort = std::mem::take(&mut sort_meta);
        }
        if aggregated && sel.items.iter().any(|i| i.nested.is_some()) {
            return later("вложенная таблица в запросе с группировкой", "Q3");
        }
        if aggregated {
            let own: HashSet<SrcId> = self.levels.last().into_iter().flatten().copied().collect();
            let anchor = self.levels.last().and_then(|ids| ids.first()).copied();
            let mut keys: Vec<Expr> = Vec::new();
            let mut index: HashMap<u64, Vec<u16>> = HashMap::new();
            let mut sets: Vec<Vec<u16>> = Vec::new();
            if let Some(group) = &sel.group {
                for set in group {
                    let mut s = Vec::with_capacity(set.len());
                    for k in set {
                        let e = self.expr(k)?;
                        s.push(find_or_push(&mut keys, &mut index, e)?);
                    }
                    sets.push(s);
                }
            }
            let ki = KeyIndex {
                keys: &keys,
                index: &index,
                own: &own,
                anchor,
                in_sub: false,
            };
            items = items.into_iter().map(|e| ki.rekey(e)).collect();
            having = having.map(|e| ki.rekey(e));
            hidden = hidden.into_iter().map(|e| ki.rekey(e)).collect();
            if items
                .iter()
                .chain(having.iter())
                .chain(hidden.iter())
                .any(|e| correlated(e, &own))
            {
                return Err(BuildError::Later {
                    what: "коррелированный подзапрос над группировкой",
                    stage: "Q3",
                });
            }
            let mut aggs = std::mem::take(&mut self.aggs);
            for a in &mut aggs {
                if a.f != Func::GroupedBy {
                    continue;
                }
                let Some(i) = a.arg.as_ref().and_then(|x| ki.find(x)) else {
                    return later("СГРУППИРОВАНОПО по выражению вне группировки", "Q3");
                };
                a.arg = Some(Expr {
                    kind: ExprKind::Lit(QValue::Number(bsl_prim::Number::from_u64(u64::from(i)))),
                    ty: QType::num(crate::types::NumQ {
                        digits: 5,
                        scale: 0,
                        nonneg: true,
                    }),
                });
            }
            rel = Rel::Aggregate {
                input: Box::new(rel),
                sets: if sets.len() > 1 { Some(sets) } else { None },
                keys,
                aggs,
            };
            if let Some(h) = having {
                rel = Rel::Filter {
                    input: Box::new(rel),
                    pred: h,
                };
            }
        }
        items.extend(hidden);
        rel = Rel::Project {
            input: Box::new(rel),
            exprs: items,
        };
        if sel.distinct {
            rel = Rel::Distinct(Box::new(rel));
        }
        if !sort.is_empty() {
            let keys = sort
                .into_iter()
                .map(|(expr, desc)| SortKey { expr, desc })
                .collect();
            rel = match hier {
                Some((id, parent)) => Rel::Hierarchy {
                    input: Box::new(rel),
                    id,
                    parent,
                    keys,
                },
                None => Rel::Sort {
                    input: Box::new(rel),
                    keys,
                },
            };
        }
        if let Some(n) = sel.top {
            rel = Rel::Top {
                input: Box::new(rel),
                n,
            };
        }
        Ok(rel)
    }

    fn nested_item(&mut self, it: &'a tir::Item, col: usize) -> R<Expr> {
        let Ok(col) = u16::try_from(col) else {
            return internal("too many columns");
        };
        let nested = it.nested.as_deref().unwrap_or_default();
        if matches!(it.expr.kind, TKind::Const(_)) {
            self.nested.push(NestedS { col, rows: None });
            return Ok(Expr {
                kind: ExprKind::Lit(QValue::Null),
                ty: it.expr.ty.clone(),
            });
        }
        if it.unknown {
            return internal("a nested table of an unknown type in Execute");
        }
        let (owner_ref, tab) = self.owner_of(&it.expr)?;
        let Some(tab_src) = self.section_source(tab, nested.iter().any(|item| item.star))? else {
            return internal("a tabular section without a source");
        };
        let owner = self.section_col(tab_src, tab, StdRole::Ref)?;
        let number = self.section_col(tab_src, tab, StdRole::LineNumber)?;
        let outer: HashSet<SrcId> = self.levels.iter().flatten().copied().collect();
        let outer_aggs = std::mem::take(&mut self.aggs);
        let outer_index = std::mem::take(&mut self.agg_index);
        self.levels.push(vec![tab_src]);
        let mut exprs = Vec::with_capacity(nested.len());
        let mut res = Ok(());
        for n in nested {
            if n.nested.is_some() {
                res = later("вложенная таблица во вложенной", "Q3");
                break;
            }
            match self.expr(&n.expr) {
                Ok(e) => exprs.push(e),
                Err(e) => {
                    res = Err(e);
                    break;
                }
            }
        }
        self.levels.pop();
        let had_aggs = !self.aggs.is_empty();
        self.aggs = outer_aggs;
        self.agg_index = outer_index;
        res?;
        if had_aggs {
            return later("агрегат во вложенной таблице", "Q3");
        }
        let mut reads_owner = false;
        for e in &exprs {
            visit_expr(e, true, &mut |x| {
                if let ExprKind::Col { src, .. } = &x.kind
                    && outer.contains(src)
                {
                    reads_owner = true;
                }
            });
        }
        if reads_owner {
            return later("поле владельца во вложенной таблице", "Q3");
        }
        self.nested.push(NestedS {
            col,
            rows: Some(NestedRows {
                scan: Rel::Scan {
                    src: tab_src,
                    table: tab,
                },
                owner,
                number,
                exprs,
            }),
        });
        Ok(owner_ref)
    }

    fn section_agg(
        &mut self,
        f: Func,
        distinct: bool,
        arg: Option<&'a TExpr>,
        ty: QType,
    ) -> R<Expr> {
        let Some(arg) = arg else {
            return internal("an aggregate over a tabular section without an argument");
        };
        let mut found = None;
        let mut stack = vec![arg];
        while let Some(e) = stack.pop() {
            if let TKind::Field {
                level, source, col, ..
            } = &e.kind
            {
                found = Some((*level, *source, *col));
                break;
            }
            crate::sema::select::each_child(e, &mut |c| stack.push(c));
        }
        let Some((level, source, Col::Field(fld))) = found else {
            return internal("an aggregate over a tabular section without a field");
        };
        let Some(src) = self
            .levels
            .len()
            .checked_sub(1 + usize::from(level))
            .and_then(|i| self.levels.get(i))
            .and_then(|ids| ids.get(usize::from(source)))
            .copied()
        else {
            return internal("a tabular section of a source that is not visible");
        };
        let Some(SrcKind::Table(qt)) = self.sources.get(src.0 as usize).map(|s| s.kind.clone())
        else {
            return later("агрегат по табличной части не из таблицы", "Q3");
        };
        let Some(tab) = self.schema.field(qt, fld).and_then(|x| x.nested) else {
            return internal("an aggregate over a field that is not a tabular section");
        };
        let owner = self.section_col(src, qt, StdRole::Ref)?;
        let Some(tab_src) = self.section_source(tab, false)? else {
            return internal("a tabular section without a source");
        };
        let section_owner = self.section_col(tab_src, tab, StdRole::Ref)?;
        let prev = self.section.replace(SectionMap {
            owner_src: src,
            field: fld,
            src: tab_src,
        });
        let arg_e = self.expr(arg);
        self.section = prev;
        let arg_e = arg_e?;
        let mut stray = false;
        visit_expr(&arg_e, true, &mut |x| {
            if let ExprKind::Col { src: s, .. } = &x.kind
                && *s == src
            {
                stray = true;
            }
        });
        if stray {
            return later("агрегат по нескольким табличным частям", "Q3");
        }
        let key_ty = section_owner.ty.clone();
        let rel = Rel::Project {
            input: Box::new(Rel::Aggregate {
                input: Box::new(Rel::Scan {
                    src: tab_src,
                    table: tab,
                }),
                keys: vec![section_owner],
                sets: None,
                aggs: vec![AggCall {
                    f,
                    distinct,
                    arg: Some(arg_e),
                    ty: ty.clone(),
                }],
            }),
            exprs: vec![
                Expr {
                    kind: ExprKind::KeyRef(0),
                    ty: key_ty,
                },
                Expr {
                    kind: ExprKind::AggRef(0),
                    ty: ty.clone(),
                },
            ],
        };
        Ok(Expr {
            kind: ExprKind::SectionAgg {
                f,
                owner: Box::new(owner),
                rel: Box::new(rel),
            },
            ty,
        })
    }

    fn owner_of(&mut self, e: &'a TExpr) -> R<(Expr, QTableId)> {
        match &e.kind {
            TKind::Field {
                level,
                source,
                col,
                derefs,
            } => {
                let Some(src) = self
                    .levels
                    .len()
                    .checked_sub(1 + usize::from(*level))
                    .and_then(|i| self.levels.get(i))
                    .and_then(|ids| ids.get(usize::from(*source)))
                    .copied()
                else {
                    return internal("a nested table of a source that is not visible");
                };
                let sel = match col {
                    Col::Field(f) => ColSel::Field(*f),
                    Col::Column(i) => ColSel::Column(*i),
                };
                let base = Expr {
                    kind: ExprKind::Col { src, col: sel },
                    ty: self.column_type(src, sel),
                };
                match derefs.split_last() {
                    None => {
                        let (ColSel::Field(f), Some(SrcKind::Table(qt))) =
                            (sel, self.sources.get(src.0 as usize).map(|s| &s.kind))
                        else {
                            return later("вложенная таблица не из таблицы", "Q3");
                        };
                        let qt = *qt;
                        let Some(tab) = self.schema.field(qt, f).and_then(|x| x.nested) else {
                            return internal("a nested table of a field that is not a section");
                        };
                        let owner = self.section_col(src, qt, StdRole::Ref)?;
                        Ok((owner, tab))
                    }
                    Some((last, init)) => {
                        let tab = self.step_section(last)?;
                        self.touch_derefs(init);
                        Ok((deref_of(base, init), tab))
                    }
                }
            }
            TKind::Cast {
                e: x,
                to,
                derefs,
                num,
            } => {
                let Some((last, init)) = derefs.split_last() else {
                    return internal("a nested table of a cast without a dereference");
                };
                let tab = self.step_section(last)?;
                self.touch_derefs(init);
                let cast = Expr {
                    kind: ExprKind::Cast {
                        x: self.bx(x)?,
                        to: to.clone(),
                        num: *num,
                    },
                    ty: to.clone(),
                };
                Ok((deref_of(cast, init), tab))
            }
            _ => later("вложенная таблица из выражения", "Q3"),
        }
    }

    fn step_section(&self, d: &tir::Deref) -> R<QTableId> {
        let mut found: Vec<QTableId> = d
            .targets
            .iter()
            .filter_map(|(t, f)| self.schema.field(*t, *f)?.nested)
            .collect();
        found.sort_unstable();
        found.dedup();
        match found.as_slice() {
            [one] => Ok(*one),
            [] => internal("a nested table step without a section"),
            _ => later("вложенная таблица составного ссылочного типа", "Q3"),
        }
    }

    fn section_source(&mut self, tab: QTableId, all_fields: bool) -> R<Option<SrcId>> {
        let Ok(n) = u32::try_from(self.sources.len()) else {
            return internal("too many sources");
        };
        let id = SrcId(n);
        self.sources.push(SourceS {
            id,
            all_fields,
            kind: SrcKind::Table(tab),
        });
        self.touch(tab);
        Ok(Some(id))
    }

    fn section_col(&self, src: SrcId, qt: QTableId, role: StdRole) -> R<Expr> {
        let t = self.schema.table(qt);
        let Some(i) = t
            .fields
            .iter()
            .position(|f| f.role == FieldRole::Std(role))
            .and_then(|i| u16::try_from(i).ok())
        else {
            return internal("a table without a standard field of a nested table");
        };
        let f = QFieldId(i);
        Ok(Expr {
            kind: ExprKind::Col {
                src,
                col: ColSel::Field(f),
            },
            ty: t
                .fields
                .get(usize::from(i))
                .map(|x| x.ty.clone())
                .unwrap_or_default(),
        })
    }

    fn declare(&mut self, s: &'a Source, ids: &mut Vec<SrcId>) -> R<Declared<'a>> {
        let Ok(n) = u32::try_from(self.sources.len()) else {
            return internal("too many sources");
        };
        let id = SrcId(n);
        ids.push(id);
        self.sources.push(SourceS {
            id,
            all_fields: s.all_fields,
            kind: SrcKind::Subquery {
                columns: Vec::new(),
            },
        });
        let rel = match &s.from {
            From::Table {
                id: t,
                args,
                periodicity,
                complement,
                span,
                ..
            } if self.schema.supports_register_vt(*t) => {
                self.register_vt(id, *t, args, *periodicity, *complement, *span)?
            }
            From::Table { id: t, args, .. }
                if matches!(
                    self.schema.table(*t).kind,
                    TableKind::Virtual(VtKind::TaskByPerformer | VtKind::FilterCriterion)
                ) =>
            {
                self.qr8_source(id, *t, args)?
            }
            From::Table { id: t, .. } => {
                match self.schema.table(*t).kind {
                    TableKind::Virtual(v)
                        if !matches!(
                            v,
                            VtKind::SequenceBoundaries | VtKind::BusinessProcessPoints
                        ) =>
                    {
                        return Err(BuildError::Later {
                            what: "виртуальные таблицы",
                            stage: "Q5",
                        });
                    }
                    _ => {}
                }
                if let Some(x) = self.sources.last_mut() {
                    x.kind = SrcKind::Table(*t);
                }
                self.touch(*t);
                Rel::Scan { src: id, table: *t }
            }
            From::Temp(t) => {
                if let Some(x) = self.sources.get_mut(n as usize) {
                    x.kind = SrcKind::Temp {
                        columns: t
                            .columns
                            .iter()
                            .map(|c| c.ty.clone().unwrap_or_default())
                            .collect(),
                        stored: t.columns.iter().map(|c| c.stored).collect(),
                    };
                }
                Rel::TempScan {
                    src: id,
                    name: t.name.clone(),
                }
            }
            From::Param { name, schema } => {
                let param = self.param(name)?;
                if let Some(x) = self.sources.get_mut(n as usize) {
                    x.kind = SrcKind::Temp {
                        columns: schema
                            .columns
                            .iter()
                            .map(|c| c.ty.clone().unwrap_or_default())
                            .collect(),
                        stored: schema.columns.iter().map(|c| c.stored).collect(),
                    };
                }
                Rel::ParamScan { src: id, param }
            }
            From::Subquery(q) => {
                let (input, columns) = self.query(q)?;
                if let Some(x) = self.sources.get_mut(n as usize) {
                    x.kind = SrcKind::Subquery { columns };
                }
                Rel::Subquery {
                    src: id,
                    input: Box::new(input),
                }
            }
        };
        let mut joins = Vec::with_capacity(s.joins.len());
        for j in &s.joins {
            let right = self.declare(&j.right, ids)?;
            self.joins = self.joins.saturating_add(1);
            joins.push((j.kind, right, &j.on));
        }
        Ok(Declared { rel, joins })
    }

    fn assemble(&mut self, d: Declared<'a>) -> R<Rel> {
        let mut rel = d.rel;
        for (kind, right, on) in d.joins {
            let r = self.assemble(right)?;
            let on = self.expr(on)?;
            rel = Rel::Join {
                kind,
                l: Box::new(rel),
                r: Box::new(r),
                on,
            };
        }
        Ok(rel)
    }

    fn exprs(&mut self, v: &'a [TExpr]) -> R<Vec<Expr>> {
        let mut out = Vec::with_capacity(v.len());
        for e in v {
            out.push(self.expr(e)?);
        }
        Ok(out)
    }

    fn bx(&mut self, e: &'a TExpr) -> R<Box<Expr>> {
        Ok(Box::new(self.expr(e)?))
    }

    fn expr(&mut self, e: &'a TExpr) -> R<Expr> {
        if !matches!(e.kind, TKind::Neg(_) | TKind::Not(_) | TKind::Compare(..)) {
            return self.expr_node(e);
        }
        let mut spine = vec![e];
        let mut cur = e;
        while let TKind::Neg(x) | TKind::Not(x) | TKind::Compare(_, x, _) = &cur.kind {
            spine.push(x);
            cur = x;
        }
        let innermost = spine.pop().unwrap_or(e);
        let mut acc = self.expr_node(innermost)?;
        for node in spine.iter().rev() {
            let kind = match &node.kind {
                TKind::Neg(_) => ExprKind::Neg(Box::new(acc)),
                TKind::Not(_) => ExprKind::Not(Box::new(acc)),
                TKind::Compare(op, _, b) => ExprKind::Compare(*op, Box::new(acc), self.bx(b)?),
                _ => return internal("a spine node is neither a unary operator nor a comparison"),
            };
            acc = Expr {
                kind,
                ty: node.ty.clone(),
            };
        }
        Ok(acc)
    }

    fn expr_node(&mut self, e: &'a TExpr) -> R<Expr> {
        let ty = e.ty.clone();
        let kind = match &e.kind {
            TKind::Const(v) => ExprKind::Lit(v.clone()),
            TKind::Value(p) => {
                let Ok(slot) = u16::try_from(self.values.len()) else {
                    return internal("too many ЗНАЧЕНИЕ slots");
                };
                self.values.push(p.clone());
                ExprKind::Value(slot)
            }
            TKind::Param(n) => ExprKind::Param(self.param(n)?),
            TKind::Field {
                level,
                source,
                col,
                derefs,
            } => return self.field(&e.ty, *level, *source, *col, derefs),
            TKind::Neg(_) | TKind::Not(_) | TKind::Compare(..) => {
                return internal("a unary operator or a comparison outside the spine walk");
            }
            TKind::Arith { first, steps } => {
                let first = self.bx(first)?;
                let mut out = Vec::with_capacity(steps.len());
                for s in steps {
                    out.push((s.op, self.expr(&s.rhs)?, s.ty.clone()));
                }
                ExprKind::Arith { first, steps: out }
            }
            TKind::And(v) => ExprKind::And(self.exprs(v)?),
            TKind::Or(v) => ExprKind::Or(self.exprs(v)?),
            TKind::InList {
                l,
                not,
                hierarchy,
                rhs,
            } => {
                if *hierarchy {
                    self.touch_hierarchy(l);
                }
                ExprKind::InList {
                    l: self.exprs(l)?,
                    not: *not,
                    hierarchy: *hierarchy,
                    rhs: self.exprs(rhs)?,
                }
            }
            TKind::InQuery {
                l,
                not,
                hierarchy,
                q,
            } => {
                if *hierarchy {
                    self.touch_hierarchy(l);
                }
                let l = self.exprs(l)?;
                let (rel, _) = self.query(q)?;
                ExprKind::InQuery {
                    l,
                    not: *not,
                    hierarchy: *hierarchy,
                    q: Box::new(rel),
                }
            }
            TKind::Between { l, not, lo, hi } => ExprKind::Between {
                l: self.bx(l)?,
                not: *not,
                lo: self.bx(lo)?,
                hi: self.bx(hi)?,
            },
            TKind::IsNull { l, not } => ExprKind::IsNull {
                l: self.bx(l)?,
                not: *not,
            },
            TKind::Refs { l, table } => ExprKind::Refs {
                l: self.bx(l)?,
                table: *table,
            },
            TKind::Like {
                l,
                not,
                pattern,
                escape,
            } => ExprKind::Like {
                l: self.bx(l)?,
                not: *not,
                pat: self.bx(pattern)?,
                esc: *escape,
            },
            TKind::Case {
                operand,
                whens,
                else_,
            } => {
                let operand = match operand {
                    Some(o) => Some(self.bx(o)?),
                    None => None,
                };
                let mut out = Vec::with_capacity(whens.len());
                for (c, r) in whens {
                    out.push((self.expr(c)?, self.expr(r)?));
                }
                let else_ = match else_ {
                    Some(o) => Some(self.bx(o)?),
                    None => None,
                };
                ExprKind::Case {
                    operand,
                    whens: out,
                    else_,
                }
            }
            TKind::Cast {
                e: x,
                to,
                derefs,
                num,
            } => {
                let cast_ty = if derefs.is_empty() {
                    ty.clone()
                } else {
                    to.clone()
                };
                let cast = Expr {
                    kind: ExprKind::Cast {
                        x: self.bx(x)?,
                        to: to.clone(),
                        num: *num,
                    },
                    ty: cast_ty,
                };
                if derefs.is_empty() {
                    return Ok(cast);
                }
                self.touch_derefs(derefs);
                ExprKind::Deref {
                    base: Box::new(cast),
                    steps: derefs.clone(),
                }
            }
            TKind::Func { f, args } => {
                if matches!(f, Func::Presentation | Func::RefPresentation) {
                    return Err(BuildError::Later {
                        what: "ПРЕДСТАВЛЕНИЕ",
                        stage: "Q3",
                    });
                }
                match f {
                    Func::GroupedBy => {
                        let arg = match args.as_slice() {
                            [a] => self.expr(a)?,
                            _ => return internal("СГРУППИРОВАНОПО without exactly one argument"),
                        };
                        let call = AggCall {
                            f: *f,
                            distinct: false,
                            arg: Some(arg),
                            ty: ty.clone(),
                        };
                        return Ok(Expr {
                            kind: ExprKind::AggRef(self.agg(call)?),
                            ty,
                        });
                    }
                    Func::RecordAutoNumber => {
                        return Ok(Expr {
                            kind: ExprKind::Lit(QValue::Null),
                            ty,
                        });
                    }
                    _ => {}
                }
                ExprKind::Func {
                    f: *f,
                    args: self.exprs(args)?,
                }
            }
            TKind::Unit(k) => ExprKind::Unit(*k),
            TKind::Agg {
                f,
                distinct,
                arg,
                nested: true,
            } => return self.section_agg(*f, *distinct, arg.as_deref(), ty),
            TKind::Agg {
                f, distinct, arg, ..
            } => {
                let arg = match arg {
                    Some(a) => Some(self.expr(a)?),
                    None => None,
                };
                let call = AggCall {
                    f: *f,
                    distinct: *distinct,
                    arg,
                    ty: ty.clone(),
                };
                ExprKind::AggRef(self.agg(call)?)
            }
        };
        Ok(Expr { kind, ty })
    }

    fn touch_derefs(&mut self, derefs: &[tir::Deref]) {
        for d in derefs {
            for (t, _) in &d.targets {
                self.touch(*t);
            }
        }
    }

    fn field(
        &mut self,
        e_ty: &QType,
        level: u16,
        source: u16,
        col: Col,
        derefs: &[tir::Deref],
    ) -> R<Expr> {
        let Some(ids) = self
            .levels
            .len()
            .checked_sub(1 + usize::from(level))
            .and_then(|i| self.levels.get(i))
        else {
            return internal("a field of a level that is not visible");
        };
        let Some(src) = ids.get(usize::from(source)).copied() else {
            return internal("a field of a missing source");
        };
        let sel = match col {
            Col::Field(f) => ColSel::Field(f),
            Col::Column(i) => ColSel::Column(i),
        };
        if let (Some(m), ColSel::Field(f), Some(first)) = (self.section, sel, derefs.first())
            && src == m.owner_src
            && f == m.field
        {
            let Some((_, sf)) = first.targets.first() else {
                return internal("a tabular section field without a target");
            };
            let rest = derefs.get(1..).unwrap_or_default();
            let base = Expr {
                kind: ExprKind::Col {
                    src: m.src,
                    col: ColSel::Field(*sf),
                },
                ty: if rest.is_empty() {
                    e_ty.clone()
                } else {
                    first
                        .targets
                        .first()
                        .and_then(|(t, _)| self.schema.field(*t, *sf))
                        .map(|x| x.ty.clone())
                        .unwrap_or_default()
                },
            };
            if rest.is_empty() {
                return Ok(base);
            }
            self.touch_derefs(rest);
            return Ok(deref_of(base, rest));
        }
        let base_ty = if derefs.is_empty() {
            e_ty.clone()
        } else {
            self.column_type(src, sel)
        };
        let base = Expr {
            kind: ExprKind::Col { src, col: sel },
            ty: base_ty,
        };
        if derefs.is_empty() {
            return Ok(base);
        }
        self.touch_derefs(derefs);
        Ok(Expr {
            kind: ExprKind::Deref {
                base: Box::new(base),
                steps: derefs.to_vec(),
            },
            ty: e_ty.clone(),
        })
    }

    fn presentation_item(
        &mut self,
        sel: &'a Select,
        it: &'a tir::Item,
        col: usize,
    ) -> R<Option<Expr>> {
        let arg = match &it.expr.kind {
            TKind::Func {
                f: Func::Presentation | Func::RefPresentation,
                args,
            } => match args.as_slice() {
                [a] => Some(a),
                _ => return internal("ПРЕДСТАВЛЕНИЕ takes one argument"),
            },
            _ => None,
        };
        let field = match &it.expr.kind {
            TKind::Field {
                level,
                source,
                col: c,
                derefs,
            } if arg.is_none() => self.presentation_of_field(*level, *source, *c, derefs)?,
            _ => None,
        };
        if arg.is_none() && field.is_none() {
            return Ok(None);
        }
        if self.query_depth != 1 || !self.single_part {
            return later("ПРЕДСТАВЛЕНИЕ в подзапросе или в объединении", "Q3");
        }
        if sel.distinct {
            return later("ПРЕДСТАВЛЕНИЕ при РАЗЛИЧНЫЕ", "Q3");
        }
        let Ok(c) = u16::try_from(col) else {
            return internal("too many columns");
        };
        let e = match (arg, field) {
            (Some(a), _) => self.expr(a)?,
            (None, Some(f)) => f,
            (None, None) => return Ok(None),
        };
        self.present.push(c);
        Ok(Some(e))
    }

    fn presentation_of_field(
        &mut self,
        level: u16,
        source: u16,
        col: Col,
        derefs: &[tir::Deref],
    ) -> R<Option<Expr>> {
        let Some(ids) = self
            .levels
            .len()
            .checked_sub(1 + usize::from(level))
            .and_then(|i| self.levels.get(i))
        else {
            return Ok(None);
        };
        let Some(src) = ids.get(usize::from(source)).copied() else {
            return Ok(None);
        };
        if let Some((last, head)) = derefs.split_last() {
            let all = !last.targets.is_empty()
                && last
                    .targets
                    .iter()
                    .all(|(t, f)| self.schema.is_presentation_field(*t, *f));
            if !all {
                return Ok(None);
            }
            let mut ty = QType::default();
            for (t, _) in &last.targets {
                if let Some(r) = self.schema.table(*t).reference {
                    ty = ty.merge(&QType::reference(r));
                }
            }
            return Ok(Some(self.field(&ty, level, source, col, head)?));
        }
        let (
            Some(SourceS {
                kind: SrcKind::Table(qt),
                ..
            }),
            Col::Field(f),
        ) = (self.sources.get(src.0 as usize), col)
        else {
            return Ok(None);
        };
        let qt = *qt;
        if !self.schema.is_presentation_field(qt, f) {
            return Ok(None);
        }
        let Some(r) = self
            .schema
            .table(qt)
            .fields
            .iter()
            .position(|x| x.role == FieldRole::Std(StdRole::Ref))
            .and_then(|i| u16::try_from(i).ok())
            .map(QFieldId)
        else {
            return Ok(None);
        };
        let ty = self
            .schema
            .field(qt, r)
            .map(|x| x.ty.clone())
            .unwrap_or_default();
        Ok(Some(Expr {
            kind: ExprKind::Col {
                src,
                col: ColSel::Field(r),
            },
            ty,
        }))
    }

    fn register_vt(
        &mut self,
        src: SrcId,
        t: QTableId,
        args: &'a [Option<TExpr>],
        periodicity: crate::schema::Periodicity,
        complement: Option<crate::schema::Complement>,
        span: Option<query_syntax::Span>,
    ) -> R<Rel> {
        let table = self.schema.table(t);
        let Some(register) = table.owner else {
            return internal("a virtual table without its register");
        };
        let TableKind::Virtual(kind) = table.kind else {
            return internal("not a virtual table");
        };
        let slice = matches!(kind, VtKind::SliceLast | VtKind::SliceFirst);
        let shape = crate::lower::vt_shape(self.schema, t);
        let all: Vec<Option<QFieldId>> = (0..table.fields.len())
            .map(|i| u16::try_from(i).ok().map(QFieldId))
            .collect();
        if let Some(x) = self.sources.get_mut(src.0 as usize) {
            x.kind = SrcKind::Virtual {
                table: t,
                cols: all,
            };
        }
        let mut inner_cols: Vec<Option<QFieldId>> =
            vec![None; crate::ir::raw_layout::width(shape.dims.len(), shape.resources.len())];
        for (i, f) in shape.dims.iter().enumerate() {
            if let Some(c) = inner_cols.get_mut(crate::ir::raw_layout::DIMS + i) {
                *c = Some(*f);
            }
        }
        if slice {
            inner_cols = (0..table.fields.len())
                .map(|i| u16::try_from(i).ok().map(QFieldId))
                .collect();
        }
        let Ok(n) = u32::try_from(self.sources.len()) else {
            return internal("too many sources");
        };
        let inner = SrcId(n);
        self.sources.push(SourceS {
            id: inner,
            all_fields: false,
            kind: SrcKind::Virtual {
                table: t,
                cols: inner_cols,
            },
        });
        let mut bounds: [Option<Expr>; 2] = [None, None];
        let mut next = 0usize;
        let mut cond = None;
        for (p, a) in table.params.iter().zip(args) {
            match p.kind {
                VtParamKind::Period => {
                    if let Some(slot) = bounds.get_mut(next) {
                        *slot = match a {
                            Some(e) => Some(self.expr(e)?),
                            None => None,
                        };
                    }
                    next += 1;
                }
                VtParamKind::Condition => {
                    if let Some(e) = a {
                        self.levels.push(vec![inner]);
                        let c = self.expr(e);
                        self.levels.pop();
                        cond = Some(c?);
                    }
                }
                _ => {}
            }
        }
        self.touch(register);
        let v = Box::new(crate::ir::RegisterVtS {
            src,
            inner,
            table: t,
            register,
            kind,
            bounds,
            cond,
            periodicity,
            complement,
            area: None,
            access: None,
            span,
        });
        Ok(if slice {
            Rel::Slice(v)
        } else if kind == crate::schema::VtKind::BalanceAndTurnovers
            && periodicity != crate::schema::Periodicity::Absent
        {
            Rel::RunningBalance(v)
        } else {
            Rel::RegisterVt(v)
        })
    }

    fn column_type(&self, src: SrcId, col: ColSel) -> QType {
        let Some(s) = self.sources.get(src.0 as usize) else {
            return QType::default();
        };
        match (&s.kind, col) {
            (SrcKind::Virtual { table, .. }, ColSel::Field(f)) => self
                .schema
                .field(*table, f)
                .map(|x| x.ty.clone())
                .unwrap_or_default(),
            (SrcKind::Table(t), ColSel::Field(f)) => self
                .schema
                .field(*t, f)
                .map(|x| x.ty.clone())
                .unwrap_or_default(),
            (SrcKind::Subquery { columns } | SrcKind::Temp { columns, .. }, ColSel::Column(i)) => {
                columns.get(usize::from(i)).cloned().unwrap_or_default()
            }
            _ => QType::default(),
        }
    }

    fn agg(&mut self, call: AggCall) -> R<u16> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::mem::discriminant(&call.f).hash(&mut h);
        call.distinct.hash(&mut h);
        if let Some(a) = &call.arg {
            shallow(a, &mut h);
        }
        let key = h.finish();
        if let Some(c) = self.agg_index.get(&key) {
            for &i in c {
                if self.aggs.get(usize::from(i)) == Some(&call) {
                    return Ok(i);
                }
            }
        }
        let Ok(i) = u16::try_from(self.aggs.len()) else {
            return internal("too many aggregates");
        };
        self.aggs.push(call);
        self.agg_index.entry(key).or_default().push(i);
        Ok(i)
    }
}

fn later<T>(what: &'static str, stage: &'static str) -> R<T> {
    Err(BuildError::Later { what, stage })
}

fn deref_of(base: Expr, steps: &[tir::Deref]) -> Expr {
    if steps.is_empty() {
        return base;
    }
    Expr {
        kind: ExprKind::Deref {
            base: Box::new(base),
            steps: steps.to_vec(),
        },
        ty: QType::default(),
    }
}

fn push_hidden(hidden: &mut Vec<Expr>, items: usize, e: Expr) -> R<u16> {
    hidden.push(e);
    match u16::try_from(items + hidden.len() - 1) {
        Ok(i) => Ok(i),
        Err(_) => internal("too many columns"),
    }
}

fn order_desc(d: OrderDir) -> R<bool> {
    match d {
        OrderDir::Asc => Ok(false),
        OrderDir::Desc => Ok(true),
        OrderDir::Hierarchy | OrderDir::HierarchyDesc => Err(BuildError::Later {
            what: "ИЕРАРХИЯ в объединении",
            stage: "Q3",
        }),
    }
}

fn out(col: usize, ty: QType) -> R<Expr> {
    let Ok(i) = u16::try_from(col) else {
        return internal("too many columns");
    };
    Ok(Expr {
        kind: ExprKind::Out(i),
        ty,
    })
}

fn shallow(e: &Expr, h: &mut impl Hasher) {
    shallow_d(e, h, SHALLOW_DEPTH);
}

const SHALLOW_DEPTH: u8 = 3;

fn shallow_d(e: &Expr, h: &mut impl Hasher, d: u8) {
    std::mem::discriminant(&e.kind).hash(h);
    let kids = |v: &[&Expr], h: &mut _| {
        v.len().hash(h);
        if d > 0 {
            for x in v {
                shallow_d(x, h, d - 1);
            }
        }
    };
    match &e.kind {
        ExprKind::Col { src, col } => {
            src.hash(h);
            col.hash(h);
        }
        ExprKind::Deref { base, steps } => {
            shallow_d(base, h, d);
            for s in steps {
                s.name.hash(h);
            }
        }
        ExprKind::Lit(v) => format!("{v:?}").hash(h),
        ExprKind::Param(i) | ExprKind::Value(i) | ExprKind::Out(i) => i.hash(h),
        ExprKind::KeyRef(i) | ExprKind::AggRef(i) => i.hash(h),
        ExprKind::OuterKey { src, key, .. } => {
            src.hash(h);
            key.hash(h);
        }
        ExprKind::Neg(x) | ExprKind::Not(x) => kids(&[x], h),
        ExprKind::Arith { first, steps } => {
            let mut v: Vec<&Expr> = vec![first];
            for (op, x, _) in steps {
                std::mem::discriminant(op).hash(h);
                v.push(x);
            }
            kids(&v, h);
        }
        ExprKind::And(v) | ExprKind::Or(v) => kids(&v.iter().collect::<Vec<_>>(), h),
        ExprKind::Compare(op, a, b) => {
            std::mem::discriminant(op).hash(h);
            kids(&[a, b], h);
        }
        ExprKind::InList {
            l,
            not,
            hierarchy,
            rhs,
        } => {
            not.hash(h);
            hierarchy.hash(h);
            kids(&l.iter().chain(rhs).collect::<Vec<_>>(), h);
        }
        ExprKind::InQuery {
            l, not, hierarchy, ..
        } => {
            not.hash(h);
            hierarchy.hash(h);
            kids(&l.iter().collect::<Vec<_>>(), h);
        }
        ExprKind::SectionAgg { f, owner, .. } => {
            f.hash(h);
            kids(&[owner], h);
        }
        ExprKind::Between { l, not, lo, hi } => {
            not.hash(h);
            kids(&[l, lo, hi], h);
        }
        ExprKind::IsNull { l, not } => {
            not.hash(h);
            kids(&[l], h);
        }
        ExprKind::Refs { l, table } => {
            table.hash(h);
            kids(&[l], h);
        }
        ExprKind::Like { l, not, pat, esc } => {
            not.hash(h);
            esc.hash(h);
            kids(&[l, pat], h);
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            let mut v: Vec<&Expr> = operand.iter().map(|x| &**x).collect();
            for (c, r) in whens {
                v.push(c);
                v.push(r);
            }
            v.extend(else_.iter().map(|x| &**x));
            kids(&v, h);
        }
        ExprKind::Cast { x, to, num } => {
            to.hash(h);
            num.hash(h);
            kids(&[x], h);
        }
        ExprKind::Func { f, args } => {
            std::mem::discriminant(f).hash(h);
            kids(&args.iter().collect::<Vec<_>>(), h);
        }
        ExprKind::Unit(k) => std::mem::discriminant(k).hash(h),
    }
}

pub(crate) fn visit_expr<'e>(e: &'e Expr, rels: bool, f: &mut dyn FnMut(&'e Expr)) {
    f(e);
    let v = |x: &'e Expr, f: &mut dyn FnMut(&'e Expr)| visit_expr(x, rels, f);
    match &e.kind {
        ExprKind::Deref { base, .. } => v(base, f),
        ExprKind::Neg(x) | ExprKind::Not(x) => v(x, f),
        ExprKind::Arith { first, steps } => {
            v(first, f);
            for (_, x, _) in steps {
                v(x, f);
            }
        }
        ExprKind::And(l) | ExprKind::Or(l) => l.iter().for_each(|x| v(x, f)),
        ExprKind::Compare(_, a, b) => {
            v(a, f);
            v(b, f);
        }
        ExprKind::InList { l, rhs, .. } => l.iter().chain(rhs).for_each(|x| v(x, f)),
        ExprKind::InQuery { l, q, .. } => {
            l.iter().for_each(|x| v(x, f));
            if rels {
                visit_rel(q, f);
            }
        }
        ExprKind::SectionAgg { owner, rel, .. } => {
            v(owner, f);
            if rels {
                visit_rel(rel, f);
            }
        }
        ExprKind::Between { l, lo, hi, .. } => {
            v(l, f);
            v(lo, f);
            v(hi, f);
        }
        ExprKind::IsNull { l, .. } | ExprKind::Refs { l, .. } => v(l, f),
        ExprKind::Like { l, pat, .. } => {
            v(l, f);
            v(pat, f);
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            operand.iter().for_each(|x| v(x, f));
            for (c, r) in whens {
                v(c, f);
                v(r, f);
            }
            else_.iter().for_each(|x| v(x, f));
        }
        ExprKind::Cast { x, .. } => v(x, f),
        ExprKind::Func { args, .. } => args.iter().for_each(|x| v(x, f)),
        _ => {}
    }
}

pub(crate) fn visit_rel<'e>(r: &'e Rel, f: &mut dyn FnMut(&'e Expr)) {
    match r {
        Rel::Empty | Rel::Scan { .. } | Rel::TempScan { .. } | Rel::ParamScan { .. } => {}
        Rel::Subquery { input, .. } | Rel::Distinct(input) | Rel::Top { input, .. } => {
            visit_rel(input, f)
        }
        Rel::Join { l, r, on, .. } => {
            visit_rel(l, f);
            visit_rel(r, f);
            visit_expr(on, true, f);
        }
        Rel::Filter { input, pred } => {
            visit_rel(input, f);
            visit_expr(pred, true, f);
        }
        Rel::Aggregate {
            input, keys, aggs, ..
        } => {
            visit_rel(input, f);
            keys.iter().for_each(|k| visit_expr(k, true, f));
            aggs.iter()
                .filter_map(|a| a.arg.as_ref())
                .for_each(|a| visit_expr(a, true, f));
        }
        Rel::Project { input, exprs } => {
            visit_rel(input, f);
            exprs.iter().for_each(|k| visit_expr(k, true, f));
        }
        Rel::Sort { input, keys } | Rel::Hierarchy { input, keys, .. } => {
            visit_rel(input, f);
            keys.iter().for_each(|k| visit_expr(&k.expr, true, f));
        }
        Rel::Union { parts, .. } => parts.iter().for_each(|p| visit_rel(p, f)),
        Rel::RegisterVt(v) | Rel::Slice(v) | Rel::RunningBalance(v) => {
            v.bounds
                .iter()
                .flatten()
                .for_each(|b| visit_expr(b, true, f));
            v.cond
                .iter()
                .chain(v.area.iter().map(|a| &a.pred))
                .chain(v.access.iter())
                .for_each(|c| visit_expr(c, true, f));
        }
    }
}

fn correlated(e: &Expr, own: &HashSet<SrcId>) -> bool {
    let mut found = false;
    visit_expr(e, false, &mut |n| {
        if let ExprKind::InQuery { q, .. } = &n.kind {
            visit_rel(q, &mut |m| {
                if let ExprKind::Col { src, .. } = &m.kind
                    && own.contains(src)
                {
                    found = true;
                }
            });
        }
    });
    found
}

fn find_or_push(keys: &mut Vec<Expr>, index: &mut HashMap<u64, Vec<u16>>, e: Expr) -> R<u16> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    shallow(&e, &mut h);
    let key = h.finish();
    crate::probe::hit();
    if let Some(c) = index.get(&key) {
        for &i in c {
            crate::probe::hit();
            if keys.get(usize::from(i)).map(|k| &k.kind) == Some(&e.kind) {
                return Ok(i);
            }
        }
    }
    let Ok(i) = u16::try_from(keys.len()) else {
        return internal("too many grouping keys");
    };
    keys.push(e);
    index.entry(key).or_default().push(i);
    Ok(i)
}

#[derive(Clone, Copy)]
struct KeyIndex<'k> {
    keys: &'k [Expr],
    index: &'k HashMap<u64, Vec<u16>>,
    own: &'k HashSet<SrcId>,
    anchor: Option<SrcId>,
    in_sub: bool,
}

fn reads_own(e: &Expr, own: &HashSet<SrcId>) -> bool {
    let mut found = false;
    visit_expr(e, false, &mut |n| {
        if let ExprKind::Col { src, .. } = &n.kind
            && own.contains(src)
        {
            found = true;
        }
    });
    found
}

impl KeyIndex<'_> {
    fn find(&self, e: &Expr) -> Option<u16> {
        if self.keys.is_empty() {
            return None;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        shallow(e, &mut h);
        self.index
            .get(&h.finish())?
            .iter()
            .copied()
            .find(|&i| self.keys.get(usize::from(i)).map(|k| &k.kind) == Some(&e.kind))
    }

    fn rel(&self, r: Rel) -> Rel {
        let bx = |x: Box<Rel>| Box::new(self.rel(*x));
        match r {
            Rel::Empty | Rel::Scan { .. } | Rel::TempScan { .. } | Rel::ParamScan { .. } => r,
            Rel::Slice(mut v) => {
                let [a, b] = std::mem::take(&mut v.bounds);
                v.bounds = [a.map(|e| self.rekey(e)), b.map(|e| self.rekey(e))];
                v.cond = v.cond.take().map(|e| self.rekey(e));
                v.area = v.area.take().map(|a| super::AreaFilter {
                    pred: self.rekey(a.pred),
                });
                v.access = v.access.take().map(|e| self.rekey(e));
                Rel::Slice(v)
            }
            Rel::RegisterVt(mut v) => {
                let [a, b] = std::mem::take(&mut v.bounds);
                v.bounds = [a.map(|e| self.rekey(e)), b.map(|e| self.rekey(e))];
                v.cond = v.cond.take().map(|e| self.rekey(e));
                v.area = v.area.take().map(|a| super::AreaFilter {
                    pred: self.rekey(a.pred),
                });
                v.access = v.access.take().map(|e| self.rekey(e));
                Rel::RegisterVt(v)
            }
            Rel::RunningBalance(mut v) => {
                let [a, b] = std::mem::take(&mut v.bounds);
                v.bounds = [a.map(|e| self.rekey(e)), b.map(|e| self.rekey(e))];
                v.cond = v.cond.take().map(|e| self.rekey(e));
                Rel::RunningBalance(v)
            }
            Rel::Subquery { src, input } => Rel::Subquery {
                src,
                input: bx(input),
            },
            Rel::Join { kind, l, r, on } => Rel::Join {
                kind,
                l: bx(l),
                r: bx(r),
                on: self.rekey(on),
            },
            Rel::Filter { input, pred } => Rel::Filter {
                input: bx(input),
                pred: self.rekey(pred),
            },
            Rel::Aggregate {
                input,
                keys,
                sets,
                aggs,
            } => Rel::Aggregate {
                input: bx(input),
                keys: self.vx(keys),
                sets,
                aggs: aggs
                    .into_iter()
                    .map(|a| AggCall {
                        arg: a.arg.map(|x| self.rekey(x)),
                        ..a
                    })
                    .collect(),
            },
            Rel::Project { input, exprs } => Rel::Project {
                input: bx(input),
                exprs: self.vx(exprs),
            },
            Rel::Distinct(x) => Rel::Distinct(bx(x)),
            Rel::Sort { input, keys } => Rel::Sort {
                input: bx(input),
                keys: self.sort_keys(keys),
            },
            Rel::Top { input, n } => Rel::Top {
                input: bx(input),
                n,
            },
            Rel::Hierarchy {
                input,
                id,
                parent,
                keys,
            } => Rel::Hierarchy {
                input: bx(input),
                id,
                parent,
                keys: self.sort_keys(keys),
            },
            Rel::Union { parts, all } => Rel::Union {
                parts: parts.into_iter().map(|p| self.rel(p)).collect(),
                all,
            },
        }
    }

    fn sort_keys(&self, keys: Vec<SortKey>) -> Vec<SortKey> {
        keys.into_iter()
            .map(|k| SortKey {
                expr: self.rekey(k.expr),
                desc: k.desc,
            })
            .collect()
    }

    #[allow(clippy::boxed_local)]
    fn bx(&self, b: Box<Expr>) -> Box<Expr> {
        Box::new(self.rekey(*b))
    }

    fn vx(&self, v: Vec<Expr>) -> Vec<Expr> {
        v.into_iter().map(|e| self.rekey(e)).collect()
    }

    fn rekey(&self, e: Expr) -> Expr {
        if let Some(i) = self.find(&e) {
            if !self.in_sub {
                return Expr {
                    kind: ExprKind::KeyRef(i),
                    ty: e.ty,
                };
            }
            if let (Some(src), true) = (self.anchor, reads_own(&e, self.own)) {
                let ty = e.ty.clone();
                return Expr {
                    kind: ExprKind::OuterKey {
                        src,
                        key: i,
                        orig: Box::new(e),
                    },
                    ty,
                };
            }
        }
        let Expr { kind, ty } = e;
        let kind = match kind {
            ExprKind::Deref { base, steps } => ExprKind::Deref {
                base: self.bx(base),
                steps,
            },
            ExprKind::Neg(x) => ExprKind::Neg(self.bx(x)),
            ExprKind::Not(x) => ExprKind::Not(self.bx(x)),
            ExprKind::Arith { first, steps } => ExprKind::Arith {
                first: self.bx(first),
                steps: steps
                    .into_iter()
                    .map(|(op, e, t)| (op, self.rekey(e), t))
                    .collect(),
            },
            ExprKind::And(v) => ExprKind::And(self.vx(v)),
            ExprKind::Or(v) => ExprKind::Or(self.vx(v)),
            ExprKind::Compare(op, a, b) => ExprKind::Compare(op, self.bx(a), self.bx(b)),
            ExprKind::InList {
                l,
                not,
                hierarchy,
                rhs,
            } => ExprKind::InList {
                l: self.vx(l),
                not,
                hierarchy,
                rhs: self.vx(rhs),
            },
            ExprKind::InQuery {
                l,
                not,
                hierarchy,
                q,
            } => ExprKind::InQuery {
                l: self.vx(l),
                not,
                hierarchy,
                q: Box::new(
                    KeyIndex {
                        in_sub: true,
                        ..*self
                    }
                    .rel(*q),
                ),
            },
            ExprKind::SectionAgg { f, owner, rel } => ExprKind::SectionAgg {
                f,
                owner: self.bx(owner),
                rel,
            },
            ExprKind::Between { l, not, lo, hi } => ExprKind::Between {
                l: self.bx(l),
                not,
                lo: self.bx(lo),
                hi: self.bx(hi),
            },
            ExprKind::IsNull { l, not } => ExprKind::IsNull { l: self.bx(l), not },
            ExprKind::Refs { l, table } => ExprKind::Refs {
                l: self.bx(l),
                table,
            },
            ExprKind::Like { l, not, pat, esc } => ExprKind::Like {
                l: self.bx(l),
                not,
                pat: self.bx(pat),
                esc,
            },
            ExprKind::Case {
                operand,
                whens,
                else_,
            } => ExprKind::Case {
                operand: operand.map(|o| self.bx(o)),
                whens: whens
                    .into_iter()
                    .map(|(c, r)| (self.rekey(c), self.rekey(r)))
                    .collect(),
                else_: else_.map(|o| self.bx(o)),
            },
            ExprKind::Cast { x, to, num } => ExprKind::Cast {
                x: self.bx(x),
                to,
                num,
            },
            ExprKind::Func { f, args } => ExprKind::Func {
                f,
                args: self.vx(args),
            },
            k => k,
        };
        Expr { kind, ty }
    }
}
