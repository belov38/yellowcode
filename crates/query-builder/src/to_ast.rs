use crate::canon;
use crate::model::*;
use crate::names;
use query_syntax::ast::{self, BinOp, ExprKind, ItemKind};
use query_syntax::{SourceMap, Span, with_deep_stack};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conventions {
    AsWritten,
    OneC,
}

pub(crate) const SP: Span = Span { start: 0, end: 0 };

pub(crate) const UNSPLIT: Span = Span {
    start: u32::MAX,
    end: u32::MAX,
};

pub(crate) const SAME_BLOCK: Span = Span { start: 0, end: 1 };

pub fn to_ast(s: &Schema, conv: Conventions) -> ast::Batch {
    let mut b = to_ast_marked(s, conv);
    take_marks(&mut b);
    b
}

pub(crate) fn to_ast_marked(s: &Schema, conv: Conventions) -> ast::Batch {
    let stmts = with_deep_stack(s.max_depth, || {
        s.batch
            .iter()
            .map(|st| match st {
                Stmt::Select(q) => {
                    let mut query = query(q, conv, false);
                    if conv == Conventions::OneC
                        && s.mode == query_syntax::ParseMode::Query
                        && let Some(totals) = &mut query.totals
                    {
                        canon::totals_aliases(
                            totals,
                            query.parts.first().map_or(&[], |p| &p.items),
                        );
                    }
                    ast::Stmt::Query(Box::new(ast::QueryStmt { query, span: SP }))
                }
                Stmt::Drop(t) => ast::Stmt::Drop(ast::DropStmt {
                    kw: SP,
                    name: ident(t),
                    span: SP,
                }),
            })
            .collect()
    });
    ast::Batch {
        stmts,
        map: SourceMap::new(""),
        mode: s.mode,
        max_depth: s.max_depth,
    }
}

pub(crate) fn ident(t: &str) -> ast::Ident {
    ast::Ident {
        text: t.into(),
        span: SP,
    }
}

fn explicit(name: &str) -> Option<ast::Alias> {
    let word = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    if !word {
        return None;
    }
    Some(ast::Alias {
        name: ident(name),
        as_kw: Some(SP),
    })
}

pub(crate) fn embedded(q: &ast::Query) -> ast::Query {
    match crate::from_ast::query(q.clone()) {
        Ok(m) => query(&m, Conventions::OneC, true),
        Err(_) => q.clone(),
    }
}

fn query(q: &SelectQuery, conv: Conventions, embedded: bool) -> ast::Query {
    let one_c = conv == Conventions::OneC;
    let last = q.operators.len().saturating_sub(1);
    let parts = q
        .operators
        .iter()
        .enumerate()
        .map(|(i, op)| {
            let mut sel = ast::Select {
                kw: SP,
                allowed: (i == 0 && q.allowed).then_some(SP),
                distinct: op.distinct.then_some(SP),
                top: op.top.as_ref().map(|d| ast::Top {
                    kw: SP,
                    count: ast::NumLit {
                        digits: d.as_str().into(),
                    },
                    count_span: SP,
                }),
                items: match (one_c, i) {
                    (true, 0) if embedded => op.fields.clone(),
                    (true, 0) => named(&op.fields, &q.names, &names::alias_set(&op.sources)),
                    (true, _) => op
                        .fields
                        .iter()
                        .cloned()
                        .map(|mut it| {
                            it.alias = None;
                            it
                        })
                        .collect(),
                    _ => op.fields.clone(),
                },
                into: q.into.as_ref().filter(|_| i == 0).map(|p| ast::IntoClause {
                    kind: p.kind,
                    kw: SP,
                    name: ident(&p.table),
                }),
                from: (!op.sources.is_empty()).then(|| ast::FromClause {
                    kw: SP,
                    sources: op
                        .sources
                        .iter()
                        .map(|s| source(s, conv, &[], embedded))
                        .collect(),
                }),
                where_: and_chain(&op.filter).map(|expr| ast::Clause { kw: SP, expr }),
                group_by: op.group.clone().map(|kind| ast::GroupBy { kw: SP, kind }),
                having: and_chain(&op.having).map(|expr| ast::Clause { kw: SP, expr }),
                for_update: op
                    .for_update
                    .clone()
                    .map(|tables| ast::ForUpdate { kw: SP, tables }),
                index_by: q
                    .index
                    .clone()
                    .filter(|_| i == last)
                    .map(|kind| ast::IndexBy { kw: SP, kind }),
                dcs: if one_c {
                    canon::dcs(&op.dcs)
                } else {
                    op.dcs.clone()
                },
                span: SP,
            };
            if one_c {
                canon::select(&mut sel, op);
            }
            sel
        })
        .collect();
    let mut order_by = (!q.order.is_empty()).then(|| ast::OrderBy {
        kw: SP,
        items: q.order.clone(),
    });
    let mut totals = q
        .totals
        .as_ref()
        .filter(|t| !t.points.is_empty())
        .map(|t| ast::Totals {
            kw: SP,
            items: t.items.clone(),
            by_kw: SP,
            points: t.points.clone(),
        });
    if one_c {
        if let Some(o) = &mut order_by {
            canon::order(&mut o.items);
        }
        if let Some(t) = &mut totals {
            canon::totals(t);
        }
    }
    ast::Query {
        parts,
        unions: q
            .operators
            .iter()
            .skip(1)
            .map(|o| ast::UnionOp {
                kw: SP,
                all: o.union_all,
            })
            .collect(),
        order_by,
        autoorder: q.autoorder.then_some(SP),
        totals,
        dcs: q.dcs.clone(),
        span: SP,
    }
}

fn named(
    items: &[ast::SelectItem],
    names: &[Option<String>],
    aliases: &HashSet<String>,
) -> Vec<ast::SelectItem> {
    items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let mut it = it.clone();
            let unnamed = matches!(it.kind, ItemKind::Expr(_) | ItemKind::Nested { .. });
            if let ItemKind::Nested { fields, .. } = &mut it.kind {
                *fields = named(fields, &names::nested(fields, aliases), aliases);
            }
            if let Some(a) = &mut it.alias {
                a.as_kw = Some(SP);
            } else if unnamed {
                it.alias = names.get(i).and_then(|n| n.as_deref()).and_then(explicit);
            }
            it
        })
        .collect()
}

fn source(s: &Source, conv: Conventions, left: &[String], embedded: bool) -> ast::Source {
    let one_c = conv == Conventions::OneC;
    let table = match &s.table {
        Table::Named { path, args } => ast::TableRef::Named {
            path: path.clone(),
            args: args.clone().map(|args| ast::VtArgs {
                args: if one_c {
                    args.into_iter().map(|a| a.map(canon::vt_arg)).collect()
                } else {
                    args
                },
                span: SP,
            }),
        },
        Table::Subquery(q) => ast::TableRef::Subquery {
            query: Box::new(query(q, conv, embedded)),
            span: SP,
        },
        Table::Param(p) => ast::TableRef::Param(p.clone()),
    };
    let alias = match (one_c, &s.alias, &s.table) {
        (true, Some(a), _) => Some(ast::Alias {
            name: a.name.clone(),
            as_kw: Some(SP),
        }),
        (true, None, Table::Named { path, .. }) if !embedded => path
            .parts
            .last()
            .and_then(|p| explicit(p.text.trim_start_matches('#'))),
        _ => s.alias.clone(),
    };
    let mut mine = left.to_vec();
    mine.extend(canon::alias_of(s));
    ast::Source {
        table,
        alias,
        joins: s
            .joins
            .iter()
            .enumerate()
            .map(|(k, j)| ast::Join {
                kind: j.kind,
                kw: if j.same_block
                    && j.optional
                    && k.checked_sub(1)
                        .and_then(|p| s.joins.get(p))
                        .is_some_and(|p| p.optional)
                {
                    SAME_BLOCK
                } else {
                    SP
                },
                right: source(&j.right, conv, &mine, embedded),
                on_kw: SP,
                on: if embedded {
                    canon::expr(&join_chain(&j.on))
                } else if one_c {
                    canon::join_condition(&j.on, &mine, &canon::alias_of(&j.right))
                } else {
                    join_chain(&j.on)
                },
                optional: j.optional,
            })
            .collect(),
        span: SP,
    }
}

fn join_chain(on: &[JoinCond]) -> ast::Expr {
    let exprs: Vec<ast::Expr> = on.iter().map(|c| c.expr.clone()).collect();
    and_chain(&exprs).unwrap_or(ast::Expr {
        kind: ExprKind::Lit(ast::Lit::Bool(true)),
        span: SP,
    })
}

pub(crate) fn and_chain(cs: &[ast::Expr]) -> Option<ast::Expr> {
    let (first, rest) = cs.split_first()?;
    if rest.is_empty() {
        return Some(first.clone());
    }
    Some(ast::Expr {
        kind: ExprKind::Chain {
            first: Box::new(first.clone()),
            rest: rest
                .iter()
                .map(|e| ast::ChainOp {
                    op: BinOp::And,
                    op_span: SP,
                    expr: e.clone(),
                })
                .collect(),
        },
        span: SP,
    })
}

pub(crate) fn take_marks(b: &mut ast::Batch) -> HashSet<usize> {
    fn source(s: &mut ast::Source, out: &mut HashSet<usize>) {
        if let ast::TableRef::Subquery { query: q, .. } = &mut s.table {
            query(q, out);
        }
        for j in &mut s.joins {
            if j.kw == SAME_BLOCK {
                j.kw = SP;
                out.insert(&*j as *const ast::Join as usize);
            }
            source(&mut j.right, out);
        }
    }
    fn query(q: &mut ast::Query, out: &mut HashSet<usize>) {
        for p in &mut q.parts {
            for s in p.from.iter_mut().flat_map(|f| f.sources.iter_mut()) {
                source(s, out);
            }
        }
    }
    let depth = b.max_depth;
    let mut out = HashSet::new();
    with_deep_stack(depth, || {
        for st in &mut b.stmts {
            if let ast::Stmt::Query(q) = st {
                query(&mut q.query, &mut out);
            }
        }
        out
    })
}
