use std::collections::HashMap;
use std::ops::Bound;

use query_ir::{ExecError, PExpr, QType, QValue};
use query_syntax::ast::{CmpOp, JoinKind};

use super::access::{eq_const, flip};
use super::{Plan, Planner, fatal, wrap};
use crate::temp::TempTable;

#[derive(Default)]
struct Cons {
    eq: Option<PExpr>,
    lo: Option<(PExpr, bool)>,
    hi: Option<(PExpr, bool)>,
}

fn range_const(e: &PExpr, ty: Option<&QType>) -> bool {
    let (PExpr::Lit(v), Some(t)) = (e, ty) else {
        return false;
    };
    if t.null || !t.refs.is_empty() || !t.others.is_empty() {
        return false;
    }
    let kinds = usize::from(t.boolean)
        + usize::from(t.number.is_some())
        + usize::from(t.string.is_some())
        + usize::from(t.date.is_some());
    kinds == 1
        && matches!(
            v,
            QValue::Bool(_) | QValue::Number(_) | QValue::String(_) | QValue::Date(_)
        )
        && match v {
            QValue::Bool(_) => t.boolean,
            QValue::Number(_) => t.number.is_some(),
            QValue::String(_) => t.string.is_some(),
            _ => t.date.is_some(),
        }
}

fn constraints(t: &TempTable, conj: &[PExpr]) -> HashMap<u16, Cons> {
    let mut out: HashMap<u16, Cons> = HashMap::new();
    let ty = |c: u16| {
        t.schema
            .columns
            .get(usize::from(c))
            .and_then(|c| c.ty.as_ref())
    };
    let col = |e: &PExpr| match e {
        PExpr::Col(c) => Some(*c),
        _ => None,
    };
    for c in conj {
        match c {
            PExpr::Compare(op, a, b) => {
                let (c, op, k) = match (col(a), col(b)) {
                    (Some(c), None) => (c, *op, &**b),
                    (None, Some(c)) => (c, flip(*op), &**a),
                    _ => continue,
                };
                let ok_range = range_const(k, ty(c));
                match op {
                    CmpOp::Eq if eq_const(k) => {
                        out.entry(c)
                            .or_default()
                            .eq
                            .get_or_insert_with(|| k.clone());
                    }
                    CmpOp::Gt if ok_range => {
                        out.entry(c)
                            .or_default()
                            .lo
                            .get_or_insert((k.clone(), false));
                    }
                    CmpOp::Ge if ok_range => {
                        out.entry(c)
                            .or_default()
                            .lo
                            .get_or_insert((k.clone(), true));
                    }
                    CmpOp::Lt if ok_range => {
                        out.entry(c)
                            .or_default()
                            .hi
                            .get_or_insert((k.clone(), false));
                    }
                    CmpOp::Le if ok_range => {
                        out.entry(c)
                            .or_default()
                            .hi
                            .get_or_insert((k.clone(), true));
                    }
                    _ => {}
                }
            }
            PExpr::Between {
                l,
                not: false,
                lo,
                hi,
            } => {
                if let Some(c) = col(l)
                    && range_const(lo, ty(c))
                    && range_const(hi, ty(c))
                {
                    let s = out.entry(c).or_default();
                    s.lo.get_or_insert(((**lo).clone(), true));
                    s.hi.get_or_insert(((**hi).clone(), true));
                }
            }
            _ => {}
        }
    }
    out
}

impl Planner<'_> {
    fn temp_table(&self, name: &str) -> Result<(crate::temp::TempTableId, &TempTable), ExecError> {
        let temps = self.temps.ok_or(ExecError::Later {
            what: "временная таблица в запросе без менеджера",
            stage: "Q3",
        })?;
        let id = temps
            .id_of(name)
            .ok_or_else(|| fatal("temporary table not found"))?;
        let t = temps
            .table_by_id(id)
            .ok_or_else(|| fatal("temporary table not found"))?;
        Ok((id, t))
    }

    pub(super) fn temp_access(&self, name: &str, pending: Vec<PExpr>) -> Result<Plan, ExecError> {
        let (id, t) = self.temp_table(name)?;
        let shown = t.schema.name.clone();
        let plan = if pending.is_empty() || !self.use_index() || t.indexes().is_empty() {
            None
        } else {
            self.temp_range(id, t, &pending)
        };
        let plan = plan.unwrap_or(Plan::TempScan {
            table: id,
            name: shown,
        });
        Ok(wrap(plan, pending))
    }

    fn temp_range(
        &self,
        id: crate::temp::TempTableId,
        t: &TempTable,
        pending: &[PExpr],
    ) -> Option<Plan> {
        let cons = constraints(t, pending);
        let mut best: Option<(usize, usize, Vec<PExpr>, Option<u16>)> = None;
        for (k, ix) in t.indexes().iter().enumerate() {
            let mut eqs = Vec::new();
            let mut tail = None;
            for c in &ix.cols {
                let Some(cn) = cons.get(c) else {
                    break;
                };
                if let Some(e) = &cn.eq {
                    eqs.push(e.clone());
                    continue;
                }
                if cn.lo.is_some() || cn.hi.is_some() {
                    tail = Some(*c);
                }
                break;
            }
            let covered = eqs.len() + usize::from(tail.is_some());
            if covered > 0 && best.as_ref().is_none_or(|b| covered > b.0) {
                best = Some((covered, k, eqs, tail));
            }
        }
        let (_, k, eqs, tail) = best?;
        let ix = t.indexes().get(k)?;
        let with_last = |e: &PExpr| {
            let mut v = eqs.clone();
            v.push(e.clone());
            v
        };
        let (lo, hi) = match tail.and_then(|c| cons.get(&c)) {
            None => (Bound::Included(eqs.clone()), Bound::Included(eqs.clone())),
            Some(c) => {
                let edge = |b: &Option<(PExpr, bool)>| match b {
                    Some((e, true)) => Bound::Included(with_last(e)),
                    Some((e, false)) => Bound::Excluded(with_last(e)),
                    None if eqs.is_empty() => Bound::Unbounded,
                    None => Bound::Included(eqs.clone()),
                };
                (edge(&c.lo), edge(&c.hi))
            }
        };
        Some(Plan::TempIndexRange {
            table: id,
            name: t.schema.name.clone(),
            index: k,
            index_name: ix.name.clone(),
            lo,
            hi,
        })
    }

    pub(super) fn temp_index_join(
        &self,
        kind: JoinKind,
        lp: &Plan,
        name: &str,
        width: u16,
        on: &PExpr,
        eq: &[(PExpr, PExpr)],
    ) -> Option<Plan> {
        let (id, t) = self.temp_table(name).ok()?;
        let mut best: Option<(usize, usize, Vec<PExpr>)> = None;
        for (k, ix) in t.indexes().iter().enumerate() {
            let mut keys = Vec::new();
            for c in &ix.cols {
                let Some((a, _)) = eq
                    .iter()
                    .find(|(_, b)| matches!(b, PExpr::Col(d) if d == c))
                else {
                    break;
                };
                keys.push(a.clone());
            }
            if !keys.is_empty() && best.as_ref().is_none_or(|b| keys.len() > b.0) {
                best = Some((keys.len(), k, keys));
            }
        }
        let (_, k, keys) = best?;
        let ix = t.indexes().get(k)?;
        Some(Plan::TempIndexJoin {
            kind,
            l: Box::new(lp.clone()),
            table: id,
            name: t.schema.name.clone(),
            index: k,
            index_name: ix.name.clone(),
            keys,
            width,
            on: on.clone(),
        })
    }
}
