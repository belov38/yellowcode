use super::paths::{expr_refers, item_refers, rename_expr, rename_group, rename_item};
use super::{
    JoinType, Op, OpError, QueryRef, alias_of, default_alias, find_path, null_item, operator_mut,
    query_mut, shown_alias, source_at_mut, unique,
};
use crate::from_ast::conjuncts;
use crate::model::*;
use crate::open::Ctx;
use crate::{names, parse};
use bsl_prim::fold_name;
use query_ir::{FieldRole, QTableId, QuerySchema, StdRole};
use query_syntax::ast::{self, ExprKind, GroupKind};
use query_syntax::{MAX_DEPTH, MsgId, QueryError, ScriptVariant};
use std::collections::HashSet;

pub(super) fn apply(s: &mut Schema, op: &Op, ctx: &Ctx<'_>) -> Result<(), OpError> {
    match op {
        Op::AddSelect { at } => insert_stmt(s, *at, Stmt::Select(SelectQuery::empty())),
        Op::AddDrop { table, at } => insert_stmt(s, *at, Stmt::Drop(table.clone())),
        Op::RemoveStmt { stmt } => {
            if *stmt >= s.batch.len() {
                return Err(OpError::NoStmt(*stmt));
            }
            s.batch.remove(*stmt);
            Ok(())
        }
        Op::MoveStmt { stmt, to } => {
            if *stmt >= s.batch.len() {
                return Err(OpError::NoStmt(*stmt));
            }
            if *to >= s.batch.len() {
                return Err(OpError::NoStmt(*to));
            }
            let st = s.batch.remove(*stmt);
            s.batch.insert(*to, st);
            Ok(())
        }
        Op::CopyStmt { stmt } => {
            let copy = s.batch.get(*stmt).cloned().ok_or(OpError::NoStmt(*stmt))?;
            s.batch.push(copy);
            Ok(())
        }
        Op::SetDropTable { stmt, table } => match s.batch.get_mut(*stmt) {
            Some(Stmt::Drop(t)) => {
                t.clone_from(table);
                Ok(())
            }
            Some(Stmt::Select(_)) => Err(OpError::NoItem {
                what: "drop statement",
                index: *stmt,
            }),
            None => Err(OpError::NoStmt(*stmt)),
        },
        Op::SetQueryText { q, text } => set_query_text(s, q, text),
        Op::AddOperator { q } => {
            let query = query_mut(s, q)?;
            let n = query.operators.first().map_or(0, |o| o.fields.len());
            let mut o = Operator::empty();
            o.fields = (0..n).map(|_| null_item()).collect();
            query.operators.push(o);
            Ok(())
        }
        Op::RemoveOperator { q, op } => {
            let query = query_mut(s, q)?;
            if *op >= query.operators.len() {
                return Err(OpError::NoOperator(*op));
            }
            if query.operators.len() == 1 {
                return Err(OpError::LastOperator);
            }
            query.operators.remove(*op);
            Ok(())
        }
        Op::SetUnionAll { q, op, all } => {
            operator_mut(s, q, *op)?.union_all = *all;
            Ok(())
        }
        Op::AddTable {
            q,
            op,
            table,
            alias,
        } => add_table(s, ctx, q, *op, table, alias.as_deref()),
        Op::AddSubquery { q, op, alias } => add_subquery(s, q, *op, alias.as_deref()),
        Op::RemoveSource { q, op, alias } => remove_source(s, q, *op, alias),
        Op::SetSourceAlias { q, op, alias, to } => set_source_alias(s, q, *op, alias, to),
        Op::SetTableParam {
            q,
            op,
            alias,
            index,
            expr,
        } => set_table_param(s, ctx, q, *op, alias, *index, expr.as_deref()),
        Op::AddJoin {
            q,
            op,
            left,
            right,
            kind,
            on,
        } => add_join(s, q, *op, left, right, *kind, on),
        Op::RemoveJoin { q, op, right } => remove_join(s, q, *op, right),
        Op::SetJoin {
            q,
            op,
            right,
            kind,
            on,
        } => set_join(s, q, *op, right, *kind, on.as_deref()),
        _ => Err(OpError::Unsupported("not an operation of the structure")),
    }
}

fn insert_stmt(s: &mut Schema, at: Option<usize>, st: Stmt) -> Result<(), OpError> {
    match at {
        None => s.batch.push(st),
        Some(i) if i <= s.batch.len() => s.batch.insert(i, st),
        Some(i) => return Err(OpError::NoStmt(i)),
    }
    Ok(())
}

fn set_query_text(s: &mut Schema, q: &QueryRef, text: &str) -> Result<(), OpError> {
    let mut parsed = parse(text, s.mode).map_err(|e| match e {
        crate::OpenError::Syntax(e) | crate::OpenError::Semantic(e) => OpError::Expr(e),
        crate::OpenError::Shape { what, .. } => OpError::Shape(what),
    })?;
    s.max_depth = (s.max_depth + parsed.max_depth).min(MAX_DEPTH);
    let mut stmts = std::mem::take(&mut parsed.batch);
    if q.nested.is_empty() {
        if q.stmt >= s.batch.len() {
            return Err(OpError::NoStmt(q.stmt));
        }
        s.batch.splice(q.stmt..=q.stmt, stmts);
        return Ok(());
    }
    let target = query_mut(s, q)?;
    match (stmts.pop(), stmts.is_empty()) {
        (Some(Stmt::Select(new)), true) => {
            *target = new;
            Ok(())
        }
        _ => Err(OpError::NotSelect(q.stmt)),
    }
}

fn syntax_error() -> OpError {
    OpError::Expr(QueryError::new(MsgId::SELECT_ERROR_SYNTAXERROR, None))
}

fn add_table(
    s: &mut Schema,
    ctx: &Ctx<'_>,
    q: &QueryRef,
    oi: usize,
    table: &str,
    alias: Option<&str>,
) -> Result<(), OpError> {
    let e = super::expr(s, table)?;
    let (tbl, parts, base) = match e.kind {
        ExprKind::Path(p) => {
            let parts: Vec<String> = p.parts.iter().map(|x| x.text.to_string()).collect();
            let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
            let base = default_alias(ctx.schema, &refs).unwrap_or_default();
            (
                Table::Named {
                    path: p,
                    args: None,
                },
                parts,
                base,
            )
        }
        ExprKind::Param(p) => {
            let base = p.name.text.to_string();
            (Table::Param(p), Vec::new(), base)
        }
        _ => return Err(syntax_error()),
    };
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    let id = if refs.len() >= 2 {
        Some(ctx.schema.find_table(&refs).ok_or_else(|| {
            OpError::Semantic(QueryError::new(MsgId::SELECT_ERROR_TABLENOTFOUND, None))
        })?)
    } else {
        None
    };
    let (alias, roots) = {
        let op = operator_mut(s, q, oi)?;
        let alias = pick_alias(&op.sources, alias, &base)?;
        let roots: Vec<(Option<QTableId>, String)> = op
            .sources
            .iter()
            .map(|r| (root_table(ctx.schema, r), shown_alias(r)))
            .collect();
        (alias, roots)
    };
    let ru = ctx.schema.script() == ScriptVariant::Russian;
    let mut plan = Plan::Cross;
    if let Some(nid) = id {
        for (i, (rid, ralias)) in roots.iter().enumerate() {
            let Some(rid) = *rid else { continue };
            let above = link_conds(ctx.schema, ru, (nid, &alias), (rid, ralias));
            if !above.is_empty() {
                plan = Plan::Above(i, above);
                break;
            }
            let under = link_conds(ctx.schema, ru, (rid, ralias), (nid, &alias));
            if !under.is_empty() {
                plan = Plan::Under(i, under);
                break;
            }
        }
    }
    let plan = match plan {
        Plan::Cross => Plan::Cross,
        Plan::Above(i, c) => Plan::Above(i, link_exprs(s, c)?),
        Plan::Under(i, c) => Plan::Under(i, link_exprs(s, c)?),
    };
    let new = Source {
        table: tbl,
        alias: Some(alias_of(&alias)),
        joins: Vec::new(),
    };
    let op = operator_mut(s, q, oi)?;
    match plan {
        Plan::Cross => op.sources.push(new),
        Plan::Above(i, on) => {
            let slot = op.sources.get_mut(i).ok_or(OpError::NoSource(alias))?;
            let old = std::mem::replace(slot, new);
            slot.joins.push(left_join(old, on));
        }
        Plan::Under(i, on) => {
            let slot = op.sources.get_mut(i).ok_or(OpError::NoSource(alias))?;
            slot.joins.push(left_join(new, on));
        }
    }
    Ok(())
}

enum Plan<T> {
    Cross,
    Above(usize, T),
    Under(usize, T),
}

fn link_exprs(s: &mut Schema, texts: Vec<String>) -> Result<Vec<JoinCond>, OpError> {
    texts
        .iter()
        .map(|t| {
            Ok(JoinCond {
                expr: super::expr(s, t)?,
                api: false,
            })
        })
        .collect()
}

fn left_join(right: Source, on: Vec<JoinCond>) -> Join {
    Join {
        kind: ast::JoinKind::Left,
        right,
        on,
        optional: false,
        same_block: false,
    }
}

fn root_table(qs: &QuerySchema, r: &Source) -> Option<QTableId> {
    match &r.table {
        Table::Named { path, .. } if path.parts.len() >= 2 => {
            let parts: Vec<&str> = path.parts.iter().map(|p| &*p.text).collect();
            qs.find_table(&parts)
        }
        _ => None,
    }
}

fn link_conds(
    qs: &QuerySchema,
    ru: bool,
    from: (QTableId, &str),
    to: (QTableId, &str),
) -> Vec<String> {
    if from.0 == to.0 {
        return Vec::new();
    }
    let target = qs.table(to.0);
    let Some(atom) = target.reference else {
        return Vec::new();
    };
    let ref_name = target
        .fields
        .iter()
        .find(|f| f.role == FieldRole::Std(StdRole::Ref))
        .map(|f| {
            if ru {
                f.name_ru.clone()
            } else {
                f.name_en.clone()
            }
        })
        .unwrap_or_else(|| if ru { "Ссылка" } else { "Ref" }.to_string());
    qs.table(from.0)
        .fields
        .iter()
        .filter(|f| {
            f.role != FieldRole::Std(StdRole::Ref)
                && f.nested.is_none()
                && f.ty.refs.contains(&atom)
        })
        .map(|f| {
            let name = if ru { &f.name_ru } else { &f.name_en };
            format!("{}.{} = {}.{}", from.1, name, to.1, ref_name)
        })
        .collect()
}

fn pick_alias(sources: &[Source], asked: Option<&str>, base: &str) -> Result<String, OpError> {
    let taken: HashSet<String> = names::alias_set(sources);
    match asked {
        Some(a) if taken.contains(&fold_name(a)) => Err(OpError::AliasTaken(a.to_string())),
        Some(a) => Ok(a.to_string()),
        None => Ok(unique(base, &|c| taken.contains(&fold_name(c)))),
    }
}

fn add_subquery(
    s: &mut Schema,
    q: &QueryRef,
    oi: usize,
    alias: Option<&str>,
) -> Result<(), OpError> {
    let op = operator_mut(s, q, oi)?;
    let alias = pick_alias(&op.sources, alias, "ВложенныйЗапрос")?;
    op.sources.push(Source {
        table: Table::Subquery(Box::new(SelectQuery::empty())),
        alias: Some(alias_of(&alias)),
        joins: Vec::new(),
    });
    Ok(())
}

fn remove_source(s: &mut Schema, q: &QueryRef, oi: usize, alias: &str) -> Result<(), OpError> {
    let key = fold_name(alias);
    let query = query_mut(s, q)?;
    let op = query.operators.get_mut(oi).ok_or(OpError::NoOperator(oi))?;
    let path = find_path(&op.sources, alias).ok_or_else(|| OpError::NoSource(alias.to_string()))?;
    let (last, parent) = path
        .split_last()
        .ok_or_else(|| OpError::NoSource(alias.to_string()))?;
    let removed = if parent.is_empty() {
        let removed = op.sources.remove(*last);
        let children: Vec<Source> = removed.joins.iter().map(|j| j.right.clone()).collect();
        for (k, c) in children.into_iter().enumerate() {
            op.sources.insert(*last + k, c);
        }
        removed
    } else {
        let p = source_at_mut(&mut op.sources, parent)
            .ok_or_else(|| OpError::NoSource(alias.to_string()))?;
        if *last >= p.joins.len() {
            return Err(OpError::NoSource(alias.to_string()));
        }
        let removed = p.joins.remove(*last).right;
        for j in &removed.joins {
            op.sources.push(j.right.clone());
        }
        removed
    };
    drop(removed);
    prune_joins(&mut op.sources, &key);
    op.filter.retain(|e| !expr_refers(e, &key));
    op.having.retain(|e| !expr_refers(e, &key));
    if let Some(g) = &mut op.group {
        let empty = match g {
            GroupKind::Exprs(v) => {
                v.retain(|e| !expr_refers(e, &key));
                v.is_empty()
            }
            GroupKind::Sets(sets) => {
                sets.iter_mut()
                    .for_each(|set| set.retain(|e| !expr_refers(e, &key)));
                sets.iter().all(|set| set.is_empty())
            }
        };
        if empty {
            op.group = None;
        }
    }
    let cols: Vec<usize> = op
        .fields
        .iter()
        .enumerate()
        .filter(|(_, it)| item_refers(it, &key))
        .map(|(i, _)| i)
        .collect();
    if oi == 0 {
        query.order.retain(|o| !expr_refers(&o.expr, &key));
        for &c in cols.iter().rev() {
            for o in &mut query.operators {
                if c < o.fields.len() {
                    o.fields.remove(c);
                }
            }
            if c < query.names.len() {
                query.names.remove(c);
            }
        }
    } else {
        for &c in &cols {
            if let Some(f) = query
                .operators
                .get_mut(oi)
                .and_then(|o| o.fields.get_mut(c))
            {
                *f = null_item();
            }
        }
    }
    Ok(())
}

fn prune_joins(sources: &mut [Source], key: &str) {
    for s in sources {
        for j in &mut s.joins {
            j.on.retain(|c| !expr_refers(&c.expr, key));
            prune_joins(std::slice::from_mut(&mut j.right), key);
        }
    }
}

fn set_source_alias(
    s: &mut Schema,
    q: &QueryRef,
    oi: usize,
    alias: &str,
    to: &str,
) -> Result<(), OpError> {
    let key = fold_name(alias);
    let query = query_mut(s, q)?;
    let op = query.operators.get_mut(oi).ok_or(OpError::NoOperator(oi))?;
    let path = find_path(&op.sources, alias).ok_or_else(|| OpError::NoSource(alias.to_string()))?;
    if fold_name(to) != key && names::alias_set(&op.sources).contains(&fold_name(to)) {
        return Err(OpError::AliasTaken(to.to_string()));
    }
    source_at_mut(&mut op.sources, &path)
        .ok_or_else(|| OpError::NoSource(alias.to_string()))?
        .alias = Some(alias_of(to));
    for it in &mut op.fields {
        rename_item(it, &key, to);
    }
    for e in op.filter.iter_mut().chain(op.having.iter_mut()) {
        rename_expr(e, &key, to);
    }
    if let Some(g) = &mut op.group {
        rename_group(g, &key, to);
    }
    rename_joins(&mut op.sources, &key, to);
    if oi == 0 {
        for o in &mut query.order {
            rename_expr(&mut o.expr, &key, to);
        }
    }
    Ok(())
}

fn rename_joins(sources: &mut [Source], key: &str, to: &str) {
    for s in sources {
        for j in &mut s.joins {
            for c in &mut j.on {
                rename_expr(&mut c.expr, key, to);
            }
            rename_joins(std::slice::from_mut(&mut j.right), key, to);
        }
    }
}

fn set_table_param(
    s: &mut Schema,
    ctx: &Ctx<'_>,
    q: &QueryRef,
    oi: usize,
    alias: &str,
    index: usize,
    text: Option<&str>,
) -> Result<(), OpError> {
    let arg = match text {
        Some(t) => Some(ast::VtArg {
            expr: super::expr(s, t)?,
            dir: None,
            dcs: Vec::new(),
        }),
        None => None,
    };
    let op = operator_mut(s, q, oi)?;
    let src = super::find_source_mut(&mut op.sources, alias)
        .ok_or_else(|| OpError::NoSource(alias.to_string()))?;
    let Table::Named { path, args } = &mut src.table else {
        return Err(OpError::NoItem {
            what: "table parameter",
            index,
        });
    };
    let parts: Vec<&str> = path.parts.iter().map(|p| &*p.text).collect();
    let count = ctx
        .schema
        .find_table(&parts)
        .map_or(0, |t| ctx.schema.table(t).params.len());
    if index >= count {
        return Err(OpError::NoItem {
            what: "table parameter",
            index,
        });
    }
    let list = args.get_or_insert_with(|| vec![None; count]);
    if list.len() < count {
        list.resize(count, None);
    }
    if let Some(slot) = list.get_mut(index) {
        *slot = arg;
    }
    if list.iter().all(Option::is_none) {
        *args = None;
    }
    Ok(())
}

fn join_kind(k: JoinType) -> ast::JoinKind {
    match k {
        JoinType::Inner => ast::JoinKind::Inner,
        JoinType::Left => ast::JoinKind::Left,
        JoinType::Right => ast::JoinKind::Right,
        JoinType::Full => ast::JoinKind::Full,
    }
}

fn add_join(
    s: &mut Schema,
    q: &QueryRef,
    oi: usize,
    left: &str,
    right: &str,
    kind: JoinType,
    on: &str,
) -> Result<(), OpError> {
    let on: Vec<JoinCond> = conjuncts(super::expr(s, on)?)
        .into_iter()
        .map(|expr| JoinCond { expr, api: true })
        .collect();
    let op = operator_mut(s, q, oi)?;
    let lp = find_path(&op.sources, left).ok_or_else(|| OpError::NoSource(left.to_string()))?;
    let rp = find_path(&op.sources, right).ok_or_else(|| OpError::NoSource(right.to_string()))?;
    let (Some(&ri), true) = (rp.first(), rp.len() == 1) else {
        return Err(OpError::NotRoot(right.to_string()));
    };
    if lp.first() == Some(&ri) {
        return Err(OpError::Cycle(right.to_string()));
    }
    let right_src = op.sources.remove(ri);
    let lp: Vec<usize> = lp
        .iter()
        .enumerate()
        .map(|(k, &i)| if k == 0 && i > ri { i - 1 } else { i })
        .collect();
    let l =
        source_at_mut(&mut op.sources, &lp).ok_or_else(|| OpError::NoSource(left.to_string()))?;
    l.joins.push(Join {
        kind: join_kind(kind),
        right: right_src,
        on,
        optional: false,
        same_block: false,
    });
    Ok(())
}

fn remove_join(s: &mut Schema, q: &QueryRef, oi: usize, right: &str) -> Result<(), OpError> {
    let op = operator_mut(s, q, oi)?;
    let path = find_path(&op.sources, right).ok_or_else(|| OpError::NoSource(right.to_string()))?;
    let (last, parent) = path
        .split_last()
        .ok_or_else(|| OpError::NoSource(right.to_string()))?;
    if parent.is_empty() {
        return Err(OpError::NoSource(right.to_string()));
    }
    let p = source_at_mut(&mut op.sources, parent)
        .ok_or_else(|| OpError::NoSource(right.to_string()))?;
    if *last >= p.joins.len() {
        return Err(OpError::NoSource(right.to_string()));
    }
    let j = p.joins.remove(*last);
    op.sources.push(j.right);
    Ok(())
}

fn set_join(
    s: &mut Schema,
    q: &QueryRef,
    oi: usize,
    right: &str,
    kind: Option<JoinType>,
    on: Option<&str>,
) -> Result<(), OpError> {
    let on = match on {
        Some(t) => Some(super::expr(s, t)?),
        None => None,
    };
    let op = operator_mut(s, q, oi)?;
    let path = find_path(&op.sources, right).ok_or_else(|| OpError::NoSource(right.to_string()))?;
    let (last, parent) = path
        .split_last()
        .ok_or_else(|| OpError::NoSource(right.to_string()))?;
    if parent.is_empty() {
        return Err(OpError::NoSource(right.to_string()));
    }
    let p = source_at_mut(&mut op.sources, parent)
        .ok_or_else(|| OpError::NoSource(right.to_string()))?;
    let j = p
        .joins
        .get_mut(*last)
        .ok_or_else(|| OpError::NoSource(right.to_string()))?;
    if let Some(k) = kind {
        j.kind = join_kind(k);
    }
    if let Some(e) = on {
        let c = JoinCond { expr: e, api: true };
        match j.on.first_mut() {
            Some(first) => *first = c,
            None => j.on.push(c),
        }
    }
    Ok(())
}
