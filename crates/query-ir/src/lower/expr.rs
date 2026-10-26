use super::dec::{ArithDec, Dec, MAX_DIGITS, dec_of, dec_op_k, opd_of};
use super::deref::{Sf, plan_expr};
use super::layout::{R, Row, fatal, internal, later, p16};
use super::rel::Lower;
use super::{PExpr, SizeKind};
use crate::bind::{PBound, fold_chain};
use crate::ir::{ColSel, Expr, ExprKind, SrcId, SrcKind};
use crate::types::QType;
use crate::value::QValue;
use query_syntax::Func;
use query_syntax::ast::BinOp;
use std::sync::Arc;

impl Lower<'_> {
    fn boxed(&mut self, e: &Expr, row: &mut Row) -> R<Box<PExpr>> {
        Ok(Box::new(self.expr(e, row)?))
    }

    pub(super) fn exprs(&mut self, v: &[Expr], row: &mut Row) -> R<Vec<PExpr>> {
        let mut out = Vec::with_capacity(v.len());
        for e in v {
            out.push(self.expr(e, row)?);
        }
        Ok(out)
    }

    pub(super) fn rhs_item(&mut self, e: &Expr, row: &mut Row) -> R<PExpr> {
        if let ExprKind::Param(i) = &e.kind {
            match self.b.params.get(usize::from(*i)) {
                Some(PBound::Set(s)) => return Ok(PExpr::Set(s.clone())),
                Some(PBound::Rows(r)) if r.width == 1 => {
                    return Ok(PExpr::Set(Arc::new(crate::ValueSet {
                        values: r
                            .rows
                            .iter()
                            .filter_map(|x| x.first())
                            .filter(|v| **v != QValue::Null)
                            .cloned()
                            .collect(),
                    })));
                }
                _ => {}
            }
        }
        self.expr(e, row)
    }

    fn tuple_rows(&self, e: &Expr, k: usize) -> R<Option<Vec<PExpr>>> {
        let ExprKind::Param(i) = &e.kind else {
            return Ok(None);
        };
        let Some(PBound::Rows(r)) = self.b.params.get(usize::from(*i)) else {
            return Ok(None);
        };
        if r.width != k {
            return fatal("таблица значений в В (..) не по ширине кортежа");
        }
        let mut out = Vec::new();
        for x in &r.rows {
            if x.contains(&QValue::Null) {
                continue;
            }
            out.extend(x.iter().cloned().map(PExpr::Lit));
        }
        Ok(Some(out))
    }

    pub(super) fn expr(&mut self, e: &Expr, row: &mut Row) -> R<PExpr> {
        if !matches!(
            e.kind,
            ExprKind::Neg(_) | ExprKind::Not(_) | ExprKind::Compare(..)
        ) {
            return self.expr_node(e, row);
        }
        let mut spine = vec![e];
        let mut cur = e;
        while let ExprKind::Neg(x) | ExprKind::Not(x) | ExprKind::Compare(_, x, _) = &cur.kind {
            spine.push(x);
            cur = x;
        }
        let innermost = spine.pop().unwrap_or(e);
        let mut acc = self.expr_node(innermost, row)?;
        for node in spine.iter().rev() {
            acc = match &node.kind {
                ExprKind::Neg(_) => {
                    let x = Box::new(acc);
                    match &*x {
                        PExpr::Lit(v) => match crate::fold::neg(v) {
                            Some(n) => PExpr::Lit(n),
                            None => PExpr::Neg(x),
                        },
                        _ => PExpr::Neg(x),
                    }
                }
                ExprKind::Not(_) => PExpr::Not(Box::new(acc)),
                ExprKind::Compare(op, _, b) => {
                    PExpr::Compare(*op, Box::new(acc), self.boxed(b, row)?)
                }
                _ => return internal("a spine node is neither a unary operator nor a comparison"),
            };
        }
        Ok(acc)
    }

    fn expr_node(&mut self, e: &Expr, row: &mut Row) -> R<PExpr> {
        Ok(match &e.kind {
            ExprKind::Lit(v) => PExpr::Lit(v.clone()),
            ExprKind::Param(i) => match self.b.params.get(usize::from(*i)) {
                Some(PBound::Value(v)) => PExpr::Lit(v.clone()),
                Some(PBound::Set(_) | PBound::Rows(_)) => {
                    return fatal("параметр-массив вне В (..)");
                }
                None => return internal("a parameter slot is not bound"),
            },
            ExprKind::Value(i) => match self.b.values.get(usize::from(*i)) {
                Some(v) => PExpr::Lit(v.clone()),
                None => return internal("a ЗНАЧЕНИЕ slot is not bound"),
            },
            ExprKind::Col { src, col } => self.col(*src, *col)?,
            ExprKind::Deref { base, steps } => self.deref_chain(row, base, steps)?,
            ExprKind::KeyRef(i) => {
                if row.keys.is_none() {
                    return internal("a group key outside the rows of an aggregate");
                }
                PExpr::Col(*i)
            }
            ExprKind::AggRef(i) => match row.keys {
                Some(n) => PExpr::Col(p16(n as i64 + i64::from(*i))?),
                None => return internal("an aggregate outside the rows of an aggregate"),
            },
            ExprKind::Out(i) => PExpr::Col(*i),
            ExprKind::Neg(_) | ExprKind::Not(_) | ExprKind::Compare(..) => {
                return internal("a unary operator or a comparison outside the spine walk");
            }
            ExprKind::Arith {
                first: first_ir,
                steps,
            } => {
                let first = self.boxed(first_ir, row)?;
                let mut out = Vec::with_capacity(steps.len());
                for (op, rhs, ty) in steps {
                    out.push((*op, self.expr(rhs, row)?, ty.clone()));
                }
                match fold_lits(&first, &out)? {
                    Some(v) => PExpr::Lit(v),
                    None => {
                        let first_dec = self.dec_of_expr(first_ir, &first);
                        let mut acc = first_dec;
                        let mut lk = opd_of(first_ir);
                        let mut decs = Vec::with_capacity(out.len());
                        for ((op, rhs, _), (_, rhs_ir, _)) in out.iter().zip(steps) {
                            let rhs = self.dec_of_expr(rhs_ir, rhs);
                            let rk = opd_of(rhs_ir);
                            acc = dec_op_k(*op, acc, lk, rhs, rk);
                            decs.push(ArithDec {
                                rhs,
                                out: acc,
                                lk,
                                rk,
                            });
                            lk = lk.join(rk);
                        }
                        PExpr::Arith {
                            first,
                            first_dec,
                            steps: out,
                            decs,
                        }
                    }
                }
            }
            ExprKind::And(v) => PExpr::And(self.exprs(v, row)?),
            ExprKind::Or(v) => PExpr::Or(self.exprs(v, row)?),
            ExprKind::InList {
                l,
                not,
                hierarchy,
                rhs,
            } => {
                if *hierarchy && !self.hier_tables(l).is_empty() {
                    return later(
                        "В ИЕРАРХИИ допускается только конъюнктом условия ГДЕ или ИМЕЮЩИЕ",
                        "Q3",
                    );
                }
                let k = l.len();
                let l = self.exprs(l, row)?;
                let mut items = Vec::with_capacity(rhs.len());
                if let ([x], true) = (rhs.as_slice(), k > 1)
                    && let Some(rows) = self.tuple_rows(x, k)?
                {
                    let exists = PExpr::Case {
                        operand: None,
                        whens: vec![(
                            PExpr::InList {
                                l,
                                not: false,
                                rhs: rows,
                            },
                            PExpr::Lit(QValue::Bool(true)),
                        )],
                        else_: Some(Box::new(PExpr::Lit(QValue::Bool(false)))),
                    };
                    return Ok(if *not {
                        PExpr::Not(Box::new(exists))
                    } else {
                        exists
                    });
                } else {
                    for x in rhs {
                        items.push(self.rhs_item(x, row)?);
                    }
                }
                PExpr::InList {
                    l,
                    not: *not,
                    rhs: items,
                }
            }
            ExprKind::InQuery {
                l,
                not,
                hierarchy,
                q,
            } => {
                if *hierarchy && !self.hier_tables(l).is_empty() {
                    return later(
                        "В ИЕРАРХИИ допускается только конъюнктом условия ГДЕ или ИМЕЮЩИЕ",
                        "Q3",
                    );
                }
                let l = self.exprs(l, row)?;
                let (q, correlated) = self.expr_query(q)?;
                self.added += 1;
                PExpr::InQuery {
                    l,
                    not: *not,
                    q: Arc::new(q),
                    correlated,
                }
            }
            ExprKind::SectionAgg { f, owner, rel } => {
                let owner = self.boxed(owner, row)?;
                let (q, _) = self.expr_query(rel)?;
                self.added += 1;
                PExpr::SectionAgg {
                    owner,
                    rel: Arc::new(q),
                    empty: if *f == Func::Count {
                        QValue::Number(bsl_prim::Number::from_i64(0))
                    } else {
                        QValue::Null
                    },
                }
            }
            ExprKind::OuterKey { src, key, .. } => self.outer_key(*src, *key)?,
            ExprKind::Between { l, not, lo, hi } => PExpr::Between {
                l: self.boxed(l, row)?,
                not: *not,
                lo: self.boxed(lo, row)?,
                hi: self.boxed(hi, row)?,
            },
            ExprKind::IsNull { l, not } => PExpr::IsNull {
                l: self.boxed(l, row)?,
                not: *not,
            },
            ExprKind::Refs { l, table } => {
                let number = self
                    .schema
                    .table(*table)
                    .reference
                    .and_then(|r| self.storage.ref_number(r));
                let Some(number) = number else {
                    return fatal(format!(
                        "ССЫЛКА {}: у таблицы нет номера ссылки в хранении",
                        self.schema.table(*table).name_ru
                    ));
                };
                PExpr::Refs {
                    l: self.boxed(l, row)?,
                    number,
                }
            }
            ExprKind::Like { l, not, pat, esc } => PExpr::Like {
                l: self.boxed(l, row)?,
                not: *not,
                pat: self.boxed(pat, row)?,
                esc: *esc,
            },
            ExprKind::Case {
                operand,
                whens,
                else_,
            } => {
                let operand = match operand {
                    Some(o) => Some(self.boxed(o, row)?),
                    None => None,
                };
                let mut out = Vec::with_capacity(whens.len());
                for (c, r) in whens {
                    out.push((self.expr(c, row)?, self.expr(r, row)?));
                }
                let else_ = match else_ {
                    Some(x) => Some(self.boxed(x, row)?),
                    None => None,
                };
                PExpr::Case {
                    operand,
                    whens: out,
                    else_,
                }
            }
            ExprKind::Cast { x, to, num } => PExpr::Cast {
                load: matches!(&x.kind, ExprKind::Col { src, .. } if self.param_srcs.contains(src)),
                x: self.boxed(x, row)?,
                to: to.clone(),
                num: *num,
                ref_tables: to
                    .refs
                    .iter()
                    .filter_map(|r| self.storage.ref_number(*r))
                    .collect(),
            },
            ExprKind::Func {
                f: Func::ValueType,
                args,
            } if matches!(args.as_slice(), [_]) => {
                let Some(a) = args.first() else {
                    return internal("ТИПЗНАЧЕНИЯ without an argument");
                };
                PExpr::TypeOf {
                    x: self.boxed(a, row)?,
                    refs: self.storage.ref_atoms(),
                    route_points: self.storage.route_point_refs(),
                }
            }
            ExprKind::Func {
                f: Func::StoredDataSize,
                args,
            } if matches!(args.as_slice(), [_]) => {
                let Some(a) = args.first() else {
                    return internal("РАЗМЕРХРАНИМЫХДАННЫХ without an argument");
                };
                let t = &a.ty;
                let atoms = usize::from(t.boolean)
                    + usize::from(t.number.is_some())
                    + usize::from(t.string.is_some())
                    + usize::from(t.date.is_some())
                    + t.refs.len()
                    + t.others.len();
                let kind = if atoms > 1 {
                    SizeKind::Composite
                } else if t.number.is_some() {
                    SizeKind::Fixed(40)
                } else if t.date.is_some() {
                    SizeKind::Fixed(8)
                } else if t.boolean {
                    SizeKind::Fixed(1)
                } else if let Some(q) = t.string {
                    if q.fixed {
                        SizeKind::Fixed(2 * q.len)
                    } else {
                        SizeKind::VarString
                    }
                } else {
                    SizeKind::Fixed(16)
                };
                PExpr::StoredSize {
                    x: self.boxed(a, row)?,
                    kind,
                }
            }
            ExprKind::Func { f, args } => {
                let p = self.exprs(args, row)?;
                let arg_dec = match (args.first(), p.first()) {
                    (Some(a), Some(x)) => self.dec_of_expr(a, x),
                    _ => Dec::UNKNOWN,
                };
                PExpr::Func {
                    f: *f,
                    args: p,
                    ty: e.ty.clone(),
                    arg_dec,
                }
            }
            ExprKind::Unit(k) => PExpr::Unit(*k),
        })
    }

    fn col(&mut self, src: SrcId, col: ColSel) -> R<PExpr> {
        let lvl = self
            .src_level
            .get(src.0 as usize)
            .copied()
            .unwrap_or(u32::MAX);
        if lvl > self.level {
            return internal("a column of a source that is not lowered yet");
        }
        let depth = self.level - lvl;
        let base = self.offs.offset(src);
        let outer = if depth == 0 {
            None
        } else {
            Some(self.note_outer(lvl)?)
        };
        let mk = |pos: i64| -> R<PExpr> {
            let col = p16(pos)?;
            Ok(match outer {
                None => PExpr::Col(col),
                Some(depth) => PExpr::Outer { depth, col },
            })
        };
        match col {
            ColSel::Column(i) => mk(base + i64::from(i)),
            ColSel::Field(f)
                if matches!(
                    self.stmt.sources.get(src.0 as usize).map(|s| &s.kind),
                    Some(SrcKind::Virtual { .. })
                ) =>
            {
                let Some(SrcKind::Virtual { cols, .. }) =
                    self.stmt.sources.get(src.0 as usize).map(|s| &s.kind)
                else {
                    return internal("a virtual source");
                };
                let position = match self.slice_cols.get(&src) {
                    Some(fields) => fields.iter().position(|c| *c == f),
                    None => cols.iter().position(|c| *c == Some(f)),
                };
                match position {
                    Some(i) => mk(base + i as i64),
                    None => internal("a field of a virtual table out of its layout"),
                }
            }
            ColSel::Field(f) => {
                let stmt = self.stmt;
                let Some(SrcKind::Table(qt)) = stmt.sources.get(src.0 as usize).map(|s| &s.kind)
                else {
                    return internal("a field of a source that is not a table");
                };
                let plan = self.plan(*qt, f)?;
                let idx = self.scan_idx.get(src.0 as usize);
                let mut at = |sf: Sf| -> R<PExpr> {
                    match idx.and_then(|m| m.get(&sf)) {
                        Some(i) => mk(base + i64::from(*i)),
                        None => internal("a column is not in the scan of its source"),
                    }
                };
                plan_expr(&plan, &mut at)
            }
        }
    }

    pub(super) fn area_of(&mut self, src: SrcId) -> R<Vec<(String, PExpr)>> {
        let seps = self.separators_of(src);
        if seps.is_empty() {
            return Ok(Vec::new());
        }
        let lvl = self
            .src_level
            .get(src.0 as usize)
            .copied()
            .unwrap_or(u32::MAX);
        if lvl > self.level {
            return Ok(Vec::new());
        }
        let depth = self.level - lvl;
        let base = self.offs.offset(src);
        let outer = if depth == 0 {
            None
        } else {
            Some(self.note_outer(lvl)?)
        };
        let mut out = Vec::with_capacity(seps.len());
        for sf in seps {
            let Some(i) = self.scan_idx.get(src.0 as usize).and_then(|m| m.get(&sf)) else {
                return Ok(Vec::new());
            };
            let col = p16(base + i64::from(*i))?;
            let name = self
                .map
                .try_table(sf.0)
                .and_then(|t| t.field(sf.1))
                .map(|f| f.md_name.clone())
                .unwrap_or_default();
            out.push((
                name,
                match outer {
                    None => PExpr::Col(col),
                    Some(depth) => PExpr::Outer { depth, col },
                },
            ));
        }
        Ok(out)
    }

    fn outer_key(&mut self, src: SrcId, key: u16) -> R<PExpr> {
        let lvl = self
            .src_level
            .get(src.0 as usize)
            .copied()
            .unwrap_or(u32::MAX);
        if lvl >= self.level {
            return internal("a key of an aggregate that is not an enclosing one");
        }
        let depth = self.note_outer(lvl)?;
        Ok(PExpr::Outer { depth, col: key })
    }

    fn note_outer(&mut self, target: u32) -> R<u16> {
        let mut lv = self.level;
        while lv > target {
            let Some(m) = self.min_ref.get_mut(lv as usize) else {
                return internal("a level without a record");
            };
            if *m <= target {
                break;
            }
            *m = target;
            lv -= 1;
        }
        match u16::try_from(self.level - target) {
            Ok(d) => Ok(d),
            Err(_) => fatal("слишком глубокая вложенность подзапросов"),
        }
    }
}

fn fold_lits(first: &PExpr, steps: &[(BinOp, PExpr, QType)]) -> R<Option<QValue>> {
    let PExpr::Lit(f) = first else {
        return Ok(None);
    };
    let mut rest = Vec::with_capacity(steps.len());
    for (op, x, _) in steps {
        let PExpr::Lit(v) = x else {
            return Ok(None);
        };
        rest.push((*op, v.clone()));
    }
    fold_chain(f.clone(), &rest)
}

pub(super) fn true_lit() -> PExpr {
    PExpr::Lit(QValue::Bool(true))
}

impl Lower<'_> {
    pub(super) fn dec_of_expr(&self, ir: &Expr, p: &PExpr) -> Dec {
        match p {
            PExpr::Lit(QValue::Number(n)) => dec_of(&ir.ty, Some(n)),
            PExpr::Lit(_) => Dec::UNKNOWN,
            PExpr::Cast { num: Some(q), .. } if q.digits > 0 => Dec {
                p: q.digits.min(MAX_DIGITS),
                s: q.scale.min(q.digits).min(MAX_DIGITS),
            },
            PExpr::Neg(x) => match &ir.kind {
                ExprKind::Neg(ix) => self.dec_of_expr(ix, x),
                _ => Dec::UNKNOWN,
            },
            PExpr::Arith {
                first_dec, decs, ..
            } => decs.last().map_or(*first_dec, |d| d.out),
            _ => match &ir.kind {
                ExprKind::AggRef(i) => self
                    .agg_decs
                    .get(usize::from(*i))
                    .copied()
                    .unwrap_or(Dec::UNKNOWN),
                ExprKind::Col {
                    src,
                    col: ColSel::Column(i),
                } => self
                    .temp_stored(*src, *i)
                    .unwrap_or_else(|| dec_of(&ir.ty, None)),
                _ => dec_of(&ir.ty, None),
            },
        }
    }

    fn temp_stored(&self, src: SrcId, i: u16) -> Option<Dec> {
        let Some(SrcKind::Temp { stored, .. }) =
            self.stmt.sources.get(src.0 as usize).map(|s| &s.kind)
        else {
            return None;
        };
        let q = stored.get(usize::from(i))?.as_ref()?;
        Some(Dec {
            p: q.digits.min(MAX_DIGITS),
            s: q.scale.min(q.digits).min(MAX_DIGITS),
        })
    }
}
