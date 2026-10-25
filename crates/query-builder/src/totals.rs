use bsl_prim::fold_name;
use query_syntax::Func;
use query_syntax::ast::{self, ExprKind, FuncArgs, ItemKind};

pub(crate) fn batch(batch: &mut ast::Batch) {
    for statement in &mut batch.stmts {
        if let ast::Stmt::Query(statement) = statement {
            query(&mut statement.query);
        }
    }
}

fn query(query: &mut ast::Query) {
    for part in &mut query.parts {
        if let Some(from) = &mut part.from {
            for item in &mut from.sources {
                source(item);
            }
        }
    }
    let [select] = query.parts.as_slice() else {
        return;
    };
    let Some(totals) = &mut query.totals else {
        return;
    };
    for item in &mut totals.items {
        let Some(target) = &item.alias else { continue };
        let ExprKind::Func {
            func,
            args: FuncArgs::List(args),
            ..
        } = &mut item.expr.kind
        else {
            continue;
        };
        if !matches!(
            func,
            Func::Sum | Func::Avg | Func::Min | Func::Max | Func::Count
        ) {
            continue;
        }
        let [argument] = args.as_mut_slice() else {
            continue;
        };
        if matches!(
            argument.kind,
            ExprKind::Path(_) | ExprKind::Param(_) | ExprKind::Lit(_)
        ) {
            continue;
        }
        let key = query_syntax::print::expr_sexpr(argument);
        let mut matches = select.items.iter().filter(|selected| {
            matches!(&selected.kind, ItemKind::Expr(expr) if query_syntax::print::expr_sexpr(expr) == key)
        });
        let Some(selected) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            continue;
        }
        let Some(alias) = &selected.alias else {
            continue;
        };
        if fold_name(&alias.name.text) != fold_name(&target.name.text) {
            continue;
        }
        let aliases = select
            .items
            .iter()
            .filter(|selected| {
                let name =
                    selected
                        .alias
                        .as_ref()
                        .map(|a| &a.name)
                        .or_else(|| match &selected.kind {
                            ItemKind::Expr(ast::Expr {
                                kind: ExprKind::Path(path),
                                ..
                            }) => path.parts.last(),
                            _ => None,
                        });
                name.is_some_and(|name| fold_name(&name.text) == fold_name(&alias.name.text))
            })
            .count();
        if aliases != 1 {
            continue;
        }
        argument.kind = ExprKind::Path(ast::Path {
            parts: vec![ast::Ident {
                text: alias.name.text.clone(),
                span: argument.span,
            }],
        });
        item.alias = None;
    }
}

fn source(item: &mut ast::Source) {
    if let ast::TableRef::Subquery { query: nested, .. } = &mut item.table {
        query(nested);
    }
    for join in &mut item.joins {
        source(&mut join.right);
    }
}
