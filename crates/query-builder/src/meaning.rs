use crate::open::Ctx;
use query_ir::CompiledStmt;
use query_ir::tir::{self, TExpr, TKind};
use query_syntax::ast::{BinOp, OrderDir};
use query_syntax::{ParseMode, QueryError, with_deep_stack};

pub fn fingerprint(text: &str, mode: ParseMode, ctx: &Ctx<'_>) -> Result<Vec<String>, QueryError> {
    let stmts = crate::open::analyze_modes(text, mode, ctx)?;
    let mut auto: Vec<String> = Vec::new();
    for s in &stmts {
        if let tir::StmtKind::Query(q) = &s.typed.kind {
            auto.extend(auto_names(q));
        }
    }
    Ok(stmts
        .iter()
        .map(|s| {
            with_deep_stack(s.typed.max_depth, || {
                let s = normalize(s, &auto);
                erase(&format!("{s:?}"))
            })
        })
        .collect())
}

fn erase(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '"' {
            let n = string_len(rest);
            let (lit, tail) = rest.split_at(n);
            out.push_str(lit);
            rest = tail;
        } else if let Some(n) = span_len(rest) {
            out.push_str("Span");
            rest = rest.get(n..).unwrap_or("");
        } else if let Some(tail) = rest.strip_prefix("max_depth: ") {
            out.push_str("max_depth: _");
            rest = tail.trim_start_matches(|c: char| c.is_ascii_digit());
        } else if let Some(tail) = rest.strip_prefix("Source { alias: ")
            && tail.starts_with('"')
        {
            out.push_str("Source { alias: _");
            rest = tail.get(string_len(tail)..).unwrap_or("");
        } else {
            out.push(c);
            rest = rest.get(c.len_utf8()..).unwrap_or("");
        }
    }
    out
}

fn string_len(s: &str) -> usize {
    let mut escaped = false;
    for (i, c) in s.char_indices().skip(1) {
        match (escaped, c) {
            (true, _) => escaped = false,
            (false, '\\') => escaped = true,
            (false, '"') => return i + 1,
            _ => {}
        }
    }
    s.len()
}

fn span_len(s: &str) -> Option<usize> {
    let t = s.strip_prefix("Span { start: ")?;
    let t = t.trim_start_matches(|c: char| c.is_ascii_digit());
    let t = t.strip_prefix(", end: ")?;
    let t = t.trim_start_matches(|c: char| c.is_ascii_digit());
    let t = t.strip_prefix(" }")?;
    Some(s.len() - t.len())
}

fn normalize(s: &CompiledStmt, auto: &[String]) -> CompiledStmt {
    let mut c = s.clone();
    if let tir::StmtKind::Query(q) = &mut c.typed.kind {
        query(q);
        labels(q, auto);
        let gone = group_params(q);
        c.params.retain(|p| !gone.contains(&p.name));
        c.columns.retain(|col| !is_separator(&col.name));
        blank_columns(&mut c.columns, auto);
        if let Some(query_ir::TempOut::Put(t)) = &mut c.temp_out {
            *t = clean_temp(t, auto);
        }
    }
    c
}

fn is_separator(name: &str) -> bool {
    name == "ОбластьДанныхОсновныеДанные" || name == "ОбластьДанныхВспомогательныеДанные"
}

fn clean_temp(t: &query_ir::TempSchema, auto: &[String]) -> std::sync::Arc<query_ir::TempSchema> {
    let mut t2 = t.clone();
    t2.columns.retain(|c| !is_separator(&c.name));
    for col in &mut t2.columns {
        if col.unknown || is_auto(&col.name, auto) {
            col.name.clear();
        }
    }
    std::sync::Arc::new(t2)
}

fn labels(q: &mut tir::Query, auto: &[String]) {
    let params: Vec<usize> = q
        .parts
        .first()
        .map(|p| {
            let mut flat = Vec::new();
            flat_sources(&p.sources, &mut flat);
            flat.iter()
                .enumerate()
                .filter(|(_, s)| matches!(s.from, tir::From::Param { .. }))
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default();
    let single = q.parts.len() == 1;
    for k in &mut q.order {
        if single {
            k.col = None;
        }
        if let TKind::Field {
            level: 0, source, ..
        } = &k.expr.kind
            && params.contains(&usize::from(*source))
        {
            k.dir = match k.dir {
                OrderDir::Hierarchy => OrderDir::Asc,
                OrderDir::HierarchyDesc => OrderDir::Desc,
                d => d,
            };
        }
    }
    for p in &mut q.parts {
        completed_group(p);
        p.items.retain(|i| !is_separator(&i.name));
        blank_items(&mut p.items, auto);
        for s in &mut p.sources {
            source_labels(s, auto);
        }
    }
}

fn completed_group(p: &mut tir::Select) {
    let fields: Vec<String> = p
        .items
        .iter()
        .filter(|i| !has_agg(&i.expr))
        .map(|i| erase(&format!("{:?}", i.expr.kind)))
        .collect();
    let Some(sets) = &mut p.group else {
        return;
    };
    for set in sets.iter_mut() {
        set.retain(|e| {
            let own = erase(&format!("{:?}", e.kind));
            let plain = matches!(e.kind, TKind::Field { .. });
            !fields
                .iter()
                .any(|f| *f == own || plain && f.contains(&own))
        });
    }
    sets.retain(|s| !s.is_empty());
    if sets.is_empty() {
        p.group = None;
    }
}

fn has_agg(e: &TExpr) -> bool {
    format!("{:?}", e.kind).contains("Agg {")
}

fn group_params(q: &mut tir::Query) -> Vec<String> {
    let mut gone = Vec::new();
    for p in &mut q.parts {
        for set in p.group.iter_mut().flatten() {
            set.retain(|e| match &e.kind {
                TKind::Param(n) => {
                    gone.push(n.clone());
                    false
                }
                _ => true,
            });
        }
        if let Some(sets) = &mut p.group {
            sets.retain(|s| !s.is_empty());
            if sets.is_empty() {
                p.group = None;
            }
        }
    }
    gone
}

fn source_labels(s: &mut tir::Source, auto: &[String]) {
    match &mut s.from {
        tir::From::Temp(t) => *t = clean_temp(t, auto).into(),
        tir::From::Param { schema, .. } => *schema = clean_temp(schema, auto).into(),
        tir::From::Subquery(q) => labels(q, auto),
        tir::From::Table { .. } => {}
    }
    for j in &mut s.joins {
        source_labels(&mut j.right, auto);
    }
}

fn flat_sources<'a>(ts: &'a [tir::Source], out: &mut Vec<&'a tir::Source>) {
    for t in ts {
        out.push(t);
        for j in &t.joins {
            flat_sources(std::slice::from_ref(&j.right), out);
        }
    }
}

fn auto_names(q: &tir::Query) -> Vec<String> {
    q.parts
        .first()
        .map(|p| {
            p.items
                .iter()
                .filter(|i| matches!(i.expr.kind, TKind::Param(_)))
                .map(|i| i.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn is_auto(name: &str, auto: &[String]) -> bool {
    let numbered = name
        .strip_prefix("Поле")
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
    numbered || auto.iter().any(|a| a == name)
}

fn unnumber(names: &mut [&mut String]) {
    let all: std::collections::HashSet<String> =
        names.iter().map(|n| bsl_prim::fold_name(n)).collect();
    for n in names.iter_mut() {
        let base = n.trim_end_matches(|c: char| c.is_ascii_digit());
        if base.len() < n.len() && !base.is_empty() && all.contains(&bsl_prim::fold_name(base)) {
            **n = base.to_string();
        }
    }
}

fn blank_items(v: &mut [tir::Item], auto: &[String]) {
    unnumber(&mut v.iter_mut().map(|i| &mut i.name).collect::<Vec<_>>());
    for it in v {
        if it.unknown || is_auto(&it.name, auto) {
            it.name.clear();
        }
        if let Some(n) = &mut it.nested {
            blank_items(n, auto);
        }
    }
}

fn blank_columns(v: &mut [query_ir::ColumnInfo], auto: &[String]) {
    unnumber(&mut v.iter_mut().map(|c| &mut c.name).collect::<Vec<_>>());
    for c in v {
        if is_auto(&c.name, auto) {
            c.name.clear();
        }
        if let Some(n) = &mut c.nested {
            blank_columns(n, auto);
        }
    }
}

fn query(q: &mut tir::Query) {
    for (i, p) in q.parts.iter_mut().enumerate() {
        select(p, i > 0);
    }
    for k in &mut q.order {
        expr(&mut k.expr);
    }
    if let Some(t) = &mut q.totals {
        for (e, _) in &mut t.items {
            expr(e);
        }
        for p in &mut t.points {
            if let tir::ControlPoint::Field(f) = p {
                expr(&mut f.expr);
                if let Some(per) = &mut f.periods {
                    per.begin
                        .iter_mut()
                        .chain(per.end.iter_mut())
                        .for_each(expr);
                }
            }
        }
    }
}

fn items(v: &mut [tir::Item], clear: bool) {
    for it in v {
        if clear {
            it.name.clear();
        }
        expr(&mut it.expr);
        if let Some(n) = &mut it.nested {
            items(n, clear);
        }
    }
}

fn select(s: &mut tir::Select, later_part: bool) {
    items(&mut s.items, later_part);
    for src in &mut s.sources {
        source(src);
    }
    for e in s.where_.iter_mut().chain(s.having.iter_mut()) {
        expr(e);
    }
    s.group.iter_mut().flatten().flatten().for_each(expr);
}

fn source(s: &mut tir::Source) {
    match &mut s.from {
        tir::From::Table { args, .. } => {
            while args.last().is_some_and(|a| a.is_none()) {
                args.pop();
            }
            args.iter_mut().flatten().for_each(expr)
        }
        tir::From::Subquery(q) => query(q),
        tir::From::Temp(_) | tir::From::Param { .. } => {}
    }
    for j in &mut s.joins {
        source(&mut j.right);
        expr(&mut j.on);
    }
}

fn expr(e: &mut TExpr) {
    match &mut e.kind {
        TKind::Const(_) | TKind::Value(_) | TKind::Param(_) | TKind::Unit(_) => {}
        TKind::Field { derefs, .. } => resolved(derefs),
        TKind::Neg(a) | TKind::Not(a) | TKind::IsNull { l: a, .. } | TKind::Refs { l: a, .. } => {
            expr(a)
        }
        TKind::Cast { e: a, derefs, .. } => {
            resolved(derefs);
            expr(a);
        }
        TKind::Arith { first, steps } => {
            expr(first);
            steps.iter_mut().for_each(|s| expr(&mut s.rhs));
        }
        TKind::And(v) | TKind::Or(v) | TKind::Func { args: v, .. } => v.iter_mut().for_each(expr),
        TKind::Compare(_, a, b)
        | TKind::Like {
            l: a, pattern: b, ..
        } => {
            expr(a);
            expr(b);
        }
        TKind::InList { l, rhs, .. } => l.iter_mut().chain(rhs.iter_mut()).for_each(expr),
        TKind::InQuery { l, q, .. } => {
            l.iter_mut().for_each(expr);
            query(q);
        }
        TKind::Between { l, lo, hi, .. } => {
            expr(l);
            expr(lo);
            expr(hi);
        }
        TKind::Case {
            operand,
            whens,
            else_,
        } => {
            operand
                .iter_mut()
                .chain(else_.iter_mut())
                .for_each(|x| expr(x));
            for (c, t) in whens {
                expr(c);
                expr(t);
            }
        }
        TKind::Agg { arg, .. } => arg.iter_mut().for_each(|x| expr(x)),
    }
    not_in(e);
    flatten(e);
    left_arith(e);
}

fn left_arith(e: &mut TExpr) {
    let TKind::Arith { first, steps } = &mut e.kind else {
        return;
    };
    let level = |op: &BinOp| matches!(op, BinOp::Mul | BinOp::Div);
    while let TKind::Arith {
        first: f2,
        steps: s2,
    } = &mut first.kind
        && let Some(outer) = steps.first().map(|s| level(&s.op))
        && s2.iter().all(|s| level(&s.op) == outer)
    {
        let (f2, mut s2) = (
            std::mem::replace(&mut **f2, placeholder()),
            std::mem::take(s2),
        );
        s2.append(steps);
        *steps = s2;
        **first = f2;
    }
}

fn placeholder() -> TExpr {
    TExpr {
        kind: TKind::Param(String::new()),
        ty: Default::default(),
        span: Default::default(),
    }
}

fn flatten(e: &mut TExpr) {
    let is_or = matches!(e.kind, TKind::Or(_));
    let (TKind::And(v) | TKind::Or(v)) = &mut e.kind else {
        return;
    };
    let same = |x: &TExpr| match &x.kind {
        TKind::Or(_) => is_or,
        TKind::And(_) => !is_or,
        _ => false,
    };
    if !v.iter().any(same) {
        return;
    }
    for x in std::mem::take(v) {
        if same(&x) {
            let (TKind::And(inner) | TKind::Or(inner)) = x.kind else {
                continue;
            };
            v.extend(inner);
        } else {
            v.push(x);
        }
    }
}

fn resolved(derefs: &mut [tir::Deref]) {
    for d in derefs.iter_mut().filter(|d| !d.targets.is_empty()) {
        d.name.clear();
    }
}

fn not_in(e: &mut TExpr) {
    let negated = match &e.kind {
        TKind::Not(inner) => matches!(
            inner.kind,
            TKind::InList { not: false, .. } | TKind::InQuery { not: false, .. }
        ),
        _ => false,
    };
    if !negated {
        return;
    }
    let kind = std::mem::replace(&mut e.kind, TKind::Param(String::new()));
    let TKind::Not(inner) = kind else { return };
    e.kind = match inner.kind {
        TKind::InList {
            l, hierarchy, rhs, ..
        } => TKind::InList {
            l,
            not: true,
            hierarchy,
            rhs,
        },
        TKind::InQuery {
            l, hierarchy, q, ..
        } => TKind::InQuery {
            l,
            not: true,
            hierarchy,
            q,
        },
        other => TKind::Not(Box::new(TExpr {
            kind: other,
            ty: inner.ty,
            span: inner.span,
        })),
    };
}
