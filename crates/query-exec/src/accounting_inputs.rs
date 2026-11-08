use crate::model::{HashModel, size_add, size_block, size_vec};
use crate::ops::{MemBudget, OwnedCharge};
use query_ir::lower::{RegisterRowsKindL, RegisterRowsL};
use query_ir::{ExecError, HierSeeds, PExpr, QValue, RelL};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) fn clone_register_step_without_chart(step: &RegisterRowsL) -> RegisterRowsL {
    #[cfg(test)]
    crate::ordinary_calibration::capture_step("actual-original-step", step);
    let rows = match &step.rows {
        RegisterRowsKindL::AccountingBalance(a) => {
            RegisterRowsKindL::AccountingBalance(query_ir::lower::BalanceRowsL {
                inputs: a.inputs.clone(),
                resources: a.resources.clone(),
                date: a.date,
                period: a.period,
                chart_layout: a.chart_layout,
                chart_input: None,
                ext1: a.ext1.clone(),
                policy_fields: a.policy_fields.clone(),
                secured: a.secured,
            })
        }
        other => other.clone(),
    };
    let cloned = RegisterRowsL {
        register: step.register,
        kind: step.kind,
        view: step.view,
        rows,
    };
    #[cfg(test)]
    crate::ordinary_calibration::capture_step("actual-cloned-step", &cloned);
    cloned
}

#[derive(Clone, Copy)]
pub(crate) enum Node<'a> {
    Rel(&'a RelL),
    Expr(&'a PExpr),
}

pub(crate) fn walk<'a>(
    r: &'a RelL,
    f: &mut dyn FnMut(Node<'a>, u32) -> Result<(), ExecError>,
) -> Result<(), ExecError> {
    fn expr<'a>(
        e: &'a PExpr,
        d: u32,
        f: &mut dyn FnMut(Node<'a>, u32) -> Result<(), ExecError>,
    ) -> Result<(), ExecError> {
        f(Node::Expr(e), d)?;
        let d = d.saturating_add(1);
        match e {
            PExpr::Neg(x)
            | PExpr::Not(x)
            | PExpr::IsNull { l: x, .. }
            | PExpr::Refs { l: x, .. }
            | PExpr::Cast { x, .. }
            | PExpr::TypeOf { x, .. }
            | PExpr::StoredSize { x, .. } => expr(x, d, f)?,
            PExpr::Arith { first, steps, .. } => {
                expr(first, d, f)?;
                for (_, x, _) in steps {
                    expr(x, d, f)?;
                }
            }
            PExpr::And(es) | PExpr::Or(es) | PExpr::Func { args: es, .. } => {
                for e in es {
                    expr(e, d, f)?;
                }
            }
            PExpr::Compare(_, a, b) => {
                expr(a, d, f)?;
                expr(b, d, f)?;
            }
            PExpr::Between { l, lo, hi, .. } => {
                for e in [l, lo, hi] {
                    expr(e, d, f)?;
                }
            }
            PExpr::Like { l, pat, .. } => {
                expr(l, d, f)?;
                expr(pat, d, f)?;
            }
            PExpr::InList { l, rhs, .. } => {
                for e in l.iter().chain(rhs) {
                    expr(e, d, f)?;
                }
            }
            PExpr::InQuery { l, q, .. } => {
                for e in l {
                    expr(e, d, f)?;
                }
                rel(q, d, f)?;
            }
            PExpr::SectionAgg { owner, rel: r, .. } => {
                expr(owner, d, f)?;
                rel(r, d, f)?;
            }
            PExpr::Case {
                operand,
                whens,
                else_,
            } => {
                for e in operand.iter().chain(else_) {
                    expr(e, d, f)?;
                }
                for (a, b) in whens {
                    expr(a, d, f)?;
                    expr(b, d, f)?;
                }
            }
            PExpr::RecordInInterval {
                period, recorder, ..
            } => {
                expr(period, d, f)?;
                expr(recorder, d, f)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn rel<'a>(
        r: &'a RelL,
        d: u32,
        f: &mut dyn FnMut(Node<'a>, u32) -> Result<(), ExecError>,
    ) -> Result<(), ExecError> {
        f(Node::Rel(r), d)?;
        let d = d.saturating_add(1);
        match r {
            RelL::RegisterRows(s) => {
                for e in s.bounds().iter().flatten() {
                    expr(e, d, f)?;
                }
                if let Some(child) = s.chart_input() {
                    rel(child, d, f)?;
                }
            }
            RelL::Slice {
                input,
                totals,
                step,
            } => {
                rel(input, d, f)?;
                if let Some(r) = totals {
                    rel(r, d, f)?;
                }
                if let Some(e) = &step.bound {
                    expr(e, d, f)?;
                }
            }
            RelL::RunningBalance { input, step } => {
                rel(input, d, f)?;
                expr(&step.period, d, f)?;
                for e in step
                    .period_fields
                    .iter()
                    .chain(step.bounds.iter().flatten())
                {
                    expr(e, d, f)?;
                }
            }
            RelL::Filter { input, pred } => {
                rel(input, d, f)?;
                expr(pred, d, f)?;
            }
            RelL::Project { input, exprs } => {
                rel(input, d, f)?;
                for e in exprs {
                    expr(e, d, f)?;
                }
            }
            RelL::Join { l, r, on, eq, .. } => {
                rel(l, d, f)?;
                rel(r, d, f)?;
                expr(on, d, f)?;
                for (a, b) in eq {
                    expr(a, d, f)?;
                    expr(b, d, f)?;
                }
            }
            RelL::Aggregate {
                input, keys, aggs, ..
            } => {
                rel(input, d, f)?;
                for e in keys
                    .iter()
                    .chain(aggs.iter().filter_map(|a| a.arg.as_ref()))
                {
                    expr(e, d, f)?;
                }
            }
            RelL::Distinct(input) | RelL::Top { input, .. } | RelL::CheckAccess { input, .. } => {
                rel(input, d, f)?
            }
            RelL::Sort { input, keys } | RelL::HierarchyOrder { input, keys, .. } => {
                rel(input, d, f)?;
                for (e, _) in keys {
                    expr(e, d, f)?;
                }
            }
            RelL::Union { parts, .. } => {
                for r in parts {
                    rel(r, d, f)?;
                }
            }
            RelL::SemiJoin { input, l, q, .. } => {
                rel(input, d, f)?;
                rel(q, d, f)?;
                for e in l {
                    expr(e, d, f)?;
                }
            }
            RelL::HierarchyClosure { seeds, .. } => match seeds {
                HierSeeds::Rel(r) => rel(r, d, f)?,
                HierSeeds::Values(es) => {
                    for e in es {
                        expr(e, d, f)?;
                    }
                }
            },
            _ => {}
        }
        Ok(())
    }
    rel(r, 1, f)
}

pub(crate) fn has_balance_columns(r: &RelL) -> Result<bool, ExecError> {
    let mut found = false;
    walk(r, &mut |node, _| {
        if let Node::Rel(RelL::RegisterRows(step)) = node
            && let RegisterRowsKindL::AccountingBalance(balance) = &step.rows
        {
            found |= balance.ext1.is_some();
        }
        Ok(())
    })?;
    Ok(found)
}

pub(crate) fn protected(r: &RelL) -> Result<bool, ExecError> {
    let mut found = false;
    walk(r, &mut |n, _| {
        if let Node::Rel(RelL::RegisterRows(s)) = n {
            found |= match &s.rows {
                RegisterRowsKindL::AccountingBalance(a) => a.secured,
                RegisterRowsKindL::AccountingTurnovers(a) => a.secured,
                _ => false,
            };
        }
        Ok(())
    })?;
    Ok(found)
}

fn literal_copy(v: &QValue) -> u64 {
    match v {
        QValue::String(_) | QValue::Binary(_) | QValue::Storage(_) | QValue::Nested(_) => 0,
        _ => crate::model::value_model(v),
    }
}

pub(crate) fn structural_model(r: &RelL) -> Result<u64, ExecError> {
    let mut bytes = 0;
    walk(r, &mut |node, _| {
        let v = |n, size| size_vec(n, size);
        let ty = |t: &query_ir::QType| -> Result<u64, ExecError> {
            size_add(
                v(t.refs.len(), std::mem::size_of::<query_ir::RefAtom>())?,
                v(t.others.len(), std::mem::size_of::<query_ir::Other>())?,
            )
        };
        let mut own = match node {
            Node::Rel(_) => size_block(
                (std::mem::size_of::<RelL>()
                    + std::mem::size_of::<crate::plan::Plan>()
                    + std::mem::size_of::<RegisterRowsL>()) as u64,
            )?,
            Node::Expr(_) => size_block((2 * std::mem::size_of::<PExpr>()) as u64)?,
        };
        let extra = match node {
            Node::Rel(RelL::Scan { fields, .. }) => v(
                fields.len(),
                std::mem::size_of::<ibschema::storage::FieldId>(),
            )?,
            Node::Rel(RelL::TempScan { name, .. }) => size_block(name.len() as u64)?,
            Node::Rel(RelL::Slice { step, .. }) => {
                v(step.dimensions.len(), std::mem::size_of::<u16>())?
            }
            Node::Rel(RelL::RunningBalance { step, .. }) => size_add(
                v(step.period_fields.len(), std::mem::size_of::<PExpr>())?,
                v(step.wanted.len(), std::mem::size_of::<(usize, usize)>())?,
            )?,
            Node::Rel(RelL::HierarchyClosure { tables, seeds }) => {
                let b = v(tables.len(), std::mem::size_of::<query_ir::HierTable>())?;
                if let HierSeeds::Values(es) = seeds {
                    size_add(b, v(es.len(), std::mem::size_of::<PExpr>())?)?
                } else {
                    b
                }
            }
            Node::Rel(RelL::Project { exprs, .. }) => v(exprs.len(), std::mem::size_of::<PExpr>())?,
            Node::Rel(RelL::Join { eq, .. }) => v(eq.len(), std::mem::size_of::<(PExpr, PExpr)>())?,
            Node::Rel(RelL::Aggregate {
                keys, sets, aggs, ..
            }) => {
                let mut b = size_add(
                    v(keys.len(), std::mem::size_of::<PExpr>())?,
                    v(aggs.len(), std::mem::size_of::<query_ir::lower::AggL>())?,
                )?;
                for a in aggs {
                    b = size_add(b, ty(&a.ty)?)?;
                }
                if let Some(sets) = sets {
                    b = size_add(b, v(sets.len(), std::mem::size_of::<Vec<u16>>())?)?;
                    for s in sets {
                        b = size_add(b, v(s.len(), std::mem::size_of::<u16>())?)?;
                    }
                }
                b
            }
            Node::Rel(RelL::Sort { keys, .. } | RelL::HierarchyOrder { keys, .. }) => {
                v(keys.len(), std::mem::size_of::<(PExpr, bool)>())?
            }
            Node::Rel(RelL::Union { parts, all }) => size_add(
                v(parts.len(), std::mem::size_of::<RelL>())?,
                v(all.len(), std::mem::size_of::<bool>())?,
            )?,
            Node::Rel(RelL::SemiJoin { l, .. }) => v(l.len(), std::mem::size_of::<PExpr>())?,
            Node::Rel(RelL::RegisterRows(s)) => match &s.rows {
                RegisterRowsKindL::AccountingBalance(a) => size_add(
                    v(
                        a.resources.len(),
                        std::mem::size_of::<query_ir::lower::BalanceResourceL>(),
                    )?,
                    v(
                        a.policy_fields.len(),
                        std::mem::size_of::<ibschema::storage::FieldId>(),
                    )?,
                )?,
                RegisterRowsKindL::AccountingTurnovers(a) => size_add(
                    v(
                        a.resources.len(),
                        std::mem::size_of::<query_ir::lower::AccountingTurnoversResourceL>(),
                    )?,
                    v(
                        a.policy_fields.len(),
                        std::mem::size_of::<ibschema::storage::FieldId>(),
                    )?,
                )?,
                RegisterRowsKindL::Accounting(a) => size_add(
                    v(
                        a.keys.len(),
                        std::mem::size_of::<query_ir::lower::DrCrSideL>(),
                    )?,
                    v(
                        a.resources.len(),
                        std::mem::size_of::<query_ir::lower::DrCrResourceL>(),
                    )?,
                )?,
                RegisterRowsKindL::Accumulation(a) => {
                    let mut b = size_add(
                        size_add(
                            v(
                                a.dims.len(),
                                std::mem::size_of::<ibschema::storage::FieldId>(),
                            )?,
                            v(
                                a.resources.len(),
                                std::mem::size_of::<ibschema::storage::FieldId>(),
                            )?,
                        )?,
                        v(a.needed_dims.len(), std::mem::size_of::<bool>())?,
                    )?;
                    for t in a.totals.iter().chain(a.turnovers.iter()) {
                        b = size_add(
                            b,
                            size_add(
                                v(
                                    t.dims.len(),
                                    std::mem::size_of::<Option<ibschema::storage::FieldId>>(),
                                )?,
                                v(
                                    t.resources.len(),
                                    std::mem::size_of::<ibschema::storage::FieldId>(),
                                )?,
                            )?,
                        )?;
                    }
                    b
                }
            },
            Node::Expr(PExpr::Lit(value)) => literal_copy(value),
            Node::Expr(PExpr::And(es) | PExpr::Or(es) | PExpr::Func { args: es, .. }) => {
                v(es.len(), std::mem::size_of::<PExpr>())?
            }
            Node::Expr(PExpr::InList { l, rhs, .. }) => size_add(
                v(l.len(), std::mem::size_of::<PExpr>())?,
                v(rhs.len(), std::mem::size_of::<PExpr>())?,
            )?,
            Node::Expr(PExpr::InQuery { l, .. }) => v(l.len(), std::mem::size_of::<PExpr>())?,
            Node::Expr(PExpr::Case { whens, .. }) => {
                v(whens.len(), std::mem::size_of::<(PExpr, PExpr)>())?
            }
            Node::Expr(PExpr::Cast { to, ref_tables, .. }) => {
                size_add(ty(to)?, v(ref_tables.len(), std::mem::size_of::<u32>())?)?
            }
            Node::Expr(PExpr::TypeOf {
                refs, route_points, ..
            }) => size_add(
                v(refs.len(), std::mem::size_of::<(u32, query_ir::RefAtom)>())?,
                v(
                    route_points.len(),
                    std::mem::size_of::<(u32, query_ir::RefAtom)>(),
                )?,
            )?,
            Node::Expr(PExpr::Arith { steps, decs, .. }) => {
                let mut b = size_add(
                    v(
                        steps.len(),
                        std::mem::size_of::<(query_syntax::ast::BinOp, PExpr, query_ir::QType)>(),
                    )?,
                    v(decs.len(), std::mem::size_of::<query_ir::lower::ArithDec>())?,
                )?;
                for (_, _, t) in steps {
                    b = size_add(b, ty(t)?)?;
                }
                b
            }
            _ => 0,
        };
        if let Node::Expr(PExpr::Func { ty: t, .. }) = node {
            own = size_add(own, ty(t)?)?;
        }
        bytes = size_add(bytes, size_add(own, extra)?)?;
        Ok(())
    })?;
    Ok(bytes)
}

pub(crate) fn admit_plan_for_calculation(
    r: &RelL,
    mem: &MemBudget<'_>,
    calculation: bool,
) -> Result<Option<OwnedCharge>, ExecError> {
    if !(calculation || protected(r)?) {
        return Ok(None);
    }
    let bytes = structural_model(r)?
        .checked_mul(2)
        .ok_or_else(crate::model::invalid_size)?;
    mem.hold_owned(bytes).map(Some)
}

struct Payload {
    _set: Option<Arc<query_ir::ValueSet>>,
    _value: Option<QValue>,
}
struct Entry {
    _payload: Payload,
    _charge: OwnedCharge,
}

pub(crate) struct PaidPayloads {
    entries: HashMap<(u8, usize, usize), Entry>,
    model: HashModel,
    table_charge: Option<OwnedCharge>,
    _node_charge: OwnedCharge,
    bytes: u64,
}
impl PaidPayloads {
    pub(crate) fn new(mem: &MemBudget<'_>) -> Result<Self, ExecError> {
        let bytes = size_block(
            (2 * std::mem::size_of::<usize>() + std::mem::size_of::<std::cell::RefCell<Self>>())
                as u64,
        )?;
        let node_charge = mem.hold_owned(bytes)?;
        Ok(Self {
            entries: HashMap::new(),
            model: HashModel::new(std::mem::size_of::<((u8, usize, usize), Entry)>() as u64),
            table_charge: None,
            _node_charge: node_charge,
            bytes,
        })
    }
    fn insert(
        &mut self,
        id: (u8, usize, usize),
        bytes: u64,
        payload: impl FnOnce() -> Payload,
        mem: &MemBudget<'_>,
    ) -> Result<bool, ExecError> {
        if self.entries.contains_key(&id) {
            return Ok(false);
        }
        let total = size_add(self.bytes, bytes)?;
        let charge = mem.hold_owned(bytes)?;
        let growth = self.model.growth();
        let table = if growth > 0 {
            Some(mem.hold_owned(growth)?)
        } else {
            None
        };
        self.entries.insert(
            id,
            Entry {
                _payload: payload(),
                _charge: charge,
            },
        );
        self.model.inserted();
        self.bytes = total;
        if let Some(table) = table {
            self.table_charge = Some(table);
        }
        Ok(true)
    }
    fn value(&mut self, value: &QValue, mem: &MemBudget<'_>) -> Result<(), ExecError> {
        let id = match value {
            QValue::String(s) => Some((1, s.0.as_ptr() as usize, s.0.len())),
            QValue::Binary(b) | QValue::Storage(b) => Some((2, b.as_ptr() as usize, b.len())),
            QValue::Nested(n) => Some((
                3,
                std::ptr::from_ref::<query_ir::NestedResult>(n) as usize,
                0,
            )),
            _ => None,
        };
        if let Some(id) = id {
            self.insert(
                id,
                crate::model::value_model(value),
                || Payload {
                    _set: None,
                    _value: Some(value.clone()),
                },
                mem,
            )?;
        }
        Ok(())
    }
    pub(crate) fn relation(&mut self, r: &RelL, mem: &MemBudget<'_>) -> Result<(), ExecError> {
        walk(r, &mut |node, _| {
            match node {
                Node::Expr(PExpr::Set(set)) => {
                    let id = (0, Arc::as_ptr(set) as usize, 0);
                    let bytes = set.values.iter().try_fold(
                        size_add(
                            size_block((16 + std::mem::size_of::<query_ir::ValueSet>()) as u64)?,
                            size_vec(set.values.capacity(), std::mem::size_of::<QValue>())?,
                        )?,
                        |b, v| size_add(b, literal_copy(v)),
                    )?;
                    if self.insert(
                        id,
                        bytes,
                        || Payload {
                            _set: Some(set.clone()),
                            _value: None,
                        },
                        mem,
                    )? {
                        for value in &set.values {
                            self.value(value, mem)?;
                        }
                    }
                }
                Node::Expr(PExpr::Lit(value)) => self.value(value, mem)?,
                _ => {}
            }
            Ok(())
        })
    }
}
impl Drop for PaidPayloads {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.entries));
    }
}
