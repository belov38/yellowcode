use crate::model::{Operator, Schema, SelectQuery, Source, Stmt, Table};
use bsl_prim::fold_name;
use query_ir::{CompiledStmt, tir};
use query_syntax::ScriptVariant;
use query_syntax::ast::{ExprKind, FuncArgs, ItemKind, Path, SelectItem};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) fn derive(q: &SelectQuery) -> Vec<Option<String>> {
    let Some(op) = q.operators.first() else {
        return Vec::new();
    };
    let mut aliases = HashSet::new();
    for s in &op.sources {
        source_aliases(s, &mut aliases);
    }
    let mut names = Names::default();
    op.fields
        .iter()
        .map(|it| item_name(it, &aliases, &mut names))
        .collect()
}

pub(crate) fn alias_set(sources: &[Source]) -> HashSet<String> {
    let mut out = HashSet::new();
    for s in sources {
        source_aliases(s, &mut out);
    }
    out
}

pub(crate) fn nested(fields: &[SelectItem], aliases: &HashSet<String>) -> Vec<Option<String>> {
    let mut names = Names::default();
    fields
        .iter()
        .map(|it| item_name(it, aliases, &mut names))
        .collect()
}

fn source_aliases(s: &Source, out: &mut HashSet<String>) {
    let implicit = match &s.table {
        Table::Named { path, .. } => path.parts.last().map(|p| p.text.to_string()),
        Table::Subquery(_) | Table::Param(_) => None,
    };
    if let Some(a) = s
        .alias
        .as_ref()
        .map(|a| a.name.text.to_string())
        .or(implicit)
    {
        out.insert(fold_name(&a));
    }
    for j in &s.joins {
        source_aliases(&j.right, out);
    }
}

fn path_name(p: &Path, aliases: &HashSet<String>) -> String {
    let lead = p.parts.len() > 1
        && p.parts
            .first()
            .is_some_and(|f| aliases.contains(&fold_name(&f.text)));
    p.parts
        .iter()
        .skip(usize::from(lead))
        .map(|x| &*x.text)
        .collect()
}

fn item_name(it: &SelectItem, aliases: &HashSet<String>, names: &mut Names) -> Option<String> {
    if let Some(a) = &it.alias {
        return Some(names.alias(&a.name.text));
    }
    let own = match &it.kind {
        ItemKind::Star | ItemKind::PathStar(_) => return None,
        ItemKind::Expr(e) => match &e.kind {
            ExprKind::Path(p) => Some(path_name(p, aliases)),
            ExprKind::Param(p) => Some(p.name.text.to_string()),
            ExprKind::Func {
                func,
                args: FuncArgs::List(args),
                ..
            } if func.is_aggregate() => match args.as_slice() {
                [a] => match &a.kind {
                    ExprKind::Path(p) => Some(path_name(p, aliases)),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        },
        ItemKind::Nested { table, .. } => match &table.kind {
            ExprKind::Path(p) => Some(path_name(p, aliases)),
            _ => None,
        },
        ItemKind::EmptyTable { .. } => None,
    };
    Some(match own {
        Some(n) if !n.is_empty() => names.derived(&n),
        _ => names.unnamed(),
    })
}

#[derive(Default)]
struct Names {
    seen: HashSet<String>,
    suffix: HashMap<String, u32>,
    unnamed: u32,
}

impl Names {
    fn alias(&mut self, name: &str) -> String {
        self.seen.insert(fold_name(name));
        name.to_string()
    }
    fn derived(&mut self, name: &str) -> String {
        let base = fold_name(name);
        if self.seen.insert(base.clone()) {
            return name.to_string();
        }
        let n = self.suffix.entry(base).or_insert(0);
        loop {
            *n += 1;
            let cand = format!("{name}{n}");
            if self.seen.insert(fold_name(&cand)) {
                return cand;
            }
        }
    }
    fn unnamed(&mut self) -> String {
        self.unnamed += 1;
        let name = format!("Поле{}", self.unnamed);
        self.derived(&name)
    }
}

pub(crate) fn from_analysis(
    schema: &mut Schema,
    compiled: &[Arc<CompiledStmt>],
    qs: &query_ir::QuerySchema,
) {
    for (st, c) in schema.batch.iter_mut().zip(compiled) {
        if let (Stmt::Select(q), tir::StmtKind::Query(tq)) = (st, &c.typed.kind) {
            query(q, tq, qs);
        }
    }
}

fn query(q: &mut SelectQuery, tq: &tir::Query, qs: &query_ir::QuerySchema) {
    if let (Some(op), Some(sel)) = (q.operators.first(), tq.parts.first()) {
        let plain = op
            .fields
            .iter()
            .all(|f| matches!(f.kind, ItemKind::Expr(_) | ItemKind::Nested { .. }));
        if plain && op.fields.len() == sel.items.len() {
            q.names = sel.items.iter().map(|i| Some(i.name.clone())).collect();
            param_columns(op, &sel.items, &mut q.names, qs.script());
        }
    }
    for (op, sel) in q.operators.iter_mut().zip(&tq.parts) {
        sources(&mut op.sources, &sel.sources, qs);
    }
}

fn param_columns(
    op: &Operator,
    items: &[tir::Item],
    names: &mut [Option<String>],
    script: ScriptVariant,
) {
    let fields = &op.fields;
    let unknown = |i: usize| {
        items.get(i).is_some_and(|it| {
            matches!(&it.expr.kind, tir::TKind::Field { derefs, .. }
                if derefs.iter().any(|d| d.targets.is_empty()))
        })
    };
    let bare = |f: &SelectItem| {
        f.alias.is_none()
            && matches!(&f.kind, ItemKind::Expr(e) if matches!(e.kind, ExprKind::Param(_)))
    };
    if names.len() != fields.len() || !(fields.iter().any(bare) || (0..fields.len()).any(unknown)) {
        return;
    }
    let word = match script {
        ScriptVariant::Russian => "Поле",
        ScriptVariant::English => "Field",
    };
    let aliases = alias_set(&op.sources);
    let mut st = Names::default();
    for (i, (f, n)) in fields.iter().zip(names.iter_mut()).enumerate() {
        let Some(name) = n.clone() else { continue };
        let unnamed = name
            .strip_prefix(word)
            .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()));
        match &f.kind {
            _ if f.alias.is_some() => {
                st.alias(&name);
            }
            ItemKind::Expr(e) => match &e.kind {
                ExprKind::Param(p) => *n = Some(st.derived(&p.name.text)),
                ExprKind::Path(p) if unknown(i) => {
                    let lead = p.parts.len() > 1
                        && p.parts
                            .first()
                            .is_some_and(|f| aliases.contains(&fold_name(&f.text)));
                    let dotted: Vec<&str> = p
                        .parts
                        .iter()
                        .skip(usize::from(lead))
                        .map(|x| &*x.text)
                        .collect();
                    *n = Some(st.derived(&dotted.join(".")));
                }
                ExprKind::Path(_) => {
                    st.alias(&name);
                }
                _ if unnamed => {
                    st.unnamed += 1;
                    *n = Some(st.derived(&format!("{word}{}", st.unnamed)));
                }
                _ => {
                    st.alias(&name);
                }
            },
            _ => {
                st.alias(&name);
            }
        }
    }
}

fn sources(ms: &mut [Source], ts: &[tir::Source], qs: &query_ir::QuerySchema) {
    for (m, t) in ms.iter_mut().zip(ts) {
        if let (Table::Subquery(q), tir::From::Subquery(tq)) = (&mut m.table, &t.from) {
            query(q, tq, qs);
        }
        for (mj, tj) in m.joins.iter_mut().zip(&t.joins) {
            sources(
                std::slice::from_mut(&mut mj.right),
                std::slice::from_ref(&tj.right),
                qs,
            );
        }
    }
}

pub(crate) fn implicit_sources(q: &SelectQuery, tq: &tir::Query, out: &mut HashSet<usize>) {
    for (op, sel) in q.operators.iter().zip(&tq.parts) {
        implicit_in(&op.sources, &sel.sources, out);
    }
}

fn implicit_in(ms: &[Source], ts: &[tir::Source], out: &mut HashSet<usize>) {
    for (m, t) in ms.iter().zip(ts) {
        if m.alias.is_none() {
            out.insert(t as *const tir::Source as usize);
        }
        if let (Table::Subquery(q), tir::From::Subquery(tq)) = (&m.table, &t.from) {
            implicit_sources(q, tq, out);
        }
        for (mj, tj) in m.joins.iter().zip(&t.joins) {
            implicit_in(
                std::slice::from_ref(&mj.right),
                std::slice::from_ref(&tj.right),
                out,
            );
        }
    }
}
