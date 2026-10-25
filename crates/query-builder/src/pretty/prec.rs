use query_syntax::ast::{BinOp, Expr, ExprKind, UnOp};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Prec {
    Or,
    And,
    Not,
    Cmp,
    Pred,
    Add,
    Mul,
    Unary,
    Primary,
}

pub(crate) fn prec(mut e: &Expr) -> Prec {
    while let ExprKind::Chain { first, rest } = &e.kind
        && rest.is_empty()
    {
        e = first;
    }
    match &e.kind {
        ExprKind::Chain { rest, .. } => match rest.first().map(|o| o.op) {
            Some(BinOp::Or) => Prec::Or,
            Some(BinOp::And) => Prec::And,
            Some(BinOp::Add | BinOp::Sub) => Prec::Add,
            _ => Prec::Mul,
        },
        ExprKind::Unary { op: UnOp::Not, .. } => Prec::Not,
        ExprKind::Unary { .. } => Prec::Unary,
        ExprKind::Compare { .. } => Prec::Cmp,
        ExprKind::In { .. }
        | ExprKind::Between { .. }
        | ExprKind::IsNull { .. }
        | ExprKind::Refs { .. }
        | ExprKind::Like { .. } => Prec::Pred,
        _ => Prec::Primary,
    }
}

pub(crate) fn chain_operand(level: Prec) -> Prec {
    match level {
        Prec::Or => Prec::And,
        Prec::And => Prec::Not,
        Prec::Add => Prec::Mul,
        _ => Prec::Unary,
    }
}
