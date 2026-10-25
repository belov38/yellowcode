use bsl_prim::fold_name;
use query_syntax::ast::{self, Expr, ExprKind, FuncArgs, GroupKind, InRhs, ItemKind, Path};

pub(crate) fn paths_mut(e: &mut Expr, f: &mut dyn FnMut(&mut Path, bool)) {
    match &mut e.kind {
        ExprKind::Lit(_)
        | ExprKind::Param(_)
        | ExprKind::TypeOf { .. }
        | ExprKind::Value { .. }
        | ExprKind::Dcs(_) => {}
        ExprKind::Path(p) => f(p, false),
        ExprKind::Chain { first, rest } => {
            paths_mut(first, f);
            for o in rest {
                paths_mut(&mut o.expr, f);
            }
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Cast { expr, .. }
        | ExprKind::Deref { expr, .. }
        | ExprKind::Paren(expr) => paths_mut(expr, f),
        ExprKind::Compare { l, r, .. } => {
            paths_mut(l, f);
            paths_mut(r, f);
        }
        ExprKind::In { l, rhs, .. } => {
            paths_mut(l, f);
            if let InRhs::List(v) = rhs {
                v.iter_mut().for_each(|x| paths_mut(x, f));
            }
        }
        ExprKind::Between { l, lo, hi, .. } => {
            paths_mut(l, f);
            paths_mut(lo, f);
            paths_mut(hi, f);
        }
        ExprKind::IsNull { l, .. } | ExprKind::Refs { l, .. } => paths_mut(l, f),
        ExprKind::Like { l, pattern, .. } => {
            paths_mut(l, f);
            paths_mut(pattern, f);
        }
        ExprKind::Tuple(v) | ExprKind::DateTime { args: v, .. } => {
            v.iter_mut().for_each(|x| paths_mut(x, f))
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
            ..
        } => {
            if let Some(o) = operand {
                paths_mut(o, f);
            }
            for w in whens {
                paths_mut(&mut w.cond, f);
                paths_mut(&mut w.then, f);
            }
            if let Some(x) = else_ {
                paths_mut(x, f);
            }
        }
        ExprKind::Func { args, .. } => {
            if let FuncArgs::List(v) = args {
                v.iter_mut().for_each(|x| paths_mut(x, f));
            }
        }
    }
}

pub(crate) fn item_paths_mut(it: &mut ast::SelectItem, f: &mut dyn FnMut(&mut Path, bool)) {
    match &mut it.kind {
        ItemKind::Expr(e) => paths_mut(e, f),
        ItemKind::PathStar(p) => f(p, true),
        ItemKind::Nested { table, fields } => {
            paths_mut(table, f);
            fields.iter_mut().for_each(|x| item_paths_mut(x, f));
        }
        ItemKind::Star | ItemKind::EmptyTable { .. } => {}
    }
}

pub(crate) fn group_paths_mut(g: &mut GroupKind, f: &mut dyn FnMut(&mut Path, bool)) {
    match g {
        GroupKind::Exprs(es) => es.iter_mut().for_each(|e| paths_mut(e, f)),
        GroupKind::Sets(sets) => sets.iter_mut().flatten().for_each(|e| paths_mut(e, f)),
    }
}

pub(crate) fn leads(p: &Path, star: bool, key: &str) -> bool {
    (p.parts.len() >= 2 || star) && p.parts.first().is_some_and(|f| fold_name(&f.text) == key)
}

pub(crate) fn expr_refers(e: &Expr, key: &str) -> bool {
    let mut c = e.clone();
    let mut hit = false;
    paths_mut(&mut c, &mut |p, star| hit |= leads(p, star, key));
    hit
}

pub(crate) fn item_refers(it: &ast::SelectItem, key: &str) -> bool {
    let mut c = it.clone();
    let mut hit = false;
    item_paths_mut(&mut c, &mut |p, star| hit |= leads(p, star, key));
    hit
}

pub(crate) fn rename_expr(e: &mut Expr, key: &str, to: &str) {
    paths_mut(e, &mut |p, star| rename_path(p, star, key, to));
}

pub(crate) fn rename_item(it: &mut ast::SelectItem, key: &str, to: &str) {
    item_paths_mut(it, &mut |p, star| rename_path(p, star, key, to));
}

pub(crate) fn rename_group(g: &mut GroupKind, key: &str, to: &str) {
    group_paths_mut(g, &mut |p, star| rename_path(p, star, key, to));
}

fn rename_path(p: &mut Path, star: bool, key: &str, to: &str) {
    if leads(p, star, key)
        && let Some(f) = p.parts.first_mut()
    {
        f.text = to.into();
    }
}
