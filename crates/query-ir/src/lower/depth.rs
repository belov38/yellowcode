use super::{HierSeeds, PExpr as E, RelL as R};
fn expr(e: &E) -> u32 {
    let es = |xs: &[E]| xs.iter().map(expr).max().unwrap_or(0);
    let depth = match e {
        E::Neg(x)
        | E::Not(x)
        | E::IsNull { l: x, .. }
        | E::Refs { l: x, .. }
        | E::Cast { x, .. }
        | E::TypeOf { x, .. }
        | E::StoredSize { x, .. } => expr(x),
        E::Compare(_, a, b) | E::Like { l: a, pat: b, .. } => expr(a).max(expr(b)),
        E::And(xs) | E::Or(xs) | E::Func { args: xs, .. } => es(xs),
        E::Arith { first, steps, .. } => {
            expr(first).max(steps.iter().map(|(_, e, _)| expr(e)).max().unwrap_or(0))
        }
        E::InList { l, rhs, .. } => es(l).max(es(rhs)),
        E::InQuery { l, q, .. } => es(l).max(relation(q)),
        E::SectionAgg { owner, rel, .. } => expr(owner).max(relation(rel)),
        E::Between { l, lo, hi, .. } => expr(l).max(expr(lo)).max(expr(hi)),
        E::Case {
            operand,
            whens,
            else_,
        } => operand
            .iter()
            .chain(else_)
            .map(|e| expr(e))
            .chain(whens.iter().map(|(a, b)| expr(a).max(expr(b))))
            .max()
            .unwrap_or(0),
        E::RecordInInterval {
            period, recorder, ..
        } => expr(period).max(expr(recorder)),
        _ => 0,
    };
    depth.saturating_add(1)
}
pub(super) fn relation(r: &R) -> u32 {
    let es = |xs: &[E]| xs.iter().map(expr).max().unwrap_or(0);
    let depth = match r {
        R::RegisterRows(s) => s
            .chart_input()
            .map_or(0, relation)
            .max(s.bounds().iter().flatten().map(expr).max().unwrap_or(0)),
        R::Slice {
            input,
            totals,
            step,
        } => relation(input)
            .max(totals.as_deref().map_or(0, relation))
            .max(step.bound.as_ref().map_or(0, expr)),
        R::RunningBalance { input, step } => relation(input)
            .max(expr(&step.period))
            .max(es(&step.period_fields))
            .max(step.bounds.iter().flatten().map(expr).max().unwrap_or(0)),
        R::Join { l, r, on, eq, .. } => relation(l).max(relation(r)).max(expr(on)).max(
            eq.iter()
                .map(|(a, b)| expr(a).max(expr(b)))
                .max()
                .unwrap_or(0),
        ),
        R::Project { input, exprs } => relation(input).max(es(exprs)),
        R::Filter { input, pred } => relation(input).max(expr(pred)),
        R::Aggregate {
            input, keys, aggs, ..
        } => relation(input).max(es(keys)).max(
            aggs.iter()
                .filter_map(|a| a.arg.as_ref())
                .map(expr)
                .max()
                .unwrap_or(0),
        ),
        R::Sort { input, keys } | R::HierarchyOrder { input, keys, .. } => {
            relation(input).max(keys.iter().map(|(e, _)| expr(e)).max().unwrap_or(0))
        }
        R::CheckAccess { input, .. } | R::Top { input, .. } | R::Distinct(input) => relation(input),
        R::Union { parts, .. } => parts.iter().map(relation).max().unwrap_or(0),
        R::SemiJoin { input, l, q, .. } => relation(input).max(relation(q)).max(es(l)),
        R::HierarchyClosure { seeds, .. } => match seeds {
            HierSeeds::Rel(r) => relation(r),
            HierSeeds::Values(xs) => es(xs),
        },
        _ => 0,
    };
    depth.saturating_add(1)
}
