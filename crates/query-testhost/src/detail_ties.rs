use query_syntax::ast::{ExprKind, ItemKind, OrderDir, Path, Stmt, TableRef};

fn folded_path(path: &Path) -> Vec<String> {
    path.parts
        .iter()
        .map(|part| bsl_prim::lowercase(&part.text).collect())
        .collect()
}

pub(crate) fn columns(stmt: &Stmt) -> Option<Vec<usize>> {
    let Stmt::Query(stmt) = stmt else { return None };
    let q = &stmt.query;
    let [select] = q.parts.as_slice() else {
        return None;
    };
    if q.totals.is_some()
        || q.autoorder.is_some()
        || select.group_by.is_some()
        || select.having.is_some()
        || select.distinct.is_some()
        || select.top.is_some()
        || select.into.is_some()
        || !select.dcs.is_empty()
        || !q.dcs.is_empty()
    {
        return None;
    }
    let [source] = select.from.as_ref()?.sources.as_slice() else {
        return None;
    };
    if !source.joins.is_empty() {
        return None;
    }
    let TableRef::Named { path, .. } = &source.table else {
        return None;
    };
    let path = folded_path(path);
    let [kind, _, view] = path.as_slice() else {
        return None;
    };
    if !matches!(kind.as_str(), "регистрнакопления" | "accumulationregister")
        || !matches!(
            view.as_str(),
            "остатки"
                | "обороты"
                | "остаткииобороты"
                | "balance"
                | "turnovers"
                | "balanceandturnovers"
        )
    {
        return None;
    }
    if !select.items.iter().all(
        |item| matches!(&item.kind, ItemKind::Expr(expr) if matches!(expr.kind, ExprKind::Path(_))),
    ) {
        return None;
    }
    let order = &q.order_by.as_ref()?.items;
    if order.is_empty() {
        return None;
    }
    order
        .iter()
        .map(|key| {
            if !matches!(key.dir, OrderDir::Asc | OrderDir::Desc) {
                return None;
            }
            let ExprKind::Path(path) = &key.expr.kind else {
                return None;
            };
            let wanted = folded_path(path);
            let mut candidates = select.items.iter().enumerate().filter_map(|(i, item)| {
                let ItemKind::Expr(expr) = &item.kind else {
                    return None;
                };
                let alias_matches = wanted.len() == 1
                    && item.alias.as_ref().is_some_and(|alias| {
                        wanted.first().is_some_and(|name| {
                            bsl_prim::lowercase(&alias.name.text).eq(name.chars())
                        })
                    });
                let expr_matches = match &expr.kind {
                    ExprKind::Path(path) => {
                        let selected = folded_path(path);
                        selected == wanted
                            || (wanted.len() == 1
                                && item.alias.is_none()
                                && selected.last() == wanted.first())
                    }
                    _ => false,
                };
                (alias_matches || expr_matches).then_some(i)
            });
            let column = candidates.next()?;
            candidates.next().is_none().then_some(column)
        })
        .collect()
}
