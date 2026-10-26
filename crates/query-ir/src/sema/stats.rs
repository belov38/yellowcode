use crate::sema::CompiledStmt;
use crate::tir::{ControlPoint, From, Item, Query, Select, Source, StmtKind, TExpr, TKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StmtStats {
    pub nodes: u64,
    pub columns: u64,
    pub fanout_sum: u64,
    pub joins: u64,
    pub subqueries: u64,
    pub sources: u64,
    pub type_refs: u64,
}

fn columns(cols: &[crate::ColumnInfo]) -> u64 {
    cols.iter()
        .map(|c| 1 + c.nested.as_ref().map_or(0, |n| columns(n)))
        .sum()
}

fn column_refs(cols: &[crate::ColumnInfo]) -> u64 {
    cols.iter()
        .map(|c| {
            (c.ty.refs.len() + c.ty.others.len()) as u64
                + c.nested.as_ref().map_or(0, |n| column_refs(n))
        })
        .sum()
}

enum Node<'a> {
    Query(&'a Query),
    Select(&'a Select),
    Item(&'a Item),
    Source(&'a Source),
    Expr(&'a TExpr),
}

pub fn stats(c: &CompiledStmt) -> StmtStats {
    walk(c, &mut |_| {})
}

pub(crate) fn visit_schema_sources(c: &CompiledStmt, f: &mut dyn FnMut(crate::QTableId)) {
    walk(c, f);
}

fn walk(c: &CompiledStmt, source: &mut dyn FnMut(crate::QTableId)) -> StmtStats {
    let s = StmtStats {
        columns: columns(&c.columns),
        type_refs: column_refs(&c.columns),
        ..StmtStats::default()
    };
    let StmtKind::Query(q) = &c.typed.kind else {
        return s;
    };
    walk_nodes(s, vec![Node::Query(q)], source)
}

pub(crate) fn predicate_stats(p: &crate::access::PredicateForm) -> StmtStats {
    let node = match p {
        crate::access::PredicateForm::Short(e) => Node::Expr(e),
        crate::access::PredicateForm::Full { query, .. } => Node::Select(query),
    };
    walk_nodes(StmtStats::default(), vec![node], &mut |_| {})
}

fn walk_nodes(
    mut s: StmtStats,
    mut todo: Vec<Node<'_>>,
    source: &mut dyn FnMut(crate::QTableId),
) -> StmtStats {
    while let Some(node) = todo.pop() {
        match node {
            Node::Query(q) => {
                todo.extend(q.parts.iter().map(Node::Select));
                todo.extend(q.order.iter().map(|k| Node::Expr(&k.expr)));
                if let Some(t) = &q.totals {
                    todo.extend(t.items.iter().map(|(e, _)| Node::Expr(e)));
                    for p in &t.points {
                        if let ControlPoint::Field(f) = p {
                            todo.push(Node::Expr(&f.expr));
                            if let Some(p) = &f.periods {
                                todo.extend(p.begin.iter().map(Node::Expr));
                                todo.extend(p.end.iter().map(Node::Expr));
                            }
                        }
                    }
                }
            }
            Node::Select(sel) => {
                todo.extend(sel.items.iter().map(Node::Item));
                todo.extend(sel.sources.iter().map(Node::Source));
                todo.extend(sel.where_.iter().map(Node::Expr));
                todo.extend(sel.having.iter().map(Node::Expr));
                if let Some(g) = &sel.group {
                    todo.extend(g.iter().flatten().map(Node::Expr));
                }
            }
            Node::Item(it) => {
                todo.push(Node::Expr(&it.expr));
                if let Some(n) = &it.nested {
                    todo.extend(n.iter().map(Node::Item));
                }
            }
            Node::Source(src) => {
                s.joins += src.joins.len() as u64;
                for j in &src.joins {
                    todo.push(Node::Source(&j.right));
                    todo.push(Node::Expr(&j.on));
                }
                match &src.from {
                    From::Table { id, args, .. } => {
                        source(*id);
                        s.sources += 1;
                        todo.extend(args.iter().flatten().map(Node::Expr));
                    }
                    From::Temp(_) | From::Param { .. } => s.sources += 1,
                    From::Subquery(q) => {
                        s.subqueries += 1;
                        todo.push(Node::Query(q));
                    }
                }
            }
            Node::Expr(e) => {
                s.nodes += 1;
                s.type_refs += (e.ty.refs.len() + e.ty.others.len()) as u64;
                match &e.kind {
                    TKind::Const(_) | TKind::Value(_) | TKind::Param(_) | TKind::Unit(_) => {}
                    TKind::Field { derefs, .. } => {
                        s.fanout_sum += derefs.iter().map(|d| d.targets.len() as u64).sum::<u64>();
                    }
                    TKind::Neg(x) | TKind::Not(x) => todo.push(Node::Expr(x)),
                    TKind::Arith { first, steps } => {
                        todo.push(Node::Expr(first));
                        todo.extend(steps.iter().map(|st| Node::Expr(&st.rhs)));
                    }
                    TKind::And(xs) | TKind::Or(xs) => todo.extend(xs.iter().map(Node::Expr)),
                    TKind::Compare(_, a, b) => {
                        todo.push(Node::Expr(a));
                        todo.push(Node::Expr(b));
                    }
                    TKind::InList { l, rhs, .. } => {
                        todo.extend(l.iter().chain(rhs).map(Node::Expr));
                    }
                    TKind::InQuery { l, q, .. } => {
                        s.subqueries += 1;
                        todo.extend(l.iter().map(Node::Expr));
                        todo.push(Node::Query(q));
                    }
                    TKind::Between { l, lo, hi, .. } => {
                        todo.push(Node::Expr(l));
                        todo.push(Node::Expr(lo));
                        todo.push(Node::Expr(hi));
                    }
                    TKind::IsNull { l, .. } | TKind::Refs { l, .. } => todo.push(Node::Expr(l)),
                    TKind::Like { l, pattern, .. } => {
                        todo.push(Node::Expr(l));
                        todo.push(Node::Expr(pattern));
                    }
                    TKind::Case {
                        operand,
                        whens,
                        else_,
                    } => {
                        todo.extend(operand.iter().map(|x| Node::Expr(x)));
                        for (w, t) in whens {
                            todo.push(Node::Expr(w));
                            todo.push(Node::Expr(t));
                        }
                        todo.extend(else_.iter().map(|x| Node::Expr(x)));
                    }
                    TKind::Cast { e, derefs, .. } => {
                        s.fanout_sum += derefs.iter().map(|d| d.targets.len() as u64).sum::<u64>();
                        todo.push(Node::Expr(e));
                    }
                    TKind::Func { args, .. } => todo.extend(args.iter().map(Node::Expr)),
                    TKind::Agg { arg, .. } => todo.extend(arg.iter().map(|x| Node::Expr(x))),
                }
            }
        }
    }
    s
}
