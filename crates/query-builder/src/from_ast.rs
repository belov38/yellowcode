use crate::error::{OpenError, Shape};
use crate::model::*;
use crate::names;
use query_syntax::ast::{self, BinOp, ExprKind};
use query_syntax::{ParseMode, ScriptVariant, parse_batch, with_deep_stack};

pub fn parse(text: &str, mode: ParseMode) -> Result<Schema, OpenError> {
    let mut b = parse_batch(text, mode).map_err(OpenError::Syntax)?;
    let text: &str = text;
    let max_depth = b.max_depth;
    with_deep_stack(max_depth, || {
        crate::periods::batch(&mut b, text);
        crate::totals::batch(&mut b);
    });
    let stmts = std::mem::take(&mut b.stmts);
    let batch = with_deep_stack(max_depth, move || {
        stmts
            .into_iter()
            .enumerate()
            .map(|(i, s)| stmt(s, text).map_err(|what| OpenError::Shape { stmt: i, what }))
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(Schema {
        batch,
        mode,
        script: ScriptVariant::Russian,
        max_depth,
    })
}

fn stmt(s: ast::Stmt, text: &str) -> Result<Stmt, Shape> {
    match s {
        ast::Stmt::Drop(d) => Ok(Stmt::Drop(d.name.text.into())),
        ast::Stmt::Query(q) => Ok(Stmt::Select(query_in(q.query, text)?)),
    }
}

pub(crate) fn query(q: ast::Query) -> Result<SelectQuery, Shape> {
    query_in(q, "")
}

fn query_in(q: ast::Query, text: &str) -> Result<SelectQuery, Shape> {
    let ast::Query {
        parts,
        unions,
        order_by,
        autoorder,
        totals,
        dcs,
        ..
    } = q;
    let last = parts.len().saturating_sub(1);
    let width = parts.first().map_or(0, |p| p.items.len());
    let (mut allowed, mut into, mut index) = (false, None, None);
    let mut operators = Vec::with_capacity(parts.len());
    for (i, p) in parts.into_iter().enumerate() {
        if p.items.len() != width {
            return Err(Shape::UnionWidth);
        }
        if i == 0 {
            allowed = p.allowed.is_some();
        }
        if let Some(c) = p.into {
            if i != 0 {
                return Err(Shape::IntoNotFirst);
            }
            into = Some(Placement {
                kind: c.kind,
                table: c.name.text.into(),
            });
        }
        if let Some(ix) = p.index_by {
            if i != last {
                return Err(Shape::IndexNotLast);
            }
            index = Some(ix.kind);
        }
        let sources = match p.from {
            Some(f) => f
                .sources
                .into_iter()
                .map(|s| source(s, text))
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        operators.push(Operator {
            union_all: i
                .checked_sub(1)
                .and_then(|j| unions.get(j))
                .is_some_and(|u| u.all),
            distinct: p.distinct.is_some(),
            top: p.top.map(|t| t.count.digits.into()),
            fields: p.items,
            sources,
            filter: p.where_.map(|c| conjuncts(c.expr)).unwrap_or_default(),
            having: p.having.map(|c| conjuncts(c.expr)).unwrap_or_default(),
            group: p.group_by.map(|g| g.kind),
            for_update: p.for_update.map(|f| f.tables),
            dcs: p.dcs,
        });
    }
    let mut out = SelectQuery {
        operators,
        allowed,
        into,
        order: order_by.map(|o| o.items).unwrap_or_default(),
        autoorder: autoorder.is_some(),
        totals: totals.map(|t| Totals {
            items: t.items,
            points: t.points,
        }),
        index,
        dcs,
        names: Vec::new(),
    };
    out.names = names::derive(&out);
    Ok(out)
}

fn source(s: ast::Source, text: &str) -> Result<Source, Shape> {
    let table = match s.table {
        ast::TableRef::Named { path, args } => Table::Named {
            path,
            args: args.map(|a| a.args),
        },
        ast::TableRef::Subquery { query: q, .. } => Table::Subquery(Box::new(query_in(*q, text)?)),
        ast::TableRef::Param(p) => Table::Param(p),
    };
    let mut prev_end: Option<u32> = None;
    let mut joins = Vec::with_capacity(s.joins.len());
    for j in s.joins {
        let same_block = prev_end.is_some_and(|e| {
            text.get(e as usize..j.kw.start as usize)
                .is_some_and(|gap| gap.trim().is_empty())
        });
        prev_end = j.optional.then_some(j.on.span.end);
        joins.push(Join {
            kind: j.kind,
            right: source(j.right, text)?,
            on: conjuncts(j.on)
                .into_iter()
                .map(|expr| JoinCond { expr, api: false })
                .collect(),
            optional: j.optional,
            same_block: j.optional && same_block,
        });
    }
    Ok(Source {
        table,
        alias: s.alias,
        joins,
    })
}

pub(crate) fn conjuncts(e: ast::Expr) -> Vec<ast::Expr> {
    let mut out = Vec::new();
    push_conjuncts(e, &mut out);
    out
}

fn push_conjuncts(e: ast::Expr, out: &mut Vec<ast::Expr>) {
    let span = e.span;
    match e.kind {
        ExprKind::Chain { first, rest } if rest.first().is_some_and(|o| o.op == BinOp::And) => {
            push_conjuncts(*first, out);
            for o in rest {
                push_conjuncts(o.expr, out);
            }
        }
        ExprKind::Paren(inner) if is_and_chain(&inner) => push_conjuncts(*inner, out),
        kind => out.push(ast::Expr { kind, span }),
    }
}

fn is_and_chain(e: &ast::Expr) -> bool {
    matches!(&e.kind, ExprKind::Chain { rest, .. } if rest.first().is_some_and(|o| o.op == BinOp::And))
}
