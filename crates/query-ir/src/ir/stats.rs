use crate::ir::{AggCall, Expr, ExprKind, Rel, SortKey, StmtS};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrStats {
    pub rels: u64,
    pub exprs: u64,
    pub derefs: u64,
    pub fanout_sum: u64,
    pub tables: u64,
    pub type_refs: u64,
}

enum Node<'a> {
    Rel(&'a Rel),
    Expr(&'a Expr),
}

fn sort_keys<'a>(keys: &'a [SortKey], todo: &mut Vec<Node<'a>>) {
    todo.extend(keys.iter().map(|k| Node::Expr(&k.expr)));
}

fn aggs<'a>(aggs: &'a [AggCall], todo: &mut Vec<Node<'a>>) {
    todo.extend(aggs.iter().filter_map(|a| a.arg.as_ref()).map(Node::Expr));
}

pub fn stats(s: &StmtS) -> IrStats {
    let mut out = IrStats {
        tables: s.tables.len() as u64,
        type_refs: s
            .columns
            .iter()
            .map(|c| (c.ty.refs.len() + c.ty.others.len()) as u64)
            .sum::<u64>()
            + s.sources
                .iter()
                .map(|src| match &src.kind {
                    crate::ir::SrcKind::Subquery { columns }
                    | crate::ir::SrcKind::Temp { columns, .. } => columns
                        .iter()
                        .map(|t| (t.refs.len() + t.others.len()) as u64)
                        .sum::<u64>(),
                    crate::ir::SrcKind::Table(_) => 0,
                })
                .sum::<u64>(),
        ..IrStats::default()
    };
    let mut todo: Vec<Node<'_>> = vec![Node::Rel(&s.root)];
    for n in &s.nested {
        if let Some(r) = &n.rows {
            todo.push(Node::Rel(&r.scan));
            todo.push(Node::Expr(&r.owner));
            todo.push(Node::Expr(&r.number));
            todo.extend(r.exprs.iter().map(Node::Expr));
        }
    }
    while let Some(node) = todo.pop() {
        match node {
            Node::Rel(r) => {
                out.rels += 1;
                match r {
                    Rel::Empty
                    | Rel::Scan { .. }
                    | Rel::TempScan { .. }
                    | Rel::ParamScan { .. } => {}
                    Rel::Subquery { input, .. } => todo.push(Node::Rel(input)),
                    Rel::Join { l, r, on, .. } => {
                        todo.push(Node::Rel(l));
                        todo.push(Node::Rel(r));
                        todo.push(Node::Expr(on));
                    }
                    Rel::Filter { input, pred } => {
                        todo.push(Node::Rel(input));
                        todo.push(Node::Expr(pred));
                    }
                    Rel::Aggregate {
                        input,
                        keys,
                        aggs: calls,
                        ..
                    } => {
                        todo.push(Node::Rel(input));
                        todo.extend(keys.iter().map(Node::Expr));
                        aggs(calls, &mut todo);
                    }
                    Rel::Project { input, exprs } => {
                        todo.push(Node::Rel(input));
                        todo.extend(exprs.iter().map(Node::Expr));
                    }
                    Rel::Distinct(input) | Rel::Top { input, .. } => todo.push(Node::Rel(input)),
                    Rel::Sort { input, keys } | Rel::Hierarchy { input, keys, .. } => {
                        todo.push(Node::Rel(input));
                        sort_keys(keys, &mut todo);
                    }
                    Rel::Union { parts, .. } => todo.extend(parts.iter().map(Node::Rel)),
                }
            }
            Node::Expr(e) => {
                out.exprs += 1;
                out.type_refs += (e.ty.refs.len() + e.ty.others.len()) as u64;
                match &e.kind {
                    ExprKind::Col { .. }
                    | ExprKind::Lit(_)
                    | ExprKind::Param(_)
                    | ExprKind::Value(_)
                    | ExprKind::KeyRef(_)
                    | ExprKind::AggRef(_)
                    | ExprKind::Out(_)
                    | ExprKind::Unit(_) => {}
                    ExprKind::Deref { base, steps } => {
                        out.derefs += steps.len() as u64;
                        out.fanout_sum += steps.iter().map(|d| d.targets.len() as u64).sum::<u64>();
                        todo.push(Node::Expr(base));
                    }
                    ExprKind::OuterKey { orig, .. } => todo.push(Node::Expr(orig)),
                    ExprKind::Neg(x) | ExprKind::Not(x) => todo.push(Node::Expr(x)),
                    ExprKind::Arith { first, steps } => {
                        todo.push(Node::Expr(first));
                        todo.extend(steps.iter().map(|(_, x, _)| Node::Expr(x)));
                    }
                    ExprKind::And(xs) | ExprKind::Or(xs) => todo.extend(xs.iter().map(Node::Expr)),
                    ExprKind::Compare(_, a, b) => {
                        todo.push(Node::Expr(a));
                        todo.push(Node::Expr(b));
                    }
                    ExprKind::InList { l, rhs, .. } => {
                        todo.extend(l.iter().chain(rhs).map(Node::Expr));
                    }
                    ExprKind::InQuery { l, q, .. } => {
                        todo.extend(l.iter().map(Node::Expr));
                        todo.push(Node::Rel(q));
                    }
                    ExprKind::SectionAgg { owner, rel, .. } => {
                        todo.push(Node::Expr(owner));
                        todo.push(Node::Rel(rel));
                    }
                    ExprKind::Between { l, lo, hi, .. } => {
                        todo.push(Node::Expr(l));
                        todo.push(Node::Expr(lo));
                        todo.push(Node::Expr(hi));
                    }
                    ExprKind::IsNull { l, .. } | ExprKind::Refs { l, .. } => {
                        todo.push(Node::Expr(l))
                    }
                    ExprKind::Like { l, pat, .. } => {
                        todo.push(Node::Expr(l));
                        todo.push(Node::Expr(pat));
                    }
                    ExprKind::Case {
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
                    ExprKind::Cast { x, .. } => todo.push(Node::Expr(x)),
                    ExprKind::Func { args, .. } => todo.extend(args.iter().map(Node::Expr)),
                }
            }
        }
    }
    out
}
