use super::layout::{R, Row, internal, later, p16};
use super::rel::Lower;
use super::{PExpr, RelL};
use crate::ir::{ColSel, Expr, ExprKind, SliceS};
use crate::schema::{FieldRole, QFieldId, StdRole, VtKind};
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, TableId};

#[derive(Clone, Debug, PartialEq)]
pub struct SliceL {
    pub register: brace::Guid,
    pub first: bool,
    pub bound: Option<PExpr>,
    pub dimensions: Vec<u16>,
    pub period: u16,
    pub recorder: Option<u16>,
    pub active: Option<u16>,
    pub width: usize,
    pub view: Option<query_syntax::Span>,
}

impl Lower<'_> {
    pub(super) fn slice(&mut self, v: &SliceS) -> R<Row> {
        let vt = self.schema.table(v.table);
        let reg = self.schema.table(v.register);
        let Some(md) = reg.object else {
            return internal("slice register metadata");
        };
        let Some(main) = self.storage.table(v.register) else {
            return internal("slice register storage");
        };
        if self
            .b
            .security
            .slice_condition_has_deferred_reference(self.schema, v)
        {
            return later("Q7b-DeferredSecurityRead", "Q7c");
        }
        let mut needed: std::collections::HashSet<QFieldId> = self
            .vt_used
            .get(v.src.0 as usize)
            .into_iter()
            .flatten()
            .copied()
            .collect();
        for pred in v
            .cond
            .iter()
            .chain(v.area.iter().map(|a| &a.pred))
            .chain(v.access.iter())
        {
            crate::ir::visit_expr_in_place(pred, true, &mut |e| {
                if let ExprKind::Col {
                    src,
                    col: ColSel::Field(field),
                } = e.kind
                    && src == v.inner
                {
                    needed.insert(field);
                }
            });
        }
        if let Some(condition) = self.b.security.conditions.get(&v.src) {
            crate::ir::visit_expr_in_place(condition, true, &mut |e| {
                if let ExprKind::Col {
                    src,
                    col: ColSel::Field(field),
                } = e.kind
                    && src == v.src
                {
                    needed.insert(field);
                }
            });
        }
        let ranks_recorder = self
            .schema
            .meta(v.register)
            .is_some_and(|m| m.recorder_position);
        for (i, field) in vt.fields.iter().enumerate() {
            if matches!(
                field.role,
                FieldRole::Dimension
                    | FieldRole::Common
                    | FieldRole::Std(StdRole::Period | StdRole::Active)
            ) || (ranks_recorder && field.role == FieldRole::Std(StdRole::Recorder))
            {
                needed.insert(QFieldId(u16::try_from(i).map_err(|_| {
                    crate::ExecError::Fatal("too many slice fields".into())
                })?));
            }
        }
        let fields: Vec<QFieldId> = (0..vt.fields.len())
            .filter_map(|i| u16::try_from(i).ok().map(QFieldId))
            .filter(|field| needed.contains(field))
            .collect();
        self.slice_cols.insert(v.src, fields.clone());
        self.slice_cols.insert(v.inner, fields.clone());
        let pos = |role| {
            fields
                .iter()
                .position(|f| {
                    vt.fields
                        .get(usize::from(f.0))
                        .is_some_and(|f| f.role == FieldRole::Std(role))
                })
                .and_then(|i| u16::try_from(i).ok())
        };
        let period = pos(StdRole::Period)
            .ok_or_else(|| crate::ExecError::Fatal("slice period field".into()))?;
        let recorder = if ranks_recorder {
            pos(StdRole::Recorder)
        } else {
            None
        };
        let dimensions = fields
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                vt.fields
                    .get(usize::from(f.0))
                    .is_some_and(|f| matches!(f.role, FieldRole::Dimension | FieldRole::Common))
            })
            .map(|(i, _)| {
                u16::try_from(i)
                    .map_err(|_| crate::ExecError::Fatal("too many slice fields".into()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut safe = true;
        if let Some(c) = &v.cond {
            crate::ir::visit_expr_in_place(c, true, &mut |e| {
                if matches!(e.kind, ExprKind::Deref { .. } | ExprKind::InQuery { .. }) {
                    safe = false;
                }
                if let ExprKind::Col {
                    src,
                    col: ColSel::Field(f),
                } = &e.kind
                    && *src == v.inner
                    && !vt
                        .fields
                        .get(usize::from(f.0))
                        .is_some_and(|f| matches!(f.role, FieldRole::Dimension | FieldRole::Common))
                {
                    safe = false;
                }
            });
        }
        let mut empty = Row::empty();
        let bound = v.bounds[0]
            .as_ref()
            .map(|b| self.expr(b, &mut empty))
            .transpose()?;
        if empty.width != 0 {
            return internal("slice boundary reads a row");
        }
        let purpose = if v.kind == VtKind::SliceFirst {
            role::TOTALS_SLICE_FIRST
        } else {
            role::TOTALS_SLICE_LAST
        };
        let totals = if safe {
            self.map
                .object(md)
                .and_then(|o| o.service.iter().find(|(p, _)| p == purpose))
                .map(|(_, t)| *t)
        } else {
            None
        };
        let totals = totals.filter(|t| self.slice_fields(v, *t, &fields).is_some());
        let mut input = self.slice_input(v, main, &fields)?;
        let totals = totals
            .map(|t| {
                let branch = self.slice_input(v, t, &fields)?;
                if branch.width != input.width || branch.signal != input.signal {
                    return later("Q7b-DeferredSecurityRead", "Q7c");
                }
                Ok(Box::new(branch.rel))
            })
            .transpose()?;
        self.set_level(v.src);
        let row = Row {
            signal: input.signal,
            width: input.width,
            rel: RelL::Slice {
                input: input.take(),
                totals,
                step: Box::new(SliceL {
                    register: md,
                    first: v.kind == VtKind::SliceFirst,
                    bound,
                    dimensions,
                    period,
                    recorder,
                    active: pos(StdRole::Active),
                    width: input.width,
                    view: v.span,
                }),
            },
            rep: Some(v.src),
            keys: None,
        };
        self.secure_row(row, v.src)
    }

    fn slice_fields(
        &self,
        v: &SliceS,
        table: TableId,
        required: &[QFieldId],
    ) -> Option<Vec<FieldId>> {
        let vt = self.schema.table(v.table);
        let info = self.map.table(table);
        let mut fields = Vec::with_capacity(required.len());
        for field in required {
            let f = vt.fields.get(usize::from(field.0))?;
            let stored = match f.role {
                FieldRole::Std(StdRole::Period) => info.field_by_std(StdField::Period),
                FieldRole::Std(StdRole::Recorder) => info.field_by_std(StdField::Recorder),
                FieldRole::Std(StdRole::LineNumber) => info.field_by_std(StdField::LineNo),
                FieldRole::Std(StdRole::Active) => info.field_by_std(StdField::Active),
                _ => {
                    f.md.and_then(|md| info.fields.iter().position(|s| s.md == Some(md)))
                        .and_then(|i| u16::try_from(i).ok())
                        .map(FieldId)
                }
            };
            let field = stored?;
            fields.push(field);
        }
        Some(fields)
    }

    fn slice_condition_signal_supported(&self, v: &SliceS, table: TableId) -> bool {
        if v.bounds.iter().any(Option::is_some)
            || v.area.is_some()
            || v.access.is_some()
            || !self
                .schema
                .meta(v.register)
                .is_some_and(|meta| meta.periodic && !meta.subordinate && !meta.recorder_position)
            || !self.map.table(table).separators.is_empty()
        {
            return false;
        }
        let Some(condition) = &v.cond else {
            return false;
        };
        let dereference = match &condition.kind {
            ExprKind::Compare(_, left, right) if slice_scalar_literal(right) => left,
            ExprKind::Compare(_, left, right) if slice_scalar_literal(left) => right,
            ExprKind::IsNull { l, .. } => l,
            _ => return false,
        };
        let ExprKind::Deref { base, steps } = &dereference.kind else {
            return false;
        };
        let ExprKind::Col {
            src,
            col: ColSel::Field(dimension),
        } = base.kind
        else {
            return false;
        };
        if src != v.inner
            || base.ty.refs.len() != 1
            || base.ty.atom_count() != 1
            || !self
                .schema
                .field(v.table, dimension)
                .is_some_and(|field| field.role == FieldRole::Dimension && field.nested.is_none())
        {
            return false;
        }
        let [step] = steps.as_slice() else {
            return false;
        };
        let [(target, field)] = step.targets.as_slice() else {
            return false;
        };
        let Some(storage) = self.storage.table(*target) else {
            return false;
        };
        if !self.map.table(storage).separators.is_empty()
            || self.storage.field(*target, *field).is_none()
            || !self
                .schema
                .field(*target, *field)
                .is_some_and(|field| field.nested.is_none() && slice_primitive(&field.ty))
        {
            return false;
        }
        let Some(source) = self.b.security.target(self.schema, base, &[], *target) else {
            return false;
        };
        let Some(policy) = self.b.security.conditions.get(&source) else {
            return false;
        };
        let ExprKind::Or(predicates) = &policy.kind else {
            return false;
        };
        if predicates.len() > 1 {
            return false;
        }
        predicates.first().is_none_or(|predicate| {
            let local_scalar = |expr: &Expr| match expr.kind {
                ExprKind::Col {
                    src,
                    col: ColSel::Field(field),
                } if src == source => {
                    self.storage.field(*target, field).is_some()
                        && self.schema.field(*target, field).is_some_and(|field| {
                            field.nested.is_none() && slice_primitive(&field.ty)
                        })
                }
                _ => false,
            };
            match &predicate.kind {
                ExprKind::Lit(crate::QValue::Bool(_) | crate::QValue::Null) => true,
                ExprKind::Compare(_, left, right) => {
                    ((local_scalar(left) || slice_scalar_literal(left))
                        && slice_scalar_literal(right))
                        || (slice_scalar_literal(left) && local_scalar(right))
                }
                _ => false,
            }
        })
    }

    fn slice_input(&mut self, v: &SliceS, table: TableId, required: &[QFieldId]) -> R<Row> {
        let fields = self.slice_fields(v, table, required).ok_or_else(|| {
            crate::ExecError::Fatal("slice field missing from base storage".into())
        })?;
        self.set_level(v.inner);
        self.inner += 1;
        let result = (|| {
            let mut row = Row {
                signal: None,
                rel: RelL::Scan { table, fields },
                width: required.len(),
                rep: Some(v.inner),
                keys: None,
            };
            for pred in v
                .cond
                .iter()
                .chain(v.area.iter().map(|a| &a.pred))
                .chain(v.access.iter())
            {
                row = self.filter_row(row, pred)?;
            }
            if row.signal.is_some() && !self.slice_condition_signal_supported(v, table) {
                return later("Q7b-DeferredSecurityRead", "Q7c");
            }
            let mut exprs = (0..required.len())
                .map(|i| p16(i as i64).map(PExpr::Col))
                .collect::<R<Vec<_>>>()?;
            let signal = row
                .signal
                .map(|_| {
                    let index = exprs.len();
                    exprs.push(row.signal_expr());
                    p16(index as i64)
                })
                .transpose()?;
            row.width = exprs.len();
            row.rel = RelL::Project {
                input: row.take(),
                exprs,
            };
            row.signal = signal;
            if self.membership
                && let Some(signal) = signal
            {
                row.rel = RelL::CheckAccess {
                    input: row.take(),
                    signal,
                };
                self.added = self.added.saturating_add(1);
            }
            Ok(row)
        })();
        self.inner -= 1;
        result
    }
}

fn slice_primitive(ty: &crate::QType) -> bool {
    ty.refs.is_empty() && ty.others.is_empty() && ty.atom_count() <= 1
}

fn slice_scalar_literal(expr: &Expr) -> bool {
    slice_primitive(&expr.ty)
        && matches!(
            expr.kind,
            ExprKind::Lit(
                crate::QValue::Null
                    | crate::QValue::Bool(_)
                    | crate::QValue::Number(_)
                    | crate::QValue::String(_)
                    | crate::QValue::Date(_)
            )
        )
}
