use super::layout::{R, Row, p16};
use super::rel::Lower;
use super::{AggL, Dec, PExpr, RelL};
use crate::ir::{ColSel, ExprKind, SrcId, SrcKind};
use crate::{QType, QValue};
use query_syntax::Func;

pub(super) fn neutral(signal: u16) -> PExpr {
    PExpr::Case {
        operand: None,
        whens: vec![(
            PExpr::IsNull {
                l: Box::new(PExpr::Col(signal)),
                not: false,
            },
            PExpr::Lit(QValue::Bool(true)),
        )],
        else_: Some(Box::new(PExpr::Col(signal))),
    }
}
pub(super) fn min(signal: u16) -> AggL {
    AggL {
        f: Func::Min,
        distinct: false,
        arg: Some(neutral(signal)),
        ty: QType::boolean(),
        arg_dec: Dec::UNKNOWN,
    }
}
impl Row {
    pub(super) fn signal_expr(&self) -> PExpr {
        self.signal
            .map(neutral)
            .unwrap_or(PExpr::Lit(QValue::Bool(true)))
    }
    pub(super) fn append_signal(&mut self, signal: PExpr) -> R<()> {
        let pos = p16(self.width as i64)?;
        let mut exprs = (0..pos).map(PExpr::Col).collect::<Vec<_>>();
        exprs.push(signal);
        self.rel = RelL::Project {
            input: self.take(),
            exprs,
        };
        self.width += 1;
        self.signal = Some(pos);
        Ok(())
    }
    pub(super) fn checked(mut self) -> Self {
        if let Some(signal) = self.signal.take() {
            self.rel = RelL::CheckAccess {
                input: self.take(),
                signal,
            };
        }
        self
    }
}
impl Lower<'_> {
    pub(super) fn secure_raw(&mut self, mut row: Row, from: SrcId, to: SrcId) -> R<Row> {
        let Some(condition) = self.b.security.conditions.get(&from) else {
            return Ok(row);
        };
        if let Some(SrcKind::Virtual { cols, .. }) =
            self.stmt.sources.get(to.0 as usize).map(|s| &s.kind)
        {
            let mut missing = false;
            crate::ir::visit_expr_in_place(condition, true, &mut |e| {
                if let ExprKind::Col {
                    src,
                    col: ColSel::Field(field),
                } = e.kind
                    && src == from
                    && !cols.contains(&Some(field))
                {
                    missing = true;
                }
            });
            if missing {
                return Err(crate::ExecError::Later {
                    what: "Q7b-VtRestrictionField",
                    stage: "Q5/Q6",
                });
            }
        }
        self.applied_security.insert(from);
        let condition =
            crate::bind::security::reanchor(condition, from, to, self.stmt.sources.len())?;
        self.policy_depth += 1;
        let pred = self.expr(&condition, &mut row);
        self.policy_depth -= 1;
        let pred = pred?;
        if self.stmt.allowed {
            row.rel = RelL::Filter {
                input: row.take(),
                pred,
            };
        } else {
            row.append_signal(PExpr::Case {
                operand: None,
                whens: vec![(pred, PExpr::Lit(QValue::Bool(true)))],
                else_: Some(Box::new(PExpr::Lit(QValue::Bool(false)))),
            })?;
        }
        Ok(row)
    }
    pub(super) fn secure_row(&mut self, mut row: Row, src: SrcId) -> R<Row> {
        let Some(condition) = self.b.security.conditions.get(&src) else {
            return Ok(row);
        };
        self.applied_security.insert(src);
        self.policy_depth += 1;
        let pred = self.expr(condition, &mut row);
        self.policy_depth -= 1;
        let pred = pred?;
        if self.stmt.allowed {
            row.rel = RelL::Filter {
                input: row.take(),
                pred,
            };
        } else {
            row.append_signal(PExpr::Case {
                operand: None,
                whens: vec![(pred, PExpr::Lit(QValue::Bool(true)))],
                else_: Some(Box::new(PExpr::Lit(QValue::Bool(false)))),
            })?;
        }
        Ok(row)
    }
}
