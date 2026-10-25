use super::OpError;
use crate::model::*;
use crate::open::{Ctx, open};
use crate::{OpenError, canon};
use query_syntax::{ast, with_deep_stack};

pub(super) fn reconcile(s: &mut Schema, ctx: &Ctx<'_>) -> Result<(), OpError> {
    let depth = s.max_depth;
    if !with_deep_stack(depth, || {
        s.batch.iter().all(|st| match st {
            Stmt::Select(q) => query_complete(q),
            Stmt::Drop(_) => true,
        })
    }) {
        return Ok(());
    }
    with_deep_stack(depth, || {
        for st in &mut s.batch {
            if let Stmt::Select(q) = st {
                complete_groups(q);
            }
        }
    });
    let mut printed = s.deep_clone();
    with_deep_stack(printed.max_depth, || {
        for st in &mut printed.batch {
            if let Stmt::Select(q) = st {
                strip_empty_index(q);
            }
        }
    });
    let text = printed.to_text();
    drop(printed);
    let opened = open(&text, s.mode, ctx).map_err(|e| match e {
        OpenError::Syntax(e) | OpenError::Semantic(e) => OpError::Semantic(e),
        OpenError::Shape { what, .. } => OpError::Shape(what),
    })?;
    let mut new = opened.schema;
    with_deep_stack(new.max_depth.max(depth), || {
        for (n, o) in new.batch.iter_mut().zip(&s.batch) {
            if let (Stmt::Select(n), Stmt::Select(o)) = (n, o) {
                carry_query(n, o);
            }
        }
    });
    new.mode = s.mode;
    *s = new;
    Ok(())
}

fn query_complete(q: &SelectQuery) -> bool {
    q.operators
        .iter()
        .all(|o| !o.fields.is_empty() && o.sources.iter().all(source_complete))
}

fn source_complete(s: &Source) -> bool {
    (match &s.table {
        Table::Subquery(q) => query_complete(q),
        _ => true,
    }) && s.joins.iter().all(|j| source_complete(&j.right))
}

fn complete_groups(q: &mut SelectQuery) {
    for op in &mut q.operators {
        canon::complete_group(op);
        for s in &mut op.sources {
            complete_groups_in(s);
        }
    }
}

fn complete_groups_in(s: &mut Source) {
    if let Table::Subquery(q) = &mut s.table {
        complete_groups(q);
    }
    for j in &mut s.joins {
        complete_groups_in(&mut j.right);
    }
}

fn strip_empty_index(q: &mut SelectQuery) {
    if is_empty_index(&q.index) {
        q.index = None;
    }
    if is_empty_totals(&q.totals) {
        q.totals = None;
    }
}

fn is_empty_totals(t: &Option<Totals>) -> bool {
    t.as_ref().is_some_and(|t| t.points.is_empty())
}

fn is_empty_index(i: &Option<ast::IndexKind>) -> bool {
    matches!(i, Some(ast::IndexKind::Fields { fields, .. }) if fields.is_empty())
}

fn carry_query(n: &mut SelectQuery, o: &SelectQuery) {
    if n.names.len() == o.names.len() {
        for (a, b) in n.names.iter_mut().zip(&o.names) {
            if b.is_some() {
                a.clone_from(b);
            }
        }
    }
    if is_empty_index(&o.index) {
        n.index.clone_from(&o.index);
    }
    if is_empty_totals(&o.totals) {
        n.totals.clone_from(&o.totals);
    }
    for (no, oo) in n.operators.iter_mut().zip(&o.operators) {
        carry_sources(&mut no.sources, &oo.sources);
        restore_unsplit(&mut no.filter, &oo.filter);
        restore_unsplit(&mut no.having, &oo.having);
    }
}

fn carry_sources(n: &mut [Source], o: &[Source]) {
    for (ns, os) in n.iter_mut().zip(o) {
        if let (Table::Subquery(nq), Table::Subquery(oq)) = (&mut ns.table, &os.table) {
            carry_query(nq, oq);
        }
        for (nj, oj) in ns.joins.iter_mut().zip(&os.joins) {
            if nj.on.len() == oj.on.len() {
                for (nc, oc) in nj.on.iter_mut().zip(&oj.on) {
                    nc.api = oc.api;
                }
            }
            carry_sources(
                std::slice::from_mut(&mut nj.right),
                std::slice::from_ref(&oj.right),
            );
        }
    }
}

fn is_and_chain(e: &ast::Expr) -> bool {
    matches!(&e.kind, ast::ExprKind::Chain { rest, .. }
        if rest.first().is_some_and(|r| r.op_span == crate::to_ast::UNSPLIT))
}

fn restore_unsplit(n: &mut Vec<ast::Expr>, o: &[ast::Expr]) {
    if !o.iter().any(is_and_chain) {
        return;
    }
    let count = |e: &ast::Expr| match &e.kind {
        ast::ExprKind::Chain { rest, .. } if is_and_chain(e) => 1 + rest.len(),
        _ => 1,
    };
    if n.len() == o.iter().map(count).sum::<usize>() {
        n.clear();
        n.extend(o.iter().cloned());
    }
}
