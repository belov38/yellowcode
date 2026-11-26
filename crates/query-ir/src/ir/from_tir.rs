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
mod autoorder_union;
mod hierarchy_union;
mod present_union;
mod qr8;
mod totals;

use autoorder_union::UnionAutoorder;
pub(crate) use autoorder_union::uniform_union_autoorder;
pub(crate) use hierarchy_union::{hierarchy_union_shape, union_autoorder_col_orderable};
pub(crate) use present_union::{UnionPresent, is_pure_ref_type, union_presentation};

type R<T> = Result<T, BuildError>;

fn internal<T>(s: &str) -> R<T> {
    Err(BuildError::Internal(s.to_string()))
}

pub(crate) fn distinct_movement_presentation(
    schema: &QuerySchema,
    select: &Select,
    item: &tir::Item,
    source_table: impl FnOnce(u16) -> Option<QTableId>,
) -> bool {
    if select.group.is_some() || select.having.is_some() {
        return false;
    }
    let TKind::Func {
        f: Func::Presentation,
        args,
    } = &item.expr.kind
    else {
        return false;
    };
    let [arg] = args.as_slice() else {
        return false;
    };
    let TKind::Field {
        level: 0,
        source,
        col: Col::Field(field),
        derefs,
    } = &arg.kind
    else {
        return false;
    };
    if !derefs.is_empty() || arg.ty.atom_count() != 1 {
        return false;
    }
    let [crate::Other::SysEnum(kind)] = arg.ty.others.as_slice() else {
        return false;
    };
    let Some(table) = source_table(*source) else {
        return false;
    };
    if schema.table(table).kind != TableKind::Object(mdschema::MdKind::AccumulationRegister) {
        return false;
    }
    let Some(field_info) = schema.field(table, *field) else {
        return false;
    };
    if field_info.role != FieldRole::Std(StdRole::RecordKind)
        || field_info.ty.atom_count() != 1
        || field_info.ty.others.as_slice() != [crate::Other::SysEnum(*kind)]
    {
        return false;
    }
    select.items.iter().any(|raw| {
        raw.nested.is_none()
            && matches!(
                &raw.expr.kind,
                TKind::Field { level: 0, source: raw_source, col: Col::Field(raw_field), derefs }
                    if raw_source == source && raw_field == field && derefs.is_empty()
            )
    })
}

fn same_tir_expr(a: &TExpr, b: &TExpr) -> bool {
    match (&a.kind, &b.kind) {
        (TKind::Const(x), TKind::Const(y)) => x == y,
        (TKind::Param(x), TKind::Param(y)) => fold_name(x) == fold_name(y),
        (TKind::Value(x), TKind::Value(y)) => x == y,
        (TKind::Unit(x), TKind::Unit(y)) => x == y,
        (
            TKind::Field {
                level: l1,
                source: s1,
                col: c1,
                derefs: d1,
            },
            TKind::Field {
                level: l2,
                source: s2,
                col: c2,
                derefs: d2,
            },
        ) => l1 == l2 && s1 == s2 && c1 == c2 && d1 == d2,
        (TKind::Neg(x), TKind::Neg(y)) | (TKind::Not(x), TKind::Not(y)) => same_tir_expr(x, y),
        (
            TKind::Arith {
                first: f1,
                steps: s1,
            },
            TKind::Arith {
                first: f2,
                steps: s2,
            },
        ) => {
            same_tir_expr(f1, f2)
                && s1.len() == s2.len()
                && s1
                    .iter()
                    .zip(s2)
                    .all(|(x, y)| x.op == y.op && same_tir_expr(&x.rhs, &y.rhs))
        }
        (TKind::And(x), TKind::And(y)) | (TKind::Or(x), TKind::Or(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_tir_expr(x, y))
        }
        (TKind::Compare(o1, l1, r1), TKind::Compare(o2, l2, r2)) => {
            o1 == o2 && same_tir_expr(l1, l2) && same_tir_expr(r1, r2)
        }
        (TKind::IsNull { l: l1, not: n1 }, TKind::IsNull { l: l2, not: n2 }) => {
            n1 == n2 && same_tir_expr(l1, l2)
        }
        (TKind::Refs { l: l1, table: t1 }, TKind::Refs { l: l2, table: t2 }) => {
            t1 == t2 && same_tir_expr(l1, l2)
        }
        (
            TKind::Case {
                operand: o1,
                whens: w1,
                else_: e1,
            },
            TKind::Case {
                operand: o2,
                whens: w2,
                else_: e2,
            },
        ) => {
            o1.as_deref()
                .zip(o2.as_deref())
                .map_or(o1.is_none() && o2.is_none(), |(x, y)| same_tir_expr(x, y))
                && w1.len() == w2.len()
                && w1
                    .iter()
                    .zip(w2)
                    .all(|((c1, r1), (c2, r2))| same_tir_expr(c1, c2) && same_tir_expr(r1, r2))
                && e1
                    .as_deref()
                    .zip(e2.as_deref())
                    .map_or(e1.is_none() && e2.is_none(), |(x, y)| same_tir_expr(x, y))
        }
        (
            TKind::Cast {
                e: e1,
                to: t1,
                derefs: d1,
                num: n1,
            },
            TKind::Cast {
                e: e2,
                to: t2,
                derefs: d2,
                num: n2,
            },
        ) => t1 == t2 && d1 == d2 && n1 == n2 && same_tir_expr(e1, e2),
        (TKind::Func { f: f1, args: a1 }, TKind::Func { f: f2, args: a2 }) => {
            f1 == f2 && a1.len() == a2.len() && a1.iter().zip(a2).all(|(x, y)| same_tir_expr(x, y))
        }
        (
            TKind::Agg {
                f: f1,
                distinct: d1,
                arg: a1,
                nested: n1,
            },
            TKind::Agg {
                f: f2,
                distinct: d2,
                arg: a2,
                nested: n2,
            },
        ) => {
            f1 == f2
                && d1 == d2
                && n1 == n2
                && a1
                    .as_deref()
                    .zip(a2.as_deref())
                    .map_or(a1.is_none() && a2.is_none(), |(x, y)| same_tir_expr(x, y))
        }
        _ => false,
    }
}

pub(crate) fn distinct_paired_presentation(
    schema: &QuerySchema,
    select: &Select,
    item: &tir::Item,
    source_table: impl Fn(u16) -> Option<QTableId>,
) -> bool {
    match &item.expr.kind {
        TKind::Func {
            f: Func::Presentation | Func::RefPresentation,
            args,
        } => {
            let [arg] = args.as_slice() else {
                return false;
            };
            select.items.iter().any(|other| {
                other.nested.is_none()
                    && !matches!(
                        other.expr.kind,
                        TKind::Func {
                            f: Func::Presentation | Func::RefPresentation,
                            ..
                        }
                    )
                    && same_tir_expr(&other.expr, arg)
            })
        }
        TKind::Field {
            level,
            source,
            col,
            derefs,
        } => {
            if let Some((last, head)) = derefs.split_last() {
                if last.targets.is_empty()
                    || !last
                        .targets
                        .iter()
                        .all(|(t, f)| schema.is_presentation_field(*t, *f))
                {
                    return false;
                }
                return select.items.iter().any(|other| {
                    other.nested.is_none()
                        && matches!(
                            &other.expr.kind,
                            TKind::Field {
                                level: ol,
                                source: os,
                                col: oc,
                                derefs: od,
                            } if ol == level && os == source && oc == col && od.as_slice() == head
                        )
                });
            }
            let (0, Col::Field(field)) = (*level, *col) else {
                return false;
            };
            let Some(table) = source_table(*source) else {
                return false;
            };
            if !schema.is_presentation_field(table, field) {
                return false;
            }
            let Some(ref_field) = schema
                .table(table)
                .fields
                .iter()
                .position(|f| f.role == FieldRole::Std(StdRole::Ref))
                .and_then(|i| u16::try_from(i).ok())
                .map(QFieldId)
            else {
                return false;
            };
            select.items.iter().any(|other| {
                other.nested.is_none()
                    && matches!(
                        &other.expr.kind,
                        TKind::Field {
                            level: 0,
                            source: os,
                            col: Col::Field(of),
                            derefs: od,
                        } if os == source && *of == ref_field && od.is_empty()
                    )
            })
        }
        _ => false,
    }
}

pub(crate) fn uniform_movement_union_presentation(query: &Query) -> Option<(u16, u16)> {
    if query.parts.len() < 2
        || query.unions.len() != query.parts.len() - 1
        || query.unions.iter().any(|all| !all)
        || !query.order.is_empty()
        || query.autoorder
        || query.totals.is_some()
    {
        return None;
    }
    let slots = |select: &Select| -> Option<(u16, u16, platform_catalog::TypeIdx)> {
        if select.distinct
            || select.group.is_some()
            || select.having.is_some()
            || select.items.iter().any(|item| item.nested.is_some())
        {
            return None;
        }
        let mut presentations =
            select
                .items
                .iter()
                .enumerate()
                .filter_map(|(col, item)| match &item.expr.kind {
                    TKind::Func {
                        f: Func::Presentation,
                        args,
                    } => Some((col, args)),
                    _ => None,
                });
        let (present, args) = presentations.next()?;
        if presentations.next().is_some() {
            return None;
        }
        let [arg] = args.as_slice() else {
            return None;
        };
        let TKind::Field {
            level: 0,
            source,
            col: Col::Field(field),
            derefs,
        } = &arg.kind
        else {
            return None;
        };
        if !derefs.is_empty() || arg.ty.atom_count() != 1 {
            return None;
        }
        let [crate::Other::SysEnum(kind)] = arg.ty.others.as_slice() else {
            return None;
        };
        let raw = select.items.iter().position(|item| {
            item.nested.is_none()
                && item.expr.ty.atom_count() == 1
                && item.expr.ty.others.as_slice() == [crate::Other::SysEnum(*kind)]
                && matches!(
                    &item.expr.kind,
                    TKind::Field { level: 0, source: raw_source, col: Col::Field(raw_field), derefs }
                        if raw_source == source && raw_field == field && derefs.is_empty()
                )
        })?;
        Some((
            u16::try_from(present).ok()?,
            u16::try_from(raw).ok()?,
            *kind,
        ))
    };
    let expected = slots(query.parts.first()?)?;
    for part in query.parts.iter().skip(1) {
        if slots(part) != Some(expected) {
            return None;
        }
    }
    Some((expected.0, expected.1))
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
    src: SrcId,
    rel: Rel,
    joins: Vec<(JoinKind, Declared<'a>, &'a TExpr)>,
}

struct Builder<'a> {
    qr8: Vec<super::Qr8Param>,
    schema: &'a QuerySchema,
    sources: Vec<SourceS>,
    levels: Vec<Vec<SrcId>>,
    implicit_sections: HashMap<super::implicit_tabular::ImplicitSectionKey, SrcId>,
    implicit_joins: HashMap<SrcId, Vec<(ColSel, SrcId, QTableId)>>,
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
    nested_from: Option<&'a Select>,
    nested_outputs: &'a [tir::Item],
    nested_source: Option<(SrcId, Vec<NestedS>)>,
    present: Vec<u16>,
    in_select_item: bool,
    single_part: bool,
    union_present: Option<UnionPresent>,
    query_depth: u32,
    pending_auto: bool,
    temp_out: bool,
    section: Option<SectionMap>,
    nested_owner: Option<NestedOwner>,
    totals: Option<crate::totals::TotalsS>,
    totals_open: bool,
    totals_extra: Vec<&'a TExpr>,
    totals_extra_cols: Vec<u16>,
    totals_extra_built: Vec<Expr>,
    root_sort: Vec<SortMeta>,
    col_names: Vec<String>,
    virtual_extra: HashMap<SrcId, Vec<QType>>,
    vt_account_remap: Option<(SrcId, QFieldId)>,
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

#[derive(Clone, Copy)]
struct NestedOwner {
    owner_src: SrcId,
    owner_table: Option<QTableId>,
    owner_field: QFieldId,
    section_table: QTableId,
    section_ref_field: QFieldId,
    section_depth: usize,
}

impl<'a> Builder<'a> {
    fn new(c: &'a CompiledStmt, schema: &'a QuerySchema) -> Builder<'a> {
        let nested_from = match &c.typed.kind {
            tir::StmtKind::Query(query) if c.temp_out.is_none() => tir::nested_from_select(query)
                .filter(|select| {
                    select.sources.iter().all(|source| {
                        matches!(&source.from, From::Table { id, .. }
                            if !matches!(schema.table(*id).kind, TableKind::Virtual(_)))
                    })
                }),
            _ => None,
        };
        let nested_outputs: &[tir::Item] = match &c.typed.kind {
            tir::StmtKind::Query(query) if nested_from.is_some() => query
                .parts
                .first()
                .map_or(&[][..], |select| select.items.as_slice()),
            _ => &[],
        };
        let mut b = Builder {
            qr8: Vec::new(),
            schema,
            sources: Vec::new(),
            levels: Vec::new(),
            implicit_sections: HashMap::new(),
            implicit_joins: HashMap::new(),
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
            nested_from,
            nested_outputs,
            nested_source: None,
            present: Vec::new(),
            in_select_item: false,
            virtual_extra: HashMap::new(),
            vt_account_remap: None,
            single_part: false,
            union_present: None,
            query_depth: 0,
            pending_auto: false,
            temp_out: c.temp_out.is_some(),
            section: None,
            nested_owner: None,
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
        let union_auto =
            if q.autoorder && q.parts.len() > 1 && self.query_depth == 1 && !self.temp_out {
                uniform_union_autoorder(q, self.schema)
            } else {
                None
            };
        let Some(first) = q.parts.first() else {
            return internal("a query without parts");
        };
        if let Some(t) = &q.totals {
            if self.query_depth != 1 || self.temp_out {
                return later("ИТОГИ вне запроса оператора", "Q4");
            }
            self.totals_prepare(q, t)?;
            let (rel, types) = self.query_rows(q, first, union_auto)?;
            self.totals = Some(self.totals_finish(q, t, &types)?);
            self.totals_open = false;
            return Ok((rel, types));
        }
        self.query_rows(q, first, union_auto)
    }

    fn query_rows(
        &mut self,
        q: &'a Query,
        first: &'a Select,
        union_auto: Option<UnionAutoorder>,
    ) -> R<(Rel, Vec<QType>)> {
        if self.query_depth == 1 {
            self.single_part = q.parts.len() == 1;
            self.union_present = if self.temp_out {
                None
            } else {
                union_presentation(q, self.schema)
            };
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
            if self.query_depth != 1
                || self.temp_out
                || !super::nested_union::supported(self.schema, q)
            {
                return later("вложенная таблица в объединении", "Q3");
            }
            if let Err(what) = super::nested_union::validate_order(q) {
                return later(what, "Q3");
            }
        }
        let mut parts = Vec::with_capacity(q.parts.len());
        let mut auto_sort = Vec::new();
        for (i, p) in q.parts.iter().enumerate() {
            let mut part = self.select(p, None)?;
            if let Some(order) = union_auto {
                self.union_autoorder_part(
                    &mut part,
                    &mut auto_sort,
                    order.reference_desc,
                    order.scalar_desc,
                )?;
            }
            parts.push(part);
            if i > 0 {
                for (t, it) in types.iter_mut().zip(&p.items) {
                    *t = t.merge(&it.expr.ty);
                }
            }
        }
        let rel = Rel::Union {
            parts,
            all: q.unions.clone(),
        };
        let (mut rel, hierarchy) = self.hierarchy_union(q, rel, &types)?;
        if union_auto.is_some() {
            rel = Rel::Sort {
                input: Box::new(rel),
                keys: auto_sort,
            };
        } else if !hierarchy && (!q.order.is_empty() || q.autoorder) {
            rel = self.order_union(q, first, rel, &types)?;
        }
        Ok((rel, types))
    }

    fn select(&mut self, sel: &'a Select, order: Option<&'a [tir::OrderKey]>) -> R<Rel> {
        let auto = std::mem::take(&mut self.pending_auto);
        self.allowed |= sel.allowed;
        if sel.items.iter().any(|i| i.nested.is_some()) {
            if self.query_depth != 1
                && !(self.query_depth == 2
                    && self
                        .nested_from
                        .is_some_and(|inner| std::ptr::eq(inner, sel)))
            {
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
        let outer_select_item = std::mem::replace(&mut self.in_select_item, false);
        let res = self.select_inner(sel, order, auto);
        self.in_select_item = outer_select_item;
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
        self.collect_implicit_sections(sel, order, &mut ids)?;
        self.levels.push(ids);
        let res = self.select_body(sel, order, auto, declared);
        self.levels.pop();
        res
    }

    fn collect_implicit_sections(
        &mut self,
        sel: &'a Select,
        order: Option<&'a [tir::OrderKey]>,
        ids: &mut Vec<SrcId>,
    ) -> R<()> {
        use super::implicit_tabular::{
            ImplicitWork, match_implicit_section, push_select_work, step_work,
        };
        let tir_count = ids.len();
        let mut work: Vec<ImplicitWork<'a>> = Vec::new();
        let extra: Vec<&'a TExpr> = if self.query_depth == 1 && self.totals_open {
            self.totals_extra.clone()
        } else {
            Vec::new()
        };
        push_select_work(sel, order, &extra, 0, &mut |item| {
            work.push(item);
            Ok(())
        })?;
        while let Some(item) = work.pop() {
            step_work(
                item,
                &mut |next| {
                    work.push(next);
                    Ok(())
                },
                &mut |source, col, derefs| {
                    let index = usize::from(source);
                    if index >= tir_count {
                        return Ok(());
                    }
                    let Some(&owner_src) = ids.get(index) else {
                        return Ok(());
                    };
                    let owner_table = match self.sources.get(owner_src.0 as usize).map(|s| &s.kind)
                    {
                        Some(SrcKind::Table(table)) => Some(*table),
                        _ => None,
                    };
                    let col_sel = match col {
                        Col::Field(f) => ColSel::Field(f),
                        Col::Column(i) => ColSel::Column(i),
                    };
                    let Some(access) = match_implicit_section(
                        self.schema,
                        owner_src,
                        owner_table,
                        col_sel,
                        derefs,
                    ) else {
                        return Ok(());
                    };
                    if self.implicit_sections.contains_key(&access.key) {
                        return Ok(());
                    }
                    let Some(tab_src) = self.section_source(access.key.tab_table, false)? else {
                        return internal("a tabular section without a source");
                    };
                    self.implicit_sections.insert(access.key, tab_src);
                    self.implicit_joins.entry(owner_src).or_default().push((
                        access.key.owner_col,
                        tab_src,
                        access.key.tab_table,
                    ));
                    ids.push(tab_src);
                    Ok(())
                },
            )?;
        }
        Ok(())
    }

    fn attach_implicit_joins(&mut self, owner_src: SrcId, mut rel: Rel) -> R<Rel> {
        let Some(entries) = self.implicit_joins.remove(&owner_src) else {
            return Ok(rel);
        };
        for (owner_col, tab_src, tab_table) in entries {
            let lhs = self.section_col(tab_src, tab_table, StdRole::Ref)?;
            let rhs = Expr {
                kind: ExprKind::Col {
                    src: owner_src,
                    col: owner_col,
                },
                ty: self.column_type(owner_src, owner_col),
            };
            let on = Expr {
                kind: ExprKind::Compare(query_syntax::ast::CmpOp::Eq, Box::new(lhs), Box::new(rhs)),
                ty: QType::boolean(),
            };
            self.joins = self.joins.saturating_add(1);
            rel = Rel::Join {
                kind: JoinKind::Left,
                l: Box::new(rel),
                r: Box::new(Rel::Scan {
                    src: tab_src,
                    table: tab_table,
                }),
                on,
            };
        }
        Ok(rel)
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
        let forwarding = self.query_depth == 2
            && self
                .nested_from
                .is_some_and(|inner| std::ptr::eq(inner, sel));
        let mut items = Vec::with_capacity(sel.items.len());
        let prev_select_item = std::mem::replace(&mut self.in_select_item, true);
        for i in &sel.items {
            let e = if i.nested.is_some() {
                let retain = !forwarding
                    || self.nested_outputs.iter().any(|output| {
                        tir::forwarded_nested_column(output)
                            .is_some_and(|column| usize::from(column) == items.len())
                    });
                self.nested_item(i, items.len(), retain)?
            } else if let Some(expr) = self.forward_nested(i, items.len())? {
                expr
            } else if let Some(arg) = self.presentation_item(sel, i, items.len())? {
                arg
            } else {
                self.expr(&i.expr)?
            };
            items.push(e);
        }
        self.in_select_item = prev_select_item;
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
            let paired_hierarchy = matches!(order, [_, suffix]
                if h_at == Some(0)
                && self.query_depth == 1
                && !self.temp_out
                && self.single_part
                && !auto
                && matches!(suffix.dir, OrderDir::Asc | OrderDir::Desc)
                && suffix.expr.ty.refs.is_empty()
                && suffix.expr.ty.others.is_empty()
                && order.iter().all(|key| {
                    matches!(
                        &key.expr.kind,
                        TKind::Field { level: 0, source: 0, derefs, .. } if derefs.is_empty()
                    )
                })
                && matches!(sel.sources.as_slice(), [source]
                    if source.joins.is_empty()
                        && matches!(&source.from, tir::From::Table { id, .. }
                            if matches!(self.schema.table(*id).kind, TableKind::Object(_))
                                && self.role_field(*id, StdRole::Ref).is_some()
                                && self.role_field(*id, StdRole::Parent).is_some())));
            if h_at.is_some()
                && (aggregated || sel.distinct || (order.len() != 1 && !paired_hierarchy))
            {
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
                        if u16::try_from(c).is_ok_and(|c| self.present.contains(&c))
                            || items.get(c).is_some_and(|e| {
                                matches!(
                                    e.kind,
                                    ExprKind::Func {
                                        f: Func::Presentation | Func::RefPresentation,
                                        ..
                                    }
                                )
                            })
                        {
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
                if matches!(k.dir, OrderDir::Hierarchy | OrderDir::HierarchyDesc) {
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
                if let Some(order_expr) = self.chart_code_order_expr(&base) {
                    let order_expr = if aggregated {
                        let ty = order_expr.ty.clone();
                        let idx = self.agg(AggCall {
                            f: Func::Min,
                            distinct: false,
                            arg: Some(order_expr),
                            ty: ty.clone(),
                        })?;
                        Expr {
                            kind: ExprKind::AggRef(idx),
                            ty,
                        }
                    } else {
                        order_expr
                    };
                    let order_ty = order_expr.ty.clone();
                    let meta_order = order_expr.clone();
                    let i = push_hidden(&mut hidden, items.len(), order_expr)?;
                    sort.push((out(usize::from(i), order_ty)?, desc));
                    sort_meta.push(SortMeta {
                        col: usize::from(i),
                        desc,
                        base_col: col,
                        base: base.clone(),
                        key: meta_order,
                    });
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
                Some(src) => self.main_order(src, sel)?,
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
            let mut ref_index: HashMap<u64, Vec<(u16, usize)>> = HashMap::new();
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
            let explicit_keys = keys.len();
            for (slot, key) in keys.iter().enumerate() {
                if let Ok(slot) = u16::try_from(slot) {
                    index_ref_prefixes(self.schema, key, slot, &mut ref_index);
                }
            }
            let allow_prefix = sets.len() == 1;
            let mut ref_grouped: HashMap<SrcId, u16> = HashMap::new();
            for (slot, key) in keys.iter().enumerate() {
                let Ok(slot) = u16::try_from(slot) else {
                    continue;
                };
                if let ExprKind::Col {
                    src,
                    col: ColSel::Field(f),
                } = &key.kind
                    && own.contains(src)
                    && let Some(SrcKind::Table(qt)) =
                        self.sources.get(src.0 as usize).map(|s| &s.kind)
                    && matches!(self.schema.table(*qt).kind, TableKind::Object(_))
                    && self
                        .schema
                        .field(*qt, *f)
                        .is_some_and(|field| field.role == FieldRole::Std(StdRole::Ref))
                {
                    ref_grouped.entry(*src).or_insert(slot);
                }
            }
            if !ref_grouped.is_empty() {
                let mut pending: Vec<(u16, Expr)> = Vec::new();
                for e in items.iter().chain(having.iter()).chain(hidden.iter()) {
                    visit_expr(e, true, &mut |n| {
                        if let ExprKind::Col {
                            src,
                            col: ColSel::Field(_),
                        } = &n.kind
                            && let Some(&ref_slot) = ref_grouped.get(src)
                        {
                            pending.push((ref_slot, n.clone()));
                        }
                    });
                }
                for (ref_slot, col_expr) in pending {
                    let dep_slot = find_or_push(&mut keys, &mut index, col_expr)?;
                    if usize::from(dep_slot) >= explicit_keys {
                        for s in &mut sets {
                            if s.contains(&ref_slot) && !s.contains(&dep_slot) {
                                s.push(dep_slot);
                            }
                        }
                    }
                }
            }
            let ki = KeyIndex {
                keys: &keys,
                index: &index,
                ref_index: &ref_index,
                own: &own,
                anchor,
                in_sub: false,
                allow_prefix,
                explicit_keys,
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
                let Some(i) = a
                    .arg
                    .as_ref()
                    .and_then(|x| ki.find(x))
                    .filter(|&i| usize::from(i) < explicit_keys)
                else {
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

    fn forward_nested(&mut self, item: &'a tir::Item, col: usize) -> R<Option<Expr>> {
        let Some(column) = tir::forwarded_nested_column(item) else {
            return Ok(None);
        };
        let Some((source, recipes)) = &self.nested_source else {
            return Ok(None);
        };
        if self.query_depth != 1 || self.levels.last().and_then(|ids| ids.first()) != Some(source) {
            return Ok(None);
        }
        let Some(mut recipe) = recipes.iter().find(|recipe| recipe.col == column).cloned() else {
            return Ok(None);
        };
        recipe.col =
            u16::try_from(col).map_err(|_| BuildError::Internal("too many columns".into()))?;
        let expr = self.expr(&item.expr)?;
        self.nested.push(recipe);
        Ok(Some(expr))
    }

    fn nested_item(&mut self, it: &'a tir::Item, col: usize, retain: bool) -> R<Expr> {
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
        if self.query_depth == 1 && self.nested.iter().any(|nested| nested.col == col) {
            return Ok(owner_ref);
        }
        let sources_before = self.sources.len();
        let Some(tab_src) = self.section_source(tab, nested.iter().any(|item| item.star))? else {
            return internal("a tabular section without a source");
        };
        let owner = self.section_col(tab_src, tab, StdRole::Ref)?;
        let number = self.section_col(tab_src, tab, StdRole::LineNumber)?;
        let outer: HashSet<SrcId> = self.levels.iter().flatten().copied().collect();
        let outer_aggs = std::mem::take(&mut self.aggs);
        let outer_index = std::mem::take(&mut self.agg_index);
        self.levels.push(vec![tab_src]);
        let next_nested_owner = self.nested_owner_context(&owner_ref, &owner, tab);
        let prev_nested_owner = std::mem::replace(&mut self.nested_owner, next_nested_owner);
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
        self.nested_owner = prev_nested_owner;
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
        if !retain {
            drop(self.nested.pop());
            self.sources.truncate(sources_before);
        }
        Ok(owner_ref)
    }

    fn nested_owner_context(
        &self,
        owner_ref: &Expr,
        section_owner: &Expr,
        section_table: QTableId,
    ) -> Option<NestedOwner> {
        let (
            ExprKind::Col {
                src: owner_src,
                col: ColSel::Field(owner_field),
            },
            ExprKind::Col {
                col: ColSel::Field(section_ref_field),
                ..
            },
        ) = (&owner_ref.kind, &section_owner.kind)
        else {
            return None;
        };
        let owner_table = match self
            .sources
            .get(owner_src.0 as usize)
            .map(|source| &source.kind)
        {
            Some(SrcKind::Table(table))
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

    fn rewrite_nested_owner_field(
        &mut self,
        e_ty: &QType,
        level: u16,
        source: u16,
        col: Col,
        derefs: &[tir::Deref],
    ) -> R<Option<Expr>> {
        let Some(no) = self.nested_owner else {
            return Ok(None);
        };
        let Col::Field(field) = col else {
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
                let result = self.field(e_ty, section_level, 0, Col::Field(sec_field), rest);
                self.nested_owner = saved;
                return result.map(Some);
            }
            if has_nested_deref(derefs) {
                return Ok(None);
            }
            let saved = self.nested_owner.take();
            let result = self.field(
                e_ty,
                section_level,
                0,
                Col::Field(no.section_ref_field),
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
            let result = self.field(e_ty, section_level, 0, Col::Field(sec_field), rest);
            self.nested_owner = saved;
            return result.map(Some);
        }
        if field_info.nested.is_some() {
            return Ok(None);
        }
        if derefs.is_empty() && self.schema.is_presentation_field(owner_table, field) {
            if !self.in_select_item {
                return Ok(None);
            }
            let owner_ref_ty = self
                .schema
                .field(owner_table, no.owner_field)
                .map(|f| f.ty.clone())
                .unwrap_or_default();
            let saved = self.nested_owner.take();
            let ref_expr = self.field(
                &owner_ref_ty,
                section_level,
                0,
                Col::Field(no.section_ref_field),
                &[],
            );
            self.nested_owner = saved;
            let ref_expr = ref_expr?;
            return Ok(Some(Expr {
                kind: ExprKind::Func {
                    f: Func::Presentation,
                    args: vec![ref_expr],
                },
                ty: e_ty.clone(),
            }));
        }
        let step_name = if !field_info.name_en.is_empty() {
            field_info.name_en.clone()
        } else {
            field_info.name_ru.clone()
        };
        let mut steps = Vec::with_capacity(1 + derefs.len());
        steps.push(tir::Deref {
            name: step_name,
            targets: vec![(owner_table, field)],
        });
        steps.extend_from_slice(derefs);
        let saved = self.nested_owner.take();
        let result = self.field(
            e_ty,
            section_level,
            0,
            Col::Field(no.section_ref_field),
            &steps,
        );
        self.nested_owner = saved;
        result.map(Some)
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
                id: table,
                accounting_dependencies,
                ..
            } if matches!(
                self.schema.table(*table).kind,
                TableKind::Virtual(VtKind::ScheduleData | VtKind::ActualActionPeriod)
            ) && accounting_dependencies
                .as_ref()
                .is_none_or(|capture| !capture.complete) =>
            {
                return Err(BuildError::Later {
                    what: if self.schema.table(*table).kind
                        == TableKind::Virtual(VtKind::ScheduleData)
                    {
                        "Q6c-ScheduleDataArguments"
                    } else {
                        "Q6c-ActualActionPeriodArguments"
                    },
                    stage: "Q6c",
                });
            }
            From::Table {
                id: t,
                args,
                records,
                accounting,
                drcr,
                periodicity,
                complement,
                span,
                breakdown,
                base,
                ..
            } if self.schema.supports_register_vt(*t) => self.register_vt(
                id,
                *t,
                args,
                records.as_ref(),
                *accounting,
                drcr.as_ref(),
                base.as_deref(),
                breakdown,
                (*periodicity, *complement),
                *span,
            )?,
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
                            VtKind::SequenceBoundaries
                                | VtKind::BusinessProcessPoints
                                | VtKind::ExtDimensions
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
                let forwarding = self.query_depth == 1
                    && q.parts.first().is_some_and(|select| {
                        self.nested_from
                            .is_some_and(|inner| std::ptr::eq(inner, select))
                    });
                let (input, columns) = if forwarding {
                    let outer = std::mem::take(&mut self.nested);
                    let built = self.query(q);
                    let recipes = std::mem::replace(&mut self.nested, outer);
                    self.nested_source = Some((id, recipes));
                    built?
                } else {
                    self.query(q)?
                };
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
        Ok(Declared {
            src: id,
            rel,
            joins,
        })
    }

    fn assemble(&mut self, d: Declared<'a>) -> R<Rel> {
        let mut rel = self.attach_implicit_joins(d.src, d.rel)?;
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
                    if !self.in_select_item {
                        return Err(BuildError::Later {
                            what: "ПРЕДСТАВЛЕНИЕ",
                            stage: "Q3",
                        });
                    }
                    let [arg] = args.as_slice() else {
                        return internal("ПРЕДСТАВЛЕНИЕ takes one argument");
                    };
                    return Ok(Expr {
                        kind: ExprKind::Func {
                            f: *f,
                            args: vec![self.expr(arg)?],
                        },
                        ty,
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
            } => {
                let prev = std::mem::replace(&mut self.in_select_item, false);
                let res = self.section_agg(*f, *distinct, arg.as_deref(), ty);
                self.in_select_item = prev;
                return res;
            }
            TKind::Agg {
                f, distinct, arg, ..
            } => {
                let prev = std::mem::replace(&mut self.in_select_item, false);
                let arg = match arg {
                    Some(a) => self.expr(a).map(Some),
                    None => Ok(None),
                };
                self.in_select_item = prev;
                let call = AggCall {
                    f: *f,
                    distinct: *distinct,
                    arg: arg?,
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
        if let Some(rewritten) =
            self.rewrite_nested_owner_field(e_ty, level, source, col, derefs)?
        {
            return Ok(rewritten);
        }
        if self.in_select_item
            && let Some(ref_expr) = self.presentation_of_field(level, source, col, derefs)?
        {
            return Ok(Expr {
                kind: ExprKind::Func {
                    f: Func::Presentation,
                    args: vec![ref_expr],
                },
                ty: e_ty.clone(),
            });
        }
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
            Col::Column(0 | 1)
                if self
                    .vt_account_remap
                    .is_some_and(|(remap_src, _)| remap_src == src) =>
            {
                let Some((_, account_field)) = self.vt_account_remap else {
                    return internal("missing account remap");
                };
                ColSel::Field(account_field)
            }
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
        let owner_table = match self.sources.get(src.0 as usize).map(|s| &s.kind) {
            Some(SrcKind::Table(table)) => Some(*table),
            _ => None,
        };
        if let Some(access) = super::implicit_tabular::match_implicit_section(
            self.schema,
            src,
            owner_table,
            sel,
            derefs,
        ) && let Some(&tab_src) = self.implicit_sections.get(&access.key)
        {
            let rest = derefs.get(access.rest_start..).unwrap_or_default();
            let tab_col = ColSel::Field(access.tab_field);
            let base_ty = if rest.is_empty() {
                e_ty.clone()
            } else {
                self.column_type(tab_src, tab_col)
            };
            let base = Expr {
                kind: ExprKind::Col {
                    src: tab_src,
                    col: tab_col,
                },
                ty: base_ty,
            };
            if rest.is_empty() {
                return Ok(base);
            }
            self.touch_derefs(rest);
            return Ok(Expr {
                kind: ExprKind::Deref {
                    base: Box::new(base),
                    steps: rest.to_vec(),
                },
                ty: e_ty.clone(),
            });
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
        let union_ok = match self.union_present {
            Some(UnionPresent::Movement { present, .. }) => {
                usize::from(present) == col
                    && distinct_movement_presentation(self.schema, sel, it, |source| {
                        let src = self.levels.last()?.get(usize::from(source))?;
                        match self.sources.get(src.0 as usize)?.kind {
                            SrcKind::Table(table) => Some(table),
                            _ => None,
                        }
                    })
            }
            Some(UnionPresent::Ref) => true,
            None => false,
        };
        if self.query_depth != 1 || (!self.single_part && !union_ok) {
            return Ok(None);
        }
        if sel.distinct {
            if self.totals_open {
                return later("ПРЕДСТАВЛЕНИЕ при РАЗЛИЧНЫЕ", "Q3");
            }
            let paired = distinct_paired_presentation(self.schema, sel, it, |source| {
                let src = self.levels.last()?.get(usize::from(source))?;
                match self.sources.get(src.0 as usize)?.kind {
                    SrcKind::Table(table) => Some(table),
                    _ => None,
                }
            });
            if !paired {
                return Ok(None);
            }
        }
        let Ok(c) = u16::try_from(col) else {
            return internal("too many columns");
        };
        let e = match (arg, field) {
            (Some(a), _) => self.expr(a)?,
            (None, Some(f)) => f,
            (None, None) => return Ok(None),
        };
        if self.single_part || !self.present.contains(&c) {
            self.present.push(c);
        }
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

    #[allow(
        clippy::too_many_arguments,
        reason = "source options retain separate typed ownership"
    )]
    fn register_vt(
        &mut self,
        src: SrcId,
        t: QTableId,
        args: &'a [Option<TExpr>],
        records: Option<&tir::RecordsOptions>,
        accounting: Option<tir::AccountingArguments>,
        drcr: Option<&'a tir::DrCrSource>,
        base: Option<&'a tir::BaseArguments>,
        breakdown: &'a [QFieldId],
        period: (
            crate::schema::Periodicity,
            Option<crate::schema::Complement>,
        ),
        span: Option<query_syntax::Span>,
    ) -> R<Rel> {
        let (periodicity, complement) = period;
        let accounting_balance = crate::lower::is_accounting_balance(self.schema, t);
        if accounting_balance && !accounting.is_some_and(|a| a.supports_balance(args)) {
            return Err(BuildError::Later {
                what: "Q6a-AccountingBalanceArguments",
                stage: "Q6a",
            });
        }
        let accounting_turnovers = crate::lower::is_accounting_turnovers(self.schema, t);
        if accounting_turnovers && !accounting.is_some_and(|a| a.supports_turnovers(args)) {
            return Err(BuildError::Later {
                what: "Q6b-TurnoversArguments",
                stage: "Q6b",
            });
        }
        let accounting_bt = crate::lower::is_accounting_balance_and_turnovers(self.schema, t);
        if accounting_bt && !accounting.is_some_and(|a| a.supports_balance_and_turnovers(args)) {
            return Err(BuildError::Later {
                what: "Q6c-AccountingBalanceAndTurnoversArguments",
                stage: "Q6c",
            });
        }
        if self.schema.table(t).kind == TableKind::Virtual(VtKind::DrCrTurnovers) {
            let Some(source) = drcr else {
                return internal("DrCr source without a semantic marker");
            };
            if source.supplied.len() != args.len()
                || source.supplied.iter().any(|x| *x > 1)
                || args.iter().any(Option::is_some)
                || source.date_slots & !0b11 != 0
                || (0..2)
                    .any(|i| source.date_slots & (1 << i) != 0 && source.date_argument(i).is_none())
            {
                return internal("invalid DrCr semantic marker or executable arguments");
            }
        } else if drcr.is_some() {
            return internal("DrCr marker on another virtual table");
        }
        if drcr.is_some_and(tir::DrCrSource::has_unsupported_arguments) {
            return Err(BuildError::Later {
                what: "Q6b-DrCrTurnoversArguments",
                stage: "Q6b",
            });
        }
        if let Some(reason) = records.and_then(|r| r.deferred) {
            return Err(BuildError::Later {
                what: reason.what(),
                stage: "Q6a",
            });
        }
        let table = self.schema.table(t);
        let Some(register) = table.owner else {
            return internal("a virtual table without its register");
        };
        let TableKind::Virtual(kind) = table.kind else {
            return internal("not a virtual table");
        };
        let base_spec = if kind == VtKind::Base {
            let Some(base_args) = base else {
                return Err(BuildError::Later {
                    what: "Q6d-CalculationBaseArguments",
                    stage: "Q6d",
                });
            };
            if base_args.deferred {
                return Err(BuildError::Later {
                    what: "Q6d-CalculationBaseArguments",
                    stage: "Q6d",
                });
            }
            let (Some(main_dims_param), Some(base_dims_param)) = (
                base_args.main_dims_param.as_ref(),
                base_args.base_dims_param.as_ref(),
            ) else {
                return Err(BuildError::Later {
                    what: "Q6d-CalculationBaseArguments",
                    stage: "Q6d",
                });
            };
            let base_table = self.schema.calculation_base(t).map_or(t, |f| f.base);
            let extra_types: Vec<QType> = breakdown
                .iter()
                .map(|f| {
                    self.schema
                        .field(base_table, *f)
                        .map(|x| x.ty.clone().with_null())
                        .unwrap_or_default()
                })
                .collect();
            if !extra_types.is_empty() {
                self.virtual_extra.insert(src, extra_types);
            }
            Some(crate::ir::BaseSpecS {
                main_dims_param: main_dims_param.clone(),
                base_dims_param: base_dims_param.clone(),
                breakdown_param: base_args.breakdown_param.clone(),
                breakdown: breakdown.to_vec(),
            })
        } else {
            None
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
        if slice
            || matches!(
                kind,
                VtKind::RecordsWithExtDimensions
                    | VtKind::ActualActionPeriod
                    | VtKind::ScheduleData
                    | VtKind::Base
            )
        {
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
        let mut cond: Option<Expr> = None;
        let mut ext_dims_arg: Option<Expr> = None;
        let combine_cond = |acc: &mut Option<Expr>, next_cond: Expr| {
            *acc = Some(match acc.take() {
                None => next_cond,
                Some(prev) => Expr {
                    kind: ExprKind::And(vec![prev, next_cond]),
                    ty: QType::boolean().with_null(),
                },
            });
        };
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
                VtParamKind::AccountCondition => {
                    if let Some(e) = a {
                        let account_field = table
                            .fields
                            .iter()
                            .enumerate()
                            .find(|(_, f)| f.role == FieldRole::Std(StdRole::Account))
                            .and_then(|(idx, _)| u16::try_from(idx).ok().map(QFieldId))
                            .ok_or_else(|| {
                                BuildError::Internal(
                                    "accounting virtual table missing Account field".into(),
                                )
                            })?;
                        let prev_remap = self.vt_account_remap.replace((inner, account_field));
                        self.levels.push(vec![inner]);
                        let c = self.expr(e);
                        self.levels.pop();
                        self.vt_account_remap = prev_remap;
                        combine_cond(&mut cond, c?);
                    }
                }
                VtParamKind::ExtDimensions if accounting_balance || accounting_bt => {
                    if let Some(e) = a {
                        ext_dims_arg = Some(self.expr(e)?);
                    }
                }
                VtParamKind::Condition => {
                    if let Some(e) = a {
                        self.levels.push(vec![inner]);
                        let c = self.expr(e);
                        self.levels.pop();
                        combine_cond(&mut cond, c?);
                    }
                }
                _ => {}
            }
        }
        if let Some(source) = drcr {
            for (i, bound) in bounds.iter_mut().enumerate() {
                if source.date_slots & (1 << i) != 0 {
                    let Some(expression) = source.date_argument(i) else {
                        return internal("missing DrCr Date argument owner");
                    };
                    *bound = Some(self.expr(expression)?);
                }
            }
        }
        let mut order = None;
        if let Some(options) = records
            && let Some(Some(e)) = args.get(3)
        {
            self.levels.push(vec![inner]);
            let expr = self.expr(e);
            self.levels.pop();
            order = Some(SortKey {
                expr: expr?,
                desc: options.order_desc,
            });
        }
        self.touch(
            if accounting_balance
                || accounting_turnovers
                || accounting_bt
                || records.is_some()
                || matches!(
                    kind,
                    VtKind::ActualActionPeriod
                        | VtKind::ScheduleData
                        | VtKind::Base
                        | VtKind::DrCrTurnovers
                )
            {
                t
            } else {
                register
            },
        );
        if accounting_balance {
            let chart = crate::lower::balance_chart(self.schema, t)
                .map_err(|e| BuildError::Internal(e.to_string()))?;
            self.touch(chart);
        } else if accounting_bt {
            let chart = crate::lower::accounting_bt_chart(self.schema, t)
                .map_err(|e| BuildError::Internal(e.to_string()))?;
            self.touch(chart);
        }
        let v = Box::new(crate::ir::RegisterVtS {
            src,
            inner,
            table: t,
            register,
            kind,
            bounds,
            cond,
            ext_dims_arg,
            order,
            top: records.and_then(|r| r.top),
            periodicity,
            complement,
            base: base_spec,
            area: None,
            access: None,
            span,
        });
        Ok(if slice {
            Rel::Slice(v)
        } else if !accounting_bt
            && kind == crate::schema::VtKind::BalanceAndTurnovers
            && !matches!(
                periodicity,
                crate::schema::Periodicity::Absent | crate::schema::Periodicity::Auto
            )
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
            (SrcKind::Virtual { .. }, ColSel::Column(i)) => self
                .virtual_extra
                .get(&src)
                .and_then(|cols| cols.get(usize::from(i)))
                .cloned()
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

fn shallow_deref_prefixes(e: &Expr, mut visit: impl FnMut(usize, u64)) {
    let ExprKind::Deref { base, steps } = &e.kind else {
        return;
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(&e.kind).hash(&mut h);
    shallow_d(base, &mut h, SHALLOW_DEPTH);
    for (i, step) in steps.iter().take(steps.len().saturating_sub(1)).enumerate() {
        step.name.hash(&mut h);
        visit(i + 1, h.finish());
    }
}

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
        ExprKind::LocalKey { key, orig } => {
            key.hash(h);
            kids(&[orig], h);
        }
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
    #[cfg(test)]
    crate::bind::security::paths::observe_ir_rel(None);
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
            #[cfg(test)]
            crate::bind::security::paths::observe_ir_rel(Some(aggs));
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
                .chain(v.ext_dims_arg.iter())
                .chain(v.area.iter().map(|a| &a.pred))
                .chain(v.access.iter())
                .chain(v.order.iter().map(|k| &k.expr))
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

fn index_ref_prefixes(
    schema: &QuerySchema,
    key: &Expr,
    slot: u16,
    ref_index: &mut HashMap<u64, Vec<(u16, usize)>>,
) {
    let ExprKind::Deref { base, steps } = &key.kind else {
        return;
    };
    if key.ty.refs.is_empty() {
        return;
    }
    let mut len = steps.len();
    while len > 0
        && steps
            .get(len - 1)
            .is_some_and(|step| crate::sema::select::is_ref_deref(schema, step))
    {
        len -= 1;
        let hash = shallow_deref_prefix(key, base, steps.get(..len).unwrap_or_default());
        ref_index.entry(hash).or_default().push((slot, len));
    }
}

fn shallow_deref_prefix(deref_expr: &Expr, base: &Expr, steps: &[tir::Deref]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if steps.is_empty() {
        shallow_d(base, &mut h, SHALLOW_DEPTH);
    } else {
        std::mem::discriminant(&deref_expr.kind).hash(&mut h);
        shallow_d(base, &mut h, SHALLOW_DEPTH);
        for step in steps {
            step.name.hash(&mut h);
        }
    }
    h.finish()
}

#[derive(Clone, Copy)]
struct KeyIndex<'k> {
    keys: &'k [Expr],
    index: &'k HashMap<u64, Vec<u16>>,
    ref_index: &'k HashMap<u64, Vec<(u16, usize)>>,
    own: &'k HashSet<SrcId>,
    anchor: Option<SrcId>,
    in_sub: bool,
    allow_prefix: bool,
    explicit_keys: usize,
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

    fn find_prefix(&self, e: &Expr) -> Option<(u16, usize)> {
        if !self.allow_prefix || self.keys.is_empty() {
            return None;
        }
        let ExprKind::Deref { base, steps } = &e.kind else {
            return None;
        };
        if steps.is_empty() {
            return None;
        }
        let mut found = None;
        if !self.ref_index.is_empty() && self.find(base).is_none() {
            let hash = shallow_deref_prefix(e, base, &[]);
            if let Some(bucket) = self.ref_index.get(&hash) {
                for &(i, key_len) in bucket {
                    if key_len != 0 {
                        continue;
                    }
                    let Some(key) = self.keys.get(usize::from(i)) else {
                        continue;
                    };
                    if let ExprKind::Deref { base: key_base, .. } = &key.kind
                        && !key.ty.refs.is_empty()
                        && key_base == base
                    {
                        found = Some((i, 0));
                        break;
                    }
                }
            }
        }
        shallow_deref_prefixes(e, |len, hash| {
            let mut matched = None;
            if let Some(bucket) = self.index.get(&hash) {
                for &i in bucket {
                    let Some(key) = self.keys.get(usize::from(i)) else {
                        continue;
                    };
                    if let ExprKind::Deref {
                        base: key_base,
                        steps: key_steps,
                    } = &key.kind
                        && !key.ty.refs.is_empty()
                        && key_base == base
                        && key_steps.as_slice().iter().eq(steps.iter().take(len))
                    {
                        matched = Some((i, len));
                        break;
                    }
                }
            }
            if matched.is_none()
                && let Some(bucket) = self.ref_index.get(&hash)
            {
                for &(i, key_len) in bucket {
                    if key_len != len {
                        continue;
                    }
                    let Some(key) = self.keys.get(usize::from(i)) else {
                        continue;
                    };
                    if let ExprKind::Deref {
                        base: key_base,
                        steps: key_steps,
                    } = &key.kind
                        && !key.ty.refs.is_empty()
                        && key_base == base
                        && key_steps
                            .get(..len)
                            .is_some_and(|prefix| prefix.iter().eq(steps.iter().take(len)))
                    {
                        matched = Some((i, len));
                        break;
                    }
                }
            }
            if let Some(m) = matched {
                found = Some(m);
            }
        });
        found
    }

    fn rel(&self, r: Rel) -> Rel {
        let bx = |x: Box<Rel>| Box::new(self.rel(*x));
        match r {
            Rel::Empty | Rel::Scan { .. } | Rel::TempScan { .. } | Rel::ParamScan { .. } => r,
            Rel::Slice(mut v) => {
                let [a, b] = std::mem::take(&mut v.bounds);
                v.bounds = [a.map(|e| self.rekey(e)), b.map(|e| self.rekey(e))];
                v.cond = v.cond.take().map(|e| self.rekey(e));
                v.ext_dims_arg = v.ext_dims_arg.take().map(|e| self.rekey(e));
                v.order = v.order.take().map(|k| SortKey {
                    expr: self.rekey(k.expr),
                    desc: k.desc,
                });
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
                v.ext_dims_arg = v.ext_dims_arg.take().map(|e| self.rekey(e));
                v.order = v.order.take().map(|k| SortKey {
                    expr: self.rekey(k.expr),
                    desc: k.desc,
                });
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
                v.ext_dims_arg = v.ext_dims_arg.take().map(|e| self.rekey(e));
                v.order = v.order.take().map(|k| SortKey {
                    expr: self.rekey(k.expr),
                    desc: k.desc,
                });
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
                if usize::from(i) >= self.explicit_keys {
                    let ty = e.ty.clone();
                    return Expr {
                        kind: ExprKind::LocalKey {
                            key: i,
                            orig: Box::new(e),
                        },
                        ty,
                    };
                }
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
        let prefix = self.find_prefix(&e);
        let Expr { kind, ty } = e;
        let kind = match kind {
            ExprKind::Deref { base, mut steps } => {
                if let Some((i, len)) = prefix
                    && let Some(key) = self.keys.get(usize::from(i))
                    && (!self.in_sub || (self.anchor.is_some() && reads_own(key, self.own)))
                {
                    let suffix = steps.split_off(len);
                    let key_ty = key.ty.clone();
                    let orig = if len == 0 {
                        base
                    } else {
                        Box::new(Expr {
                            kind: ExprKind::Deref { base, steps },
                            ty: key_ty.clone(),
                        })
                    };
                    let kind = if let (true, Some(src)) = (self.in_sub, self.anchor) {
                        ExprKind::OuterKey { src, key: i, orig }
                    } else {
                        ExprKind::LocalKey { key: i, orig }
                    };
                    ExprKind::Deref {
                        base: Box::new(Expr { kind, ty: key_ty }),
                        steps: suffix,
                    }
                } else {
                    ExprKind::Deref {
                        base: self.bx(base),
                        steps,
                    }
                }
            }
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
