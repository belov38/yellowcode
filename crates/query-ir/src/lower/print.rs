use super::{PExpr, RelL, StmtL};
use crate::QuerySchema;
use crate::ir::print::{bin_op, cmp_op, lit_text, type_text};
use ibschema::storage::StorageMap;
use query_syntax::ast::JoinKind;
use query_syntax::with_deep_stack;
use std::fmt::Write;

pub fn print(s: &StmtL, map: &StorageMap, schema: &QuerySchema) -> String {
    with_deep_stack(s.max_depth, || {
        let mut p = Printer {
            map,
            schema,
            out: String::new(),
        };
        p.rel(&s.root);
        for n in &s.nested {
            let _ = write!(p.out, " (nested col{}", n.col);
            if let Some(r) = &n.rows {
                p.w(" ");
                p.rel(&r.rel);
            }
            p.w(")");
        }
        p.out
    })
}

struct Printer<'a> {
    map: &'a StorageMap,
    schema: &'a QuerySchema,
    out: String,
}

impl Printer<'_> {
    fn w(&mut self, t: &str) {
        self.out.push_str(t);
    }

    fn rels<'r>(&mut self, v: impl IntoIterator<Item = &'r RelL>) {
        for r in v {
            self.w(" ");
            self.rel(r);
        }
    }

    fn rel(&mut self, r: &RelL) {
        match r {
            RelL::Empty => self.w("(empty)"),
            RelL::Values { rows } => {
                let _ = write!(self.out, "(values {}x{})", rows.rows.len(), rows.width);
            }
            RelL::TempScan { name, width } => {
                let _ = write!(self.out, "(temp-scan {name} {width})");
            }
            RelL::Scan { table, fields } => {
                let info = self.map.try_table(*table);
                let name = info.map_or("?", |t| t.logical.as_str());
                let _ = write!(self.out, "(scan {name}");
                for f in fields {
                    match info.and_then(|t| t.field(*f)) {
                        Some(x) => {
                            let _ = write!(self.out, " {}", x.name);
                        }
                        None => {
                            let _ = write!(self.out, " ?{}", f.0);
                        }
                    }
                }
                self.w(")");
            }
            RelL::Join {
                kind,
                l,
                r,
                on,
                eq,
                by_id,
            } => {
                let k = match kind {
                    JoinKind::Inner => "inner",
                    JoinKind::Left => "left",
                    JoinKind::Right => "right",
                    JoinKind::Full => "full",
                };
                let _ = write!(self.out, "(join {k}");
                self.rels([&**l, &**r]);
                self.w(" ");
                self.expr(on);
                if !eq.is_empty() {
                    self.w(" (eq");
                    for (a, b) in eq {
                        self.w(" (");
                        self.expr(a);
                        self.w(" ");
                        self.expr(b);
                        self.w(")");
                    }
                    self.w(")");
                }
                if *by_id {
                    self.w(" by-id");
                }
                self.w(")");
            }
            RelL::Filter { input, pred } => {
                self.w("(filter ");
                self.rel(input);
                self.w(" ");
                self.expr(pred);
                self.w(")");
            }
            RelL::Aggregate {
                input,
                keys,
                sets,
                aggs,
            } => {
                self.w("(agg ");
                self.rel(input);
                self.w(" (keys");
                self.list_sp(keys);
                self.w(")");
                if let Some(sets) = sets {
                    self.w(" (sets");
                    for set in sets {
                        self.w(" (");
                        for (i, k) in set.iter().enumerate() {
                            if i > 0 {
                                self.w(" ");
                            }
                            let _ = write!(self.out, "{k}");
                        }
                        self.w(")");
                    }
                    self.w(")");
                }
                self.w(" (aggs");
                for a in aggs {
                    let _ = write!(self.out, " ({}", a.f.ru());
                    if a.distinct {
                        self.w(" distinct");
                    }
                    match &a.arg {
                        Some(e) => {
                            self.w(" ");
                            self.expr(e);
                        }
                        None => self.w(" *"),
                    }
                    self.w(")");
                }
                self.w("))");
            }
            RelL::Project { input, exprs } => {
                self.w("(project ");
                self.rel(input);
                self.list_sp(exprs);
                self.w(")");
            }
            RelL::Distinct(x) => {
                self.w("(distinct ");
                self.rel(x);
                self.w(")");
            }
            RelL::Sort { input, keys } => {
                self.w("(sort ");
                self.rel(input);
                self.keys(keys);
                self.w(")");
            }
            RelL::Top { input, n } => {
                let _ = write!(self.out, "(top {n} ");
                self.rel(input);
                self.w(")");
            }
            RelL::Union { parts, all } => {
                self.w("(union");
                if all.iter().all(|a| *a) {
                    self.w(" all");
                } else if all.iter().any(|a| *a) {
                    self.w(" mixed:");
                    for (i, a) in all.iter().enumerate() {
                        if i > 0 {
                            self.w(",");
                        }
                        self.w(if *a { "all" } else { "distinct" });
                    }
                }
                self.rels(parts);
                self.w(")");
            }
            RelL::SemiJoin {
                input,
                l,
                q,
                not,
                correlated,
            } => {
                self.w("(semi");
                if *not {
                    self.w(" not");
                }
                if *correlated {
                    self.w(" correlated");
                }
                self.w(" ");
                self.rel(input);
                self.w(" (");
                self.list(l);
                self.w(") ");
                self.rel(q);
                self.w(")");
            }
            RelL::HierarchyClosure { tables, seeds } => {
                self.w("(hier-closure (");
                for (i, t) in tables.iter().enumerate() {
                    if i > 0 {
                        self.w(" ");
                    }
                    let _ = write!(
                        self.out,
                        "{}:t{}#{}#{}",
                        t.number, t.table.0, t.id.0, t.parent.0
                    );
                }
                self.w(") ");
                match seeds {
                    crate::lower::HierSeeds::Values(v) => {
                        self.w("(values ");
                        self.list(v);
                        self.w(")");
                    }
                    crate::lower::HierSeeds::Rel(r) => self.rel(r),
                }
                self.w(")");
            }
            RelL::HierarchyOrder {
                input,
                id,
                parent,
                keys,
            } => {
                let _ = write!(self.out, "(hierarchy #{id} #{parent} ");
                self.rel(input);
                self.keys(keys);
                self.w(")");
            }
        }
    }

    fn keys(&mut self, keys: &[(PExpr, bool)]) {
        for (e, desc) in keys {
            self.w(" (");
            self.expr(e);
            self.w(if *desc { " desc)" } else { " asc)" });
        }
    }

    fn list(&mut self, v: &[PExpr]) {
        for (i, e) in v.iter().enumerate() {
            if i > 0 {
                self.w(" ");
            }
            self.expr(e);
        }
    }

    fn list_sp(&mut self, v: &[PExpr]) {
        for e in v {
            self.w(" ");
            self.expr(e);
        }
    }

    fn op(&mut self, name: &str, args: &[&PExpr]) {
        let _ = write!(self.out, "({name}");
        for a in args {
            self.w(" ");
            self.expr(a);
        }
        self.w(")");
    }

    fn lhs(&mut self, l: &[PExpr]) {
        if l.len() == 1 {
            self.list(l);
        } else {
            self.w("(");
            self.list(l);
            self.w(")");
        }
    }

    fn expr(&mut self, e: &PExpr) {
        match e {
            PExpr::Col(i) => {
                let _ = write!(self.out, "#{i}");
            }
            PExpr::Outer { depth, col } => {
                let _ = write!(self.out, "outer({depth},#{col})");
            }
            PExpr::Lit(v) => {
                let t = lit_text(self.schema, v);
                self.w(&t);
            }
            PExpr::Set(s) => {
                let _ = write!(self.out, "(set {})", s.values.len());
            }
            PExpr::Neg(x) => self.op("neg", &[x]),
            PExpr::Not(x) => self.op("not", &[x]),
            PExpr::Arith { first, steps, .. } => {
                for (op, _, _) in steps.iter().rev() {
                    let _ = write!(self.out, "({} ", bin_op(*op));
                }
                self.expr(first);
                for (_, rhs, _) in steps {
                    self.w(" ");
                    self.expr(rhs);
                    self.w(")");
                }
            }
            PExpr::And(v) => {
                self.w("(and ");
                self.list(v);
                self.w(")");
            }
            PExpr::Or(v) => {
                self.w("(or ");
                self.list(v);
                self.w(")");
            }
            PExpr::Compare(op, a, b) => self.op(cmp_op(*op), &[a, b]),
            PExpr::InList { l, not, rhs } => {
                self.w(if *not { "(not-in " } else { "(in " });
                self.lhs(l);
                self.w(" (list ");
                self.list(rhs);
                self.w("))");
            }
            PExpr::InQuery {
                l,
                not,
                q,
                correlated,
            } => {
                self.w("(in-query");
                if *not {
                    self.w(" not");
                }
                if *correlated {
                    self.w(" correlated");
                }
                self.w(" (");
                self.list(l);
                self.w(") ");
                self.rel(q);
                self.w(")");
            }
            PExpr::Between { l, not, lo, hi } => {
                self.op(if *not { "not-between" } else { "between" }, &[l, lo, hi]);
            }
            PExpr::IsNull { l, not } => {
                self.op(if *not { "not-null" } else { "is-null" }, &[l]);
            }
            PExpr::Refs { l, number } => {
                self.w("(refs ");
                self.expr(l);
                let _ = write!(self.out, " {number})");
            }
            PExpr::Like { l, not, pat, esc } => {
                self.w(if *not { "(not-like " } else { "(like " });
                self.expr(l);
                self.w(" ");
                self.expr(pat);
                if let Some(c) = esc {
                    let _ = write!(self.out, " esc '{c}'");
                }
                self.w(")");
            }
            PExpr::Case {
                operand,
                whens,
                else_,
            } => {
                self.w("(case");
                if let Some(o) = operand {
                    self.w(" ");
                    self.expr(o);
                }
                for (c, r) in whens {
                    self.w(" (when ");
                    self.expr(c);
                    self.w(" ");
                    self.expr(r);
                    self.w(")");
                }
                if let Some(x) = else_ {
                    self.w(" (else ");
                    self.expr(x);
                    self.w(")");
                }
                self.w(")");
            }
            PExpr::Cast {
                x,
                to,
                num,
                ref_tables,
                load,
            } => {
                self.w(if *load { "(load-cast " } else { "(cast " });
                self.expr(x);
                let t = type_text(self.schema, to, *num);
                let _ = write!(self.out, " {t}");
                if !ref_tables.is_empty() {
                    let _ = write!(self.out, " refs={ref_tables:?}");
                }
                self.w(")");
            }
            PExpr::Func { f, args, .. } => {
                let _ = write!(self.out, "({}", f.ru());
                self.list_sp(args);
                self.w(")");
            }
            PExpr::TypeOf { x, .. } => {
                self.w("(ТИПЗНАЧЕНИЯ ");
                self.expr(x);
                self.w(")");
            }
            PExpr::SectionAgg { owner, rel, .. } => {
                self.w("(section-agg ");
                self.expr(owner);
                self.w(" ");
                self.rel(rel);
                self.w(")");
            }
            PExpr::StoredSize { x, kind } => {
                let _ = write!(self.out, "(РАЗМЕРХРАНИМЫХДАННЫХ {kind:?} ");
                self.expr(x);
                self.w(")");
            }
            PExpr::Unit(k) => self.w(k.ru()),
        }
    }
}
