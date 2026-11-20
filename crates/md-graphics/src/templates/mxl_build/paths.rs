use super::{BuildError, R, nodes::Cx};
use memory_budget::{BudgetedVec, Category};
use unicode_normalization::char::{canonical_combining_class, decompose_canonical};
fn key(path: &str, cx: Cx<'_>) -> R<BudgetedVec<(u8, char)>> {
    if path.len() > 4096 {
        return Err(BuildError::Invalid("asset path length"));
    }
    let mut out = BudgetedVec::new(cx.budget, Category::Metadata)?;
    for c in path.chars() {
        cx.check()?;
        let mut failure = None;
        decompose_canonical(c, |c| {
            if failure.is_none()
                && let Err(e) = out.try_push((canonical_combining_class(c), c))
            {
                failure = Some(e);
            }
        });
        if let Some(e) = failure {
            return Err(e.into());
        }
    }
    let mut run = 0;
    for i in 0..out.len() {
        let class = out.get(i).ok_or(BuildError::Invalid("path character"))?.0;
        if class == 0 {
            run = 0;
            continue;
        }
        run += 1;
        if run > 64 {
            return Err(BuildError::Invalid("asset combining sequence limit"));
        }
        let mut at = i;
        while at > 0
            && out
                .get(at - 1)
                .is_some_and(|(previous, _)| *previous > class)
        {
            out.as_mut_slice().swap(at - 1, at);
            at -= 1;
        }
    }
    Ok(out)
}
pub(super) fn equal(a: &str, b: &str, fold: bool, cx: Cx<'_>) -> R<bool> {
    let a = key(a, cx)?;
    let b = key(b, cx)?;
    Ok(if fold {
        a.iter()
            .flat_map(|(_, c)| c.to_lowercase())
            .eq(b.iter().flat_map(|(_, c)| c.to_lowercase()))
    } else {
        a.iter().map(|(_, c)| c).eq(b.iter().map(|(_, c)| c))
    })
}
