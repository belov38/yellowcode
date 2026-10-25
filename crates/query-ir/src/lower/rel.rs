use super::deref::{Chain, NodeKey, Plan, Sf};
use super::expr::true_lit;
use super::layout::{Offsets, R, Row, fatal, internal, later, p16};
use super::{AggL, HierSeeds, HierTable, NestedL, NestedRowsL, PExpr, RelL};
use crate::bind::ValueRows;
use crate::bind::{Bound, visit_expr, visit_stmt};
use crate::ir::{ColSel, Expr, ExprKind, NestedRows, Rel, SrcId, SrcKind, StmtS};
use crate::schema::{QFieldId, QTableId, QuerySchema, StorageBinding, TableKind};
use crate::value::QValue;
use ibschema::names::StdField;
use ibschema::storage::{StorageMap, TableId};
use query_syntax::ast::{CmpOp, JoinKind};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub(super) struct Lower<'a> {
    pub schema: &'a QuerySchema,
    pub storage: &'a StorageBinding,
    pub map: &'a StorageMap,
    pub b: &'a Bound<'a>,
    pub predef_names: HashMap<QTableId, Vec<([u8; 16], String)>>,
    pub predef_wanted: Vec<QTableId>,
    pub stmt: &'a StmtS,
    pub plans: HashMap<(QTableId, QFieldId), Rc<Plan>>,
    pub scan_cols: Vec<Vec<Sf>>,
    pub scan_idx: Vec<HashMap<Sf, u16>>,
    pub offs: Offsets,
    pub src_level: Vec<u32>,
    pub param_srcs: Vec<SrcId>,
    pub level: u32,
    pub min_ref: Vec<u32>,
    pub nodes: HashMap<NodeKey, u32>,
    pub next_node: u32,
    pub chains: HashMap<u32, Chain>,
    pub chain_log: Option<Vec<(u32, Option<Chain>)>>,
    pub frozen: bool,
    pub added: u32,
    pub agg_decs: Vec<super::Dec>,
    pub inner: u32,
}

const LEFT: u8 = 1;
const RIGHT: u8 = 2;

struct Semi<'e> {
    l: &'e [Expr],
    not: bool,
    src: SemiSrc<'e>,
}

enum SemiSrc<'e> {
    Query(&'e Rel),
    HierList(&'e [Expr]),
    HierQuery(&'e Rel),
}

impl<'a> Lower<'a> {
    pub fn new(
        schema: &'a QuerySchema,
        storage: &'a StorageBinding,
        map: &'a StorageMap,
        b: &'a Bound<'a>,
        predef_names: HashMap<QTableId, Vec<([u8; 16], String)>>,
    ) -> Lower<'a> {
        let n = b.stmt.sources.len();
        Lower {
            schema,
            storage,
            map,
            b,
            predef_names,
            predef_wanted: Vec::new(),
            stmt: b.stmt,
            plans: HashMap::new(),
            scan_cols: vec![Vec::new(); n],
            scan_idx: vec![HashMap::new(); n],
            offs: Offsets::new(n),
            src_level: vec![u32::MAX; n],
            param_srcs: Vec::new(),
            level: 0,
            min_ref: vec![0],
            nodes: HashMap::new(),
            next_node: 0,
            chains: HashMap::new(),
            chain_log: None,
            frozen: false,
            added: 0,
            agg_decs: Vec::new(),
            inner: 0,
        }
    }

    pub fn added_depth(&self) -> u32 {
        self.added
    }

    pub fn run(&mut self) -> R<RelL> {
        self.collect_scan_columns()?;
        let stmt = self.stmt;
        Ok(self.rel(&stmt.root)?.rel)
    }

    pub fn run_nested(&mut self) -> R<Vec<NestedL>> {
        let stmt = self.stmt;
        let mut out = Vec::with_capacity(stmt.nested.len());
        for nt in &stmt.nested {
            let Some(columns) = stmt
                .columns
                .get(usize::from(nt.col))
                .and_then(|c| c.nested.clone())
            else {
                return internal("a nested table over a column without nested columns");
            };
            let rows = match &nt.rows {
                None => None,
                Some(r) => Some(self.nested_rows(r)?),
            };
            out.push(NestedL {
                col: nt.col,
                columns,
                rows,
            });
        }
        Ok(out)
    }

    fn nested_rows(&mut self, r: &NestedRows) -> R<NestedRowsL> {
        let mut row = self.rel(&r.scan)?;
        let PExpr::Col(ref_col) = self.expr(&r.owner, &mut row)? else {
            return internal("the owner of a tabular section is not a column");
        };
        let w = row.width;
        let owners = Arc::new(ValueRows {
            width: 1,
            rows: Vec::new(),
        });
        let on = PExpr::Compare(
            CmpOp::Eq,
            Box::new(PExpr::Col(ref_col)),
            Box::new(PExpr::Col(p16(w as i64)?)),
        );
        self.added += 1;
        row.rel = RelL::Join {
            kind: JoinKind::Inner,
            l: row.take(),
            r: Box::new(RelL::Values {
                rows: owners.clone(),
            }),
            on,
            eq: vec![(PExpr::Col(ref_col), PExpr::Col(0))],
            by_id: false,
        };
        row.width = w + 1;
        let mut exprs = vec![PExpr::Col(ref_col), self.expr(&r.number, &mut row)?];
        for e in &r.exprs {
            exprs.push(self.expr(e, &mut row)?);
        }
        Ok(NestedRowsL {
            rel: RelL::Project {
                input: row.take(),
                exprs,
            },
            owners,
        })
    }

    fn collect_scan_columns(&mut self) -> R<()> {
        let stmt = self.stmt;
        let mut err = None;
        visit_stmt(stmt, &mut |e: &Expr, _| {
            if err.is_some() {
                return;
            }
            if let ExprKind::Col {
                src,
                col: ColSel::Field(f),
            } = &e.kind
                && let Err(x) = self.need(*src, *f)
            {
                err = Some(x);
            }
            if let ExprKind::Deref { base, .. } = &e.kind
                && let ExprKind::Col { src, .. } = &base.kind
                && let Err(x) = self.need_separators(*src)
            {
                err = Some(x);
            }
        });
        err.map_or(Ok(()), Err)
    }

    pub(super) fn separators_of(&self, src: SrcId) -> Vec<Sf> {
        let Some(SrcKind::Table(qt)) = self.stmt.sources.get(src.0 as usize).map(|s| &s.kind)
        else {
            return Vec::new();
        };
        let Some(tid) = self.storage.table(*qt) else {
            return Vec::new();
        };
        self.map
            .try_table(tid)
            .map(|t| t.separators.iter().map(|s| (tid, s.field)).collect())
            .unwrap_or_default()
    }

    fn need_separators(&mut self, src: SrcId) -> R<()> {
        let seps = self.separators_of(src);
        let (Some(list), Some(idx)) = (
            self.scan_cols.get_mut(src.0 as usize),
            self.scan_idx.get_mut(src.0 as usize),
        ) else {
            return Ok(());
        };
        for c in seps {
            if let std::collections::hash_map::Entry::Vacant(v) = idx.entry(c) {
                let Ok(i) = u16::try_from(list.len()) else {
                    return fatal("слишком много колонок источника");
                };
                v.insert(i);
                list.push(c);
            }
        }
        Ok(())
    }

    fn need(&mut self, src: SrcId, f: QFieldId) -> R<()> {
        let Some(SrcKind::Table(qt)) = self.stmt.sources.get(src.0 as usize).map(|s| &s.kind)
        else {
            return internal("a field of a source that is not a table");
        };
        let plan = self.plan(*qt, f)?;
        let mut cols = Vec::new();
        plan.needs(&mut cols);
        let (Some(list), Some(idx)) = (
            self.scan_cols.get_mut(src.0 as usize),
            self.scan_idx.get_mut(src.0 as usize),
        ) else {
            return internal("a source out of range");
        };
        for c in cols {
            if let std::collections::hash_map::Entry::Vacant(v) = idx.entry(c) {
                let Ok(i) = u16::try_from(list.len()) else {
                    return fatal("слишком много колонок источника");
                };
                v.insert(i);
                list.push(c);
            }
        }
        Ok(())
    }

    pub(super) fn rel(&mut self, r: &Rel) -> R<Row> {
        match r {
            Rel::Empty => Ok(Row::empty()),
            Rel::Scan { src, table } => self.scan(*src, *table),
            Rel::TempScan { src, name } => self.temp_scan(*src, name),
            Rel::ParamScan { src, param } => self.param_scan(*src, *param),
            Rel::Subquery { src, input } => {
                self.inner += 1;
                let inner = self.rel(input);
                self.inner -= 1;
                let inner = inner?;
                self.set_level(*src);
                Ok(Row {
                    rel: inner.rel,
                    width: inner.width,
                    rep: Some(*src),
                    keys: None,
                })
            }
            Rel::Join { kind, l, r, on } => self.join(*kind, l, r, on),
            Rel::Filter { input, pred } => self.filter(input, pred),
            Rel::Aggregate {
                input,
                keys,
                sets,
                aggs,
            } => {
                let mut row = self.rel(input)?;
                let k = self.exprs(keys, &mut row)?;
                let mut a = Vec::with_capacity(aggs.len());
                let mut decs = Vec::with_capacity(aggs.len());
                for x in aggs {
                    let arg = match &x.arg {
                        Some(e) => Some(self.expr(e, &mut row)?),
                        None => None,
                    };
                    let arg_dec = match (&x.arg, &arg) {
                        (Some(e), Some(p)) => self.dec_of_expr(e, p),
                        _ => super::Dec::UNKNOWN,
                    };
                    decs.push(super::agg_dec(x.f, arg_dec));
                    a.push(AggL {
                        f: x.f,
                        distinct: x.distinct,
                        arg,
                        ty: x.ty.clone(),
                        arg_dec,
                    });
                }
                self.agg_decs = decs;
                let width = k.len() + a.len();
                let nkeys = k.len();
                Ok(Row {
                    rel: RelL::Aggregate {
                        input: row.take(),
                        keys: k,
                        sets: sets.clone(),
                        aggs: a,
                    },
                    width,
                    rep: None,
                    keys: Some(nkeys),
                })
            }
            Rel::Project { input, exprs } => {
                let mut row = self.rel(input)?;
                let mut ps = Vec::with_capacity(exprs.len());
                for e in exprs {
                    let code = if self.inner == 0 {
                        self.type_code_param(e)?
                    } else {
                        None
                    };
                    ps.push(match code {
                        Some(code) => code,
                        None => self.expr(e, &mut row)?,
                    });
                }
                Ok(Row {
                    width: ps.len(),
                    rel: RelL::Project {
                        input: row.take(),
                        exprs: ps,
                    },
                    rep: None,
                    keys: None,
                })
            }
            Rel::Distinct(x) => {
                let mut row = self.rel(x)?;
                row.rel = RelL::Distinct(row.take());
                Ok(row)
            }
            Rel::Sort { input, keys } => {
                let mut row = self.rel(input)?;
                let mut ks = Vec::with_capacity(keys.len());
                for k in keys {
                    ks.push((self.expr(&k.expr, &mut row)?, k.desc));
                }
                row.rel = RelL::Sort {
                    input: row.take(),
                    keys: ks,
                };
                Ok(row)
            }
            Rel::Hierarchy {
                input,
                id,
                parent,
                keys,
            } => {
                let mut row = self.rel(input)?;
                if usize::from(*id) >= row.width || usize::from(*parent) >= row.width {
                    return internal("hierarchy column out of range");
                }
                let mut ks = Vec::with_capacity(keys.len());
                for k in keys {
                    ks.push((self.expr(&k.expr, &mut row)?, k.desc));
                }
                row.rel = RelL::HierarchyOrder {
                    input: row.take(),
                    id: *id,
                    parent: *parent,
                    keys: ks,
                };
                Ok(row)
            }
            Rel::Top { input, n } => {
                let mut row = self.rel(input)?;
                row.rel = RelL::Top {
                    input: row.take(),
                    n: *n,
                };
                Ok(row)
            }
            Rel::Union { parts, all } => {
                let mut rels = Vec::with_capacity(parts.len());
                let mut width = 0;
                for (i, p) in parts.iter().enumerate() {
                    let row = self.rel(p)?;
                    if i == 0 {
                        width = row.width;
                    }
                    rels.push(row.rel);
                }
                Ok(Row {
                    rel: RelL::Union {
                        parts: rels,
                        all: all.clone(),
                    },
                    width,
                    rep: None,
                    keys: None,
                })
            }
        }
    }

    fn type_code_param(&self, e: &Expr) -> R<Option<PExpr>> {
        let ExprKind::Param(i) = &e.kind else {
            return Ok(None);
        };
        let Some(crate::bind::PBound::Value(QValue::Type(t))) = self.b.params.get(usize::from(*i))
        else {
            return Ok(None);
        };
        let Some(code) = self.type_code(t) else {
            return later("тип-параметр в поле, код типа неизвестен", "Q3");
        };
        Ok(Some(PExpr::Lit(QValue::Binary(
            Arc::<[u8]>::from(code).into(),
        ))))
    }

    fn type_code(&self, t: &crate::QType) -> Option<Vec<u8>> {
        let short = |c: u8| Some(vec![c, 0, 0, 0, 0]);
        let atoms = usize::from(t.null)
            + usize::from(t.boolean)
            + usize::from(t.number.is_some())
            + usize::from(t.string.is_some())
            + usize::from(t.date.is_some())
            + t.refs.len()
            + t.others.len();
        if atoms > 1 {
            return None;
        }
        if atoms == 0 {
            return short(1);
        }
        if t.null {
            return short(0);
        }
        if t.boolean {
            return short(2);
        }
        if t.number.is_some() {
            return short(3);
        }
        if t.date.is_some() {
            return short(4);
        }
        if t.string.is_some() {
            return short(5);
        }
        if let [r] = t.refs.as_slice() {
            let mut code = vec![8u8];
            code.extend(self.storage.ref_number(*r)?.to_be_bytes());
            return Some(code);
        }
        let (g, _) = self.schema.atom_uuid(t).into_iter().next()?;
        let mut code = vec![0xFFu8];
        code.extend(g.to_bytes_le());
        Some(code)
    }

    fn set_level(&mut self, src: SrcId) {
        if let Some(l) = self.src_level.get_mut(src.0 as usize) {
            *l = self.level;
        }
    }

    fn temp_scan(&mut self, src: SrcId, name: &str) -> R<Row> {
        self.set_level(src);
        let Some(SrcKind::Temp { columns, .. }) =
            self.stmt.sources.get(src.0 as usize).map(|s| &s.kind)
        else {
            return internal("a temporary table scan of a source that is not one");
        };
        let width = p16(columns.len() as i64)?;
        Ok(Row {
            width: usize::from(width),
            rel: RelL::TempScan {
                name: bsl_prim::fold_name(name),
                width,
            },
            rep: Some(src),
            keys: None,
        })
    }

    fn param_scan(&mut self, src: SrcId, param: u16) -> R<Row> {
        self.set_level(src);
        self.param_srcs.push(src);
        let Some(crate::bind::PBound::Rows(rows)) = self.b.params.get(usize::from(param)) else {
            return internal("a value table scan of a parameter that is not one");
        };
        Ok(Row {
            width: rows.width,
            rel: RelL::Values { rows: rows.clone() },
            rep: Some(src),
            keys: None,
        })
    }

    fn scan(&mut self, src: SrcId, table: QTableId) -> R<Row> {
        self.set_level(src);
        let t = self.schema.table(table);
        let cols = self
            .scan_cols
            .get(src.0 as usize)
            .cloned()
            .unwrap_or_default();
        if t.kind == TableKind::Constants {
            return self.constants(src, cols);
        }
        if self.schema.is_unstored(table) {
            return Ok(Row {
                width: 0,
                rel: RelL::Top {
                    input: Box::new(RelL::Empty),
                    n: 0,
                },
                rep: Some(src),
                keys: None,
            });
        }
        let Some(tid) = self.storage.table(table) else {
            return fatal(format!("таблица {} не привязана к хранению", t.name_ru));
        };
        Ok(Row {
            width: cols.len(),
            rel: RelL::Scan {
                table: tid,
                fields: cols.iter().map(|c| c.1).collect(),
            },
            rep: Some(src),
            keys: None,
        })
    }

    fn constants(&mut self, src: SrcId, cols: Vec<Sf>) -> R<Row> {
        let mut order: Vec<TableId> = Vec::new();
        let mut by_table: HashMap<TableId, Vec<Sf>> = HashMap::new();
        for c in cols {
            if !by_table.contains_key(&c.0) {
                order.push(c.0);
            }
            by_table.entry(c.0).or_default().push(c);
        }
        let mut rel = RelL::Empty;
        let mut flat: Vec<Sf> = Vec::new();
        for t in order {
            let cs = by_table.remove(&t).unwrap_or_default();
            rel = RelL::Join {
                kind: JoinKind::Left,
                l: Box::new(rel),
                r: Box::new(RelL::Scan {
                    table: t,
                    fields: cs.iter().map(|c| c.1).collect(),
                }),
                on: true_lit(),
                eq: Vec::new(),
                by_id: false,
            };
            self.added += 1;
            flat.extend(cs);
        }
        let mut idx = HashMap::with_capacity(flat.len());
        for (i, c) in flat.iter().enumerate() {
            let Ok(i) = u16::try_from(i) else {
                return fatal("слишком много колонок источника");
            };
            idx.insert(*c, i);
        }
        if let Some(m) = self.scan_idx.get_mut(src.0 as usize) {
            *m = idx;
        }
        Ok(Row {
            width: flat.len(),
            rel,
            rep: Some(src),
            keys: None,
        })
    }

    fn side_mask(&mut self, e: &Expr, l: SrcId, r: SrcId) -> R<u8> {
        let mut mask = 0u8;
        let mut bad = false;
        visit_expr(e, false, &mut |x: &Expr, _| {
            if let ExprKind::Col { src, .. } = &x.kind {
                if self.src_level.get(src.0 as usize).copied() != Some(self.level) {
                    return;
                }
                if self.offs.same(*src, l) {
                    mask |= LEFT;
                } else if self.offs.same(*src, r) {
                    mask |= RIGHT;
                } else {
                    bad = true;
                }
            }
        });
        if bad {
            return internal("a column of ПО outside both inputs");
        }
        Ok(mask)
    }

    fn join(&mut self, kind: JoinKind, l: &Rel, r: &Rel, on: &Expr) -> R<Row> {
        let mut lr = self.rel(l)?;
        let mut rr = self.rel(r)?;
        let (Some(lrep), Some(rrep)) = (lr.rep, rr.rep) else {
            return internal("a join input without a source");
        };
        let mut derefs: Vec<&Expr> = Vec::new();
        crate::ir::visit_expr_in_place(on, false, &mut |x| {
            if matches!(x.kind, ExprKind::Deref { .. }) {
                derefs.push(x);
            }
        });
        let mut cross: Vec<&Expr> = Vec::new();
        for d in derefs {
            let ExprKind::Deref { base, steps } = &d.kind else {
                continue;
            };
            let side = self.side_mask(base, lrep, rrep)?;
            if side == LEFT | RIGHT {
                if kind != JoinKind::Inner {
                    return later("разыменование выражения из двух источников в ПО", "Q3");
                }
                cross.push(d);
                continue;
            }
            let row = if side == RIGHT { &mut rr } else { &mut lr };
            self.deref_chain(row, base, steps)?;
        }
        let (keep, post): (Vec<&Expr>, Vec<&Expr>) = if cross.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            conjuncts(on).into_iter().partition(|c| {
                let mut hit = false;
                visit_expr(c, false, &mut |x, _| {
                    hit |= cross.iter().any(|d| std::ptr::eq(*d, x));
                });
                !hit
            })
        };
        self.frozen = true;
        let res = if cross.is_empty() {
            self.join_fixed(kind, on, lr, rr, lrep, rrep)
        } else {
            let rest = Expr {
                kind: match keep.as_slice() {
                    [] => ExprKind::Lit(QValue::Bool(true)),
                    [one] => one.kind.clone(),
                    many => ExprKind::And(many.iter().map(|c| (*c).clone()).collect()),
                },
                ty: on.ty.clone(),
            };
            self.join_fixed(kind, &rest, lr, rr, lrep, rrep)
        };
        self.frozen = false;
        let mut row = res?;
        if cross.is_empty() {
            return Ok(row);
        }
        for d in &cross {
            if let ExprKind::Deref { base, steps } = &d.kind {
                self.deref_chain(&mut row, base, steps)?;
            }
        }
        let mut ps = Vec::with_capacity(post.len());
        for c in post {
            ps.push(self.expr(c, &mut row)?);
        }
        let pred = if ps.len() == 1 {
            ps.pop().unwrap_or_else(true_lit)
        } else {
            PExpr::And(ps)
        };
        row.rel = RelL::Filter {
            input: row.take(),
            pred,
        };
        Ok(row)
    }

    fn join_fixed(
        &mut self,
        kind: JoinKind,
        on: &Expr,
        lr: Row,
        rr: Row,
        lrep: SrcId,
        rrep: SrcId,
    ) -> R<Row> {
        let (mut lr, mut rr) = (lr, rr);
        let mut raw: Vec<(PExpr, PExpr)> = Vec::new();
        for c in conjuncts(on) {
            let ExprKind::Compare(CmpOp::Eq, a, b) = &c.kind else {
                continue;
            };
            if has_in_query(a) || has_in_query(b) {
                continue;
            }
            let (ma, mb) = (
                self.side_mask(a, lrep, rrep)?,
                self.side_mask(b, lrep, rrep)?,
            );
            if ma == LEFT && mb == RIGHT {
                raw.push((self.expr(a, &mut lr)?, self.expr(b, &mut rr)?));
            } else if ma == RIGHT && mb == LEFT {
                raw.push((self.expr(b, &mut lr)?, self.expr(a, &mut rr)?));
            }
        }
        let swap = kind == JoinKind::Right;
        let root = self.offs.merge(lrep, lr.width, rrep, rr.width, !swap);
        let width = lr.width + rr.width;
        let (first, second, eq) = if swap {
            (rr, lr, raw.into_iter().map(|(a, b)| (b, a)).collect())
        } else {
            (lr, rr, raw)
        };
        let mut row = Row {
            rel: RelL::Empty,
            width,
            rep: Some(root),
            keys: None,
        };
        let on = self.expr(on, &mut row)?;
        let (mut first, mut second) = (first, second);
        row.rel = RelL::Join {
            kind: if swap { JoinKind::Left } else { kind },
            l: first.take(),
            r: second.take(),
            on,
            eq,
            by_id: false,
        };
        Ok(row)
    }

    fn filter(&mut self, input: &Rel, pred: &Expr) -> R<Row> {
        let mut row = self.rel(input)?;
        let mut plain: Vec<&Expr> = Vec::new();
        let mut semis: Vec<(Semi<'_>, Vec<HierTable>)> = Vec::new();
        for c in conjuncts(pred) {
            match as_semi(c) {
                Some(s) => {
                    let tables = match s.src {
                        SemiSrc::Query(_) => Vec::new(),
                        _ => self.hier_tables(s.l),
                    };
                    match s.src {
                        SemiSrc::HierList(_) if tables.is_empty() => plain.push(c),
                        SemiSrc::HierQuery(q) if tables.is_empty() => semis.push((
                            Semi {
                                src: SemiSrc::Query(q),
                                ..s
                            },
                            tables,
                        )),
                        _ => semis.push((s, tables)),
                    }
                }
                None => plain.push(c),
            }
        }
        if !plain.is_empty() {
            let mut ps = Vec::with_capacity(plain.len());
            for c in plain {
                ps.push(self.expr(c, &mut row)?);
            }
            let pred = if ps.len() == 1 {
                ps.pop().unwrap_or_else(true_lit)
            } else {
                PExpr::And(ps)
            };
            row.rel = RelL::Filter {
                input: row.take(),
                pred,
            };
        }
        for (s, tables) in semis {
            let l = self.exprs(s.l, &mut row)?;
            let (q, correlated) = match s.src {
                SemiSrc::Query(q) => self.semi_query(q)?,
                SemiSrc::HierQuery(q) => {
                    let (q, correlated) = self.semi_query(q)?;
                    let seeds = HierSeeds::Rel(Box::new(q));
                    (RelL::HierarchyClosure { tables, seeds }, correlated)
                }
                SemiSrc::HierList(rhs) => {
                    let mut seeds = Vec::with_capacity(rhs.len());
                    for x in rhs {
                        let p = self.rhs_item(x, &mut row)?;
                        if !matches!(p, PExpr::Lit(_) | PExpr::Set(_)) {
                            return later(
                                "В ИЕРАРХИИ (выражение вместо параметра или значения)",
                                "Q3",
                            );
                        }
                        seeds.push(p);
                    }
                    let seeds = HierSeeds::Values(seeds);
                    (RelL::HierarchyClosure { tables, seeds }, false)
                }
            };
            self.added += 1;
            row.rel = RelL::SemiJoin {
                input: row.take(),
                l,
                q: Box::new(q),
                not: s.not,
                correlated,
            };
        }
        Ok(row)
    }
}

impl Lower<'_> {
    pub(super) fn expr_query(&mut self, q: &Rel) -> R<(RelL, bool)> {
        let frozen = std::mem::replace(&mut self.frozen, false);
        let outer_log = self.chain_log.replace(Vec::new());
        let res = self.semi_query(q);
        let log = std::mem::replace(&mut self.chain_log, outer_log).unwrap_or_default();
        for (node, old) in log.into_iter().rev() {
            match old {
                Some(c) => {
                    self.chains.insert(node, c);
                }
                None => {
                    self.chains.remove(&node);
                }
            }
        }
        self.frozen = frozen;
        res
    }

    pub(super) fn semi_query(&mut self, q: &Rel) -> R<(RelL, bool)> {
        self.level += 1;
        self.min_ref.push(self.level);
        let outer_decs = self.agg_decs.clone();
        self.inner += 1;
        let q = self.rel(q);
        self.inner -= 1;
        self.agg_decs = outer_decs;
        let correlated = self.min_ref.pop().is_some_and(|m| m < self.level);
        self.level -= 1;
        Ok((q?.rel, correlated))
    }

    pub(super) fn hier_tables(&self, l: &[Expr]) -> Vec<HierTable> {
        let mut out: Vec<HierTable> = Vec::new();
        for atom in l.iter().flat_map(|x| x.ty.refs.iter()) {
            let Some(qt) = self.schema.hierarchical_table(*atom) else {
                continue;
            };
            let (Some(number), Some(table)) =
                (self.storage.ref_number(*atom), self.storage.table(qt))
            else {
                continue;
            };
            if self.schema.is_unstored(qt) || out.iter().any(|t| t.number == number) {
                continue;
            }
            let Some(info) = self.map.try_table(table) else {
                continue;
            };
            if let (Some(id), Some(parent)) = (
                info.field_by_std(StdField::Id),
                info.field_by_std(StdField::ParentId),
            ) {
                out.push(HierTable {
                    number,
                    table,
                    id,
                    parent,
                });
            }
        }
        out
    }
}

fn has_in_query(e: &Expr) -> bool {
    let mut hit = false;
    visit_expr(e, false, &mut |x, _| {
        hit |= matches!(x.kind, ExprKind::InQuery { .. });
    });
    hit
}

fn conjuncts(e: &Expr) -> Vec<&Expr> {
    let mut out = Vec::new();
    let mut stack = vec![e];
    while let Some(x) = stack.pop() {
        match &x.kind {
            ExprKind::And(v) => stack.extend(v.iter().rev()),
            _ => out.push(x),
        }
    }
    out
}

fn as_semi(mut e: &Expr) -> Option<Semi<'_>> {
    let mut flip = false;
    loop {
        match &e.kind {
            ExprKind::Not(x) => {
                flip = !flip;
                e = x;
            }
            ExprKind::InQuery {
                l,
                not,
                hierarchy,
                q,
            } => {
                return Some(Semi {
                    l,
                    not: *not != flip,
                    src: if *hierarchy {
                        SemiSrc::HierQuery(q)
                    } else {
                        SemiSrc::Query(q)
                    },
                });
            }
            ExprKind::InList {
                l,
                not,
                hierarchy: true,
                rhs,
            } => {
                return Some(Semi {
                    l,
                    not: *not != flip,
                    src: SemiSrc::HierList(rhs),
                });
            }
            _ => return None,
        }
    }
}
