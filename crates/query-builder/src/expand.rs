use crate::model::*;
use crate::spell;
use crate::to_ast::{SP, ident};
use bsl_prim::fold_name;
use query_ir::{CompiledStmt, FieldRole, QuerySchema, tir};
use query_syntax::ast::{self, Expr, ExprKind, ItemKind, Path, SelectItem};
use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

struct Cx<'a> {
    qs: &'a QuerySchema,
    ru: bool,
    dcs: bool,
}

pub(crate) fn apply(
    schema: &mut Schema,
    compiled: &[Arc<CompiledStmt>],
    qs: &QuerySchema,
    dcs: bool,
) {
    let cx = Cx {
        qs,
        ru: qs.script() == query_syntax::ScriptVariant::Russian,
        dcs,
    };
    for (st, c) in schema.batch.iter_mut().zip(compiled) {
        if let (Stmt::Select(q), tir::StmtKind::Query(tq)) = (st, &c.typed.kind) {
            query(q, tq, &cx);
        }
    }
}

fn open_stars(op: &mut Operator, flat: &[&tir::Source], aliases: &[String]) {
    let unknown = |alias: &str, column: &str| {
        flat.iter().zip(aliases).any(|(s, a)| {
            a.eq_ignore_ascii_case(alias)
                && match &s.from {
                    tir::From::Param { schema, .. } | tir::From::Temp(schema) => {
                        schema.open
                            || schema
                                .columns
                                .iter()
                                .find(|c| fold_name(&c.name) == fold_name(column))
                                .is_none_or(|c| c.unknown || c.ty.is_none())
                    }
                    _ => false,
                }
        })
    };
    for b in &mut op.dcs {
        if let ast::DcsKind::Where(l) = &mut b.kind {
            for it in &mut l.items {
                let ExprKind::Path(p) = &it.expr.kind else {
                    continue;
                };
                if let [first, second, ..] = p.parts.as_slice()
                    && it.star.is_some()
                    && unknown(&first.text, &second.text)
                {
                    it.star = None;
                }
            }
        }
    }
}

struct Expanded {
    kept: Vec<bool>,
    fields: Vec<Option<Expr>>,
    renamed: Vec<Option<String>>,
}

fn query(q: &mut SelectQuery, tq: &tir::Query, cx: &Cx<'_>) {
    let mut mask: Option<Vec<bool>> = None;
    let mut starred: Vec<Option<Expr>> = Vec::new();
    for (i, (op, sel)) in q.operators.iter_mut().zip(&tq.parts).enumerate() {
        let aliases = source_aliases(&op.sources, &sel.sources, cx);
        let mut flat = Vec::new();
        flat_sources(&sel.sources, &mut flat);
        tabular_fields(op, sel);
        open_stars(op, &flat, &aliases);
        if let Some(Expanded {
            kept,
            fields,
            renamed,
        }) = fields(op, sel, &flat, &aliases, cx)
            && i == 0
        {
            starred = fields;
            q.names = sel
                .items
                .iter()
                .enumerate()
                .zip(&kept)
                .filter(|(_, k)| **k)
                .map(|((n, i), _)| {
                    Some(
                        renamed
                            .get(n)
                            .cloned()
                            .flatten()
                            .unwrap_or_else(|| i.name.clone()),
                    )
                })
                .collect();
            mask = Some(kept);
        }
        sources(&mut op.sources, &sel.sources, cx);
        crate::canon::regroup(op, cx.dcs);
    }
    for (k, tk) in q.order.iter_mut().zip(&tq.order) {
        if let (Some(Some(e)), ExprKind::Path(p)) =
            (tk.col.and_then(|i| starred.get(i)), &k.expr.kind)
            && p.parts.len() == 1
        {
            k.expr = e.clone();
        }
    }
    bare_keys(q, tq);
    if let Some(sel) = tq.parts.first() {
        let mut flat = Vec::new();
        flat_sources(&sel.sources, &mut flat);
        for (k, tk) in q.order.iter_mut().zip(&tq.order) {
            if let tir::TKind::Field {
                level: 0, source, ..
            } = &tk.expr.kind
                && tk.col.is_none()
                && matches!(
                    flat.get(usize::from(*source)).map(|s| &s.from),
                    Some(tir::From::Param { .. })
                )
            {
                k.dir = match k.dir {
                    ast::OrderDir::Hierarchy => ast::OrderDir::Asc,
                    ast::OrderDir::HierarchyDesc => ast::OrderDir::Desc,
                    d => d,
                };
            }
        }
    }
    if let (Some(t), Some(tt), Some(sel)) = (&mut q.totals, &tq.totals, tq.parts.first()) {
        let kept = mask;
        for (p, tp) in t.points.iter_mut().zip(&tt.points) {
            let (ast::ControlPoint::Field { expr, .. }, tir::ControlPoint::Field(f)) = (p, tp)
            else {
                continue;
            };
            let shown = column_of(f, &sel.items).map(|i| match &kept {
                Some(k) => k.iter().take(i).filter(|k| **k).count(),
                None => i,
            });
            if let Some(Some(name)) = shown.and_then(|i| q.names.get(i)) {
                *expr = path_expr(&[name]);
            }
        }
    }
}

fn column_of(f: &tir::TotalsField, items: &[tir::Item]) -> Option<usize> {
    f.col.or_else(|| {
        matches!(f.expr.kind, tir::TKind::Field { .. })
            .then(|| items.iter().position(|i| i.expr.kind == f.expr.kind))
            .flatten()
    })
}

fn flat_sources<'a>(ts: &'a [tir::Source], out: &mut Vec<&'a tir::Source>) {
    for t in ts {
        out.push(t);
        for j in &t.joins {
            flat_sources(std::slice::from_ref(&j.right), out);
        }
    }
}

fn source_aliases(ms: &[Source], ts: &[tir::Source], cx: &Cx<'_>) -> Vec<String> {
    let mut out = Vec::new();
    aliases_in(ms, ts, cx, &mut out);
    out
}

fn aliases_in(ms: &[Source], ts: &[tir::Source], cx: &Cx<'_>, out: &mut Vec<String>) {
    for (m, t) in ms.iter().zip(ts) {
        out.push(spell::spelled_alias(cx.qs, cx.ru, t, m.alias.is_none()));
        for (mj, tj) in m.joins.iter().zip(&t.joins) {
            aliases_in(
                std::slice::from_ref(&mj.right),
                std::slice::from_ref(&tj.right),
                cx,
                out,
            );
        }
    }
}

fn sources(ms: &mut [Source], ts: &[tir::Source], cx: &Cx<'_>) {
    for (m, t) in ms.iter_mut().zip(ts) {
        match (&mut m.table, &t.from) {
            (Table::Named { args, .. }, tir::From::Table { id, .. }) => {
                if let Some(args) = args {
                    let n = cx.qs.table(*id).params.len();
                    if args.len() < n {
                        args.resize(n, None);
                    }
                }
                if m.alias.is_none()
                    && let Some(a) = spell::virtual_alias(cx.qs, *id, cx.ru)
                {
                    m.alias = Some(ast::Alias {
                        name: ident(&a),
                        as_kw: None,
                    });
                }
            }
            (Table::Subquery(q), tir::From::Subquery(tq)) => query(q, tq, cx),
            _ => {}
        }
        for (mj, tj) in m.joins.iter_mut().zip(&t.joins) {
            sources(
                std::slice::from_mut(&mut mj.right),
                std::slice::from_ref(&tj.right),
                cx,
            );
        }
    }
}

pub(crate) fn kept(
    qs: &QuerySchema,
    fields: &[SelectItem],
    sel: &tir::Select,
) -> Option<Vec<bool>> {
    let groups = groups(fields, &sel.items)?;
    let mut flat = Vec::new();
    flat_sources(&sel.sources, &mut flat);
    let mut out = vec![true; sel.items.len()];
    for (f, range) in fields.iter().zip(groups) {
        if !is_star(f) {
            continue;
        }
        for i in range {
            if let Some(it) = sel.items.get(i)
                && separator(qs, it, &flat)
                && let Some(k) = out.get_mut(i)
            {
                *k = false;
            }
        }
    }
    Some(out)
}

fn separator(qs: &QuerySchema, it: &tir::Item, flat: &[&tir::Source]) -> bool {
    let tir::TKind::Field {
        level: 0,
        source,
        col: tir::Col::Field(f),
        ..
    } = &it.expr.kind
    else {
        return false;
    };
    let Some(tir::From::Table { id, .. }) = flat.get(usize::from(*source)).map(|s| &s.from) else {
        return false;
    };
    qs.field(*id, *f).is_some_and(|f| {
        f.role == FieldRole::Common
            && matches!(
                f.name_ru.as_str(),
                "ОбластьДанныхОсновныеДанные" | "ОбластьДанныхВспомогательныеДанные"
            )
    })
}

fn tabular_fields(op: &mut Operator, sel: &tir::Select) {
    let Some(groups) = groups(&op.fields, &sel.items) else {
        return;
    };
    for (f, range) in op.fields.iter_mut().zip(groups) {
        let ItemKind::Expr(e) = &f.kind else { continue };
        if !matches!(e.kind, ExprKind::Path(_)) {
            continue;
        }
        let Some(inner) = sel
            .items
            .get(range.start)
            .and_then(|i| i.nested.as_ref())
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        f.kind = ItemKind::Nested {
            table: e.clone(),
            fields: inner.iter().map(nested_field).collect(),
        };
    }
}

fn nested_field(x: &tir::Item) -> SelectItem {
    SelectItem {
        kind: ItemKind::Expr(path_expr(&[&x.name])),
        alias: Some(as_alias(&x.name)),
        span: SP,
    }
}

fn is_star(it: &SelectItem) -> bool {
    matches!(it.kind, ItemKind::Star | ItemKind::PathStar(_))
}

pub(crate) fn groups(fields: &[SelectItem], items: &[tir::Item]) -> Option<Vec<Range<usize>>> {
    let mut out = Vec::with_capacity(fields.len());
    let mut at = 0usize;
    for f in fields {
        let span = match &f.kind {
            ItemKind::Star => Some(f.span),
            ItemKind::PathStar(p) => Some(p.span()),
            _ => None,
        };
        let end = match span {
            Some(sp) => {
                let mut e = at;
                while items.get(e).is_some_and(|i| i.expr.span == sp) {
                    e += 1;
                }
                e
            }
            None => {
                if at >= items.len() {
                    return None;
                }
                at + 1
            }
        };
        out.push(at..end);
        at = end;
    }
    (at == items.len()).then_some(out)
}

fn fields(
    op: &mut Operator,
    sel: &tir::Select,
    flat: &[&tir::Source],
    aliases: &[String],
    cx: &Cx<'_>,
) -> Option<Expanded> {
    if !op.fields.iter().any(is_star) {
        return None;
    }
    let groups = groups(&op.fields, &sel.items)?;
    let kept = kept(cx.qs, &op.fields, sel)?;
    let mut out = Vec::with_capacity(sel.items.len());
    let mut starred = vec![None; sel.items.len()];
    let mut renamed: Vec<Option<String>> = vec![None; sel.items.len()];
    let explicit: HashSet<String> = op
        .fields
        .iter()
        .filter(|f| !is_star(f))
        .filter_map(|f| f.alias.as_ref().map(|a| fold_name(&a.name.text)))
        .collect();
    let mut taken: HashSet<String> = explicit.clone();
    for (f, range) in op.fields.iter().zip(groups) {
        if !is_star(f) {
            out.push(f.clone());
            continue;
        }
        for i in range {
            let it = sel.items.get(i)?;
            if kept.get(i) == Some(&false) {
                continue;
            }
            let mut x = star_item(it, flat, aliases, cx)?;
            if let (Some(slot), ItemKind::Expr(e)) = (starred.get_mut(i), &x.kind) {
                *slot = Some(e.clone());
            }
            if let Some(a) = &mut x.alias {
                let base = a.name.text.to_string();
                if explicit.contains(&fold_name(&base)) {
                    let mut n = 0;
                    let name = loop {
                        n += 1;
                        let cand = format!("{base}{n}");
                        if !taken.contains(&fold_name(&cand)) {
                            break cand;
                        }
                    };
                    taken.insert(fold_name(&name));
                    a.name = ident(&name);
                    if let Some(slot) = renamed.get_mut(i) {
                        *slot = Some(name);
                    }
                } else {
                    taken.insert(fold_name(&base));
                }
            }
            out.push(x);
        }
    }
    op.fields = out;
    Some(Expanded {
        kept,
        fields: starred,
        renamed,
    })
}

fn star_item(
    it: &tir::Item,
    flat: &[&tir::Source],
    aliases: &[String],
    cx: &Cx<'_>,
) -> Option<SelectItem> {
    let tir::TKind::Field {
        level: 0,
        source,
        col,
        derefs,
    } = &it.expr.kind
    else {
        return None;
    };
    if !derefs.is_empty() {
        return None;
    }
    let si = usize::from(*source);
    let src = flat.get(si)?;
    let alias = aliases.get(si)?;
    let field = raw_name(src, *col, it, cx);
    let item = match &it.nested {
        Some(inner) => SelectItem {
            kind: ItemKind::Nested {
                table: path_expr(&[alias, &field]),
                fields: inner.iter().map(nested_field).collect(),
            },
            alias: Some(as_alias(&it.name)),
            span: SP,
        },
        None => SelectItem {
            kind: ItemKind::Expr(path_expr(&[alias, &field])),
            alias: Some(as_alias(&it.name)),
            span: SP,
        },
    };
    Some(item)
}

fn raw_name(src: &tir::Source, col: tir::Col, it: &tir::Item, cx: &Cx<'_>) -> String {
    let own = match (&src.from, col) {
        (tir::From::Table { id, .. }, tir::Col::Field(f)) => cx
            .qs
            .field(*id, f)
            .map(|f| if cx.ru { &f.name_ru } else { &f.name_en }.clone()),
        (tir::From::Temp(s), tir::Col::Column(i)) => {
            s.columns.get(usize::from(i)).map(|c| c.name.clone())
        }
        (tir::From::Param { schema, .. }, tir::Col::Column(i)) => {
            schema.columns.get(usize::from(i)).map(|c| c.name.clone())
        }
        (tir::From::Subquery(q), tir::Col::Column(i)) => q
            .parts
            .first()
            .and_then(|p| p.items.get(usize::from(i)))
            .map(|i| i.name.clone()),
        _ => None,
    };
    own.unwrap_or_else(|| it.name.clone())
}

fn path_expr(parts: &[&str]) -> Expr {
    Expr {
        kind: ExprKind::Path(Path {
            parts: parts.iter().map(|p| ident(p)).collect(),
        }),
        span: SP,
    }
}

fn as_alias(name: &str) -> ast::Alias {
    ast::Alias {
        name: ident(name),
        as_kw: Some(SP),
    }
}

fn column_field(q: &SelectQuery, tq: &tir::Query, col: usize, key: &Expr) -> Option<Expr> {
    let op = q.operators.first()?;
    if op.fields.len() != tq.parts.first()?.items.len() {
        return None;
    }
    let it = op.fields.get(col)?;
    match &it.kind {
        ItemKind::Expr(e)
            if !aliased(it, key) && matches!(&e.kind, ExprKind::Path(p) if p.parts.len() >= 2) =>
        {
            Some(e.clone())
        }
        _ => None,
    }
}

fn is_bare(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Path(p) if p.parts.len() == 1)
}

fn bare_keys(q: &mut SelectQuery, tq: &tir::Query) {
    let order: Vec<(usize, Expr)> = q
        .order
        .iter()
        .zip(&tq.order)
        .enumerate()
        .filter(|(_, (k, _))| is_bare(&k.expr))
        .filter_map(|(i, (k, tk))| Some((i, column_field(q, tq, tk.col?, &k.expr)?)))
        .collect();
    for (i, e) in order {
        if let Some(k) = q.order.get_mut(i) {
            k.expr = e;
        }
    }
    let sets: Vec<Vec<usize>> = tq
        .parts
        .iter()
        .find(|p| !p.index_by.is_empty())
        .map(|p| p.index_by.clone())
        .unwrap_or_default();
    let Some(mut index) = q.index.take() else {
        return;
    };
    let sets_mut: Vec<&mut Vec<Expr>> = match &mut index {
        ast::IndexKind::Fields { fields, .. } => vec![fields],
        ast::IndexKind::Sets(s) => s.iter_mut().map(|s| &mut s.fields).collect(),
    };
    for (fs, cols) in sets_mut.into_iter().zip(&sets) {
        for (f, c) in fs.iter_mut().zip(cols) {
            if is_bare(f)
                && let Some(e) = column_field(q, tq, *c, f)
            {
                *f = e;
            }
        }
    }
    q.index = Some(index);
}

fn aliased(it: &SelectItem, key: &Expr) -> bool {
    let (Some(a), ExprKind::Path(p)) = (&it.alias, &key.kind) else {
        return false;
    };
    p.parts
        .first()
        .is_some_and(|f| bsl_prim::fold_name(&f.text) == bsl_prim::fold_name(&a.name.text))
}
