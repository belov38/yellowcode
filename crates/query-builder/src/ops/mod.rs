mod lists;
mod paths;
mod reconcile;
mod structure;

use crate::model::*;
use crate::open::Ctx;
use crate::{Shape, canon, names, spell};
use bsl_prim::fold_name;
use query_syntax::ast::{self, ExprKind, Lit};
use query_syntax::{MAX_DEPTH, QueryError, Span, with_deep_stack};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryRef {
    pub stmt: usize,
    #[serde(default)]
    pub nested: Vec<(usize, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Asc,
    Desc,
    Hierarchy,
    HierarchyDesc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointKind {
    Items,
    Hierarchy,
    HierarchyOnly,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    AddSelect {
        #[serde(default)]
        at: Option<usize>,
    },
    AddDrop {
        table: String,
        #[serde(default)]
        at: Option<usize>,
    },
    RemoveStmt {
        stmt: usize,
    },
    MoveStmt {
        stmt: usize,
        to: usize,
    },
    CopyStmt {
        stmt: usize,
    },
    SetDropTable {
        stmt: usize,
        table: String,
    },
    SetQueryText {
        q: QueryRef,
        text: String,
    },
    AddOperator {
        q: QueryRef,
    },
    RemoveOperator {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
    },
    SetUnionAll {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        all: bool,
    },
    AddTable {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        table: String,
        #[serde(default)]
        alias: Option<String>,
    },
    AddSubquery {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        #[serde(default)]
        alias: Option<String>,
    },
    RemoveSource {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        alias: String,
    },
    SetSourceAlias {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        alias: String,
        to: String,
    },
    SetTableParam {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        alias: String,
        index: usize,
        #[serde(default)]
        expr: Option<String>,
    },
    AddJoin {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        left: String,
        right: String,
        kind: JoinType,
        on: String,
    },
    RemoveJoin {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        right: String,
    },
    SetJoin {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        right: String,
        #[serde(default)]
        kind: Option<JoinType>,
        #[serde(default)]
        on: Option<String>,
    },
    AddField {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        expr: String,
        #[serde(default)]
        alias: Option<String>,
    },
    SetField {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        column: usize,
        expr: String,
    },
    RemoveField {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        column: usize,
    },
    SetAlias {
        q: QueryRef,
        column: usize,
        alias: String,
    },
    MoveColumn {
        q: QueryRef,
        column: usize,
        to: usize,
    },
    AddCondition {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        expr: String,
    },
    SetCondition {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        index: usize,
        expr: String,
    },
    RemoveCondition {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        index: usize,
    },
    AddGroup {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        expr: String,
    },
    RemoveGroup {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        index: usize,
    },
    AddOrder {
        q: QueryRef,
        expr: String,
        #[serde(default)]
        dir: Option<Direction>,
    },
    SetOrderDir {
        q: QueryRef,
        index: usize,
        dir: Direction,
    },
    RemoveOrder {
        q: QueryRef,
        index: usize,
    },
    SetAutoorder {
        q: QueryRef,
        on: bool,
    },
    AddTotal {
        q: QueryRef,
        expr: String,
    },
    RemoveTotal {
        q: QueryRef,
        index: usize,
    },
    AddControlPoint {
        q: QueryRef,
        expr: String,
        #[serde(default)]
        kind: Option<PointKind>,
    },
    RemoveControlPoint {
        q: QueryRef,
        index: usize,
    },
    SetOverall {
        q: QueryRef,
        on: bool,
    },
    AddIndex {
        q: QueryRef,
    },
    AddIndexField {
        q: QueryRef,
        expr: String,
    },
    SetIndexUnique {
        q: QueryRef,
        on: bool,
    },
    SetDistinct {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        on: bool,
    },
    SetTop {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        #[serde(default)]
        n: Option<u64>,
    },
    SetAllowed {
        q: QueryRef,
        on: bool,
    },
    SetForUpdate {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        on: bool,
    },
    AddTableForUpdate {
        q: QueryRef,
        #[serde(rename = "op_index")]
        op: usize,
        table: String,
    },
    SetPlacement {
        q: QueryRef,
        #[serde(default)]
        table: Option<String>,
    },
}

#[derive(Debug)]
pub enum OpError {
    NoStmt(usize),
    NotSelect(usize),
    NoOperator(usize),
    LastOperator,
    NoSource(String),
    AliasTaken(String),
    NotRoot(String),
    Cycle(String),
    NoItem { what: &'static str, index: usize },
    Expr(QueryError),
    Semantic(QueryError),
    Shape(Shape),
    Unsupported(&'static str),
}

pub fn apply(s: &mut Schema, op: &Op, ctx: &Ctx<'_>) -> Result<(), OpError> {
    if !s.unresolved_syntax().is_empty() {
        return Err(OpError::Unsupported("unresolved adjacent DCS parameter"));
    }
    let before = s.deep_clone();
    let text_bytes = format!("{op:?}").len();
    let depth = s
        .max_depth
        .saturating_add(u32::try_from(text_bytes / 2).unwrap_or(MAX_DEPTH))
        .min(MAX_DEPTH);
    let r = with_deep_stack(depth, || match op {
        Op::AddField { .. }
        | Op::SetField { .. }
        | Op::RemoveField { .. }
        | Op::SetAlias { .. }
        | Op::MoveColumn { .. }
        | Op::AddCondition { .. }
        | Op::SetCondition { .. }
        | Op::RemoveCondition { .. }
        | Op::AddGroup { .. }
        | Op::RemoveGroup { .. }
        | Op::AddOrder { .. }
        | Op::SetOrderDir { .. }
        | Op::RemoveOrder { .. }
        | Op::SetAutoorder { .. }
        | Op::AddTotal { .. }
        | Op::RemoveTotal { .. }
        | Op::AddControlPoint { .. }
        | Op::RemoveControlPoint { .. }
        | Op::SetOverall { .. }
        | Op::AddIndex { .. }
        | Op::AddIndexField { .. }
        | Op::SetIndexUnique { .. }
        | Op::SetDistinct { .. }
        | Op::SetTop { .. }
        | Op::SetAllowed { .. }
        | Op::SetForUpdate { .. }
        | Op::AddTableForUpdate { .. }
        | Op::SetPlacement { .. } => lists::apply(s, op, ctx),
        _ => structure::apply(s, op, ctx),
    });
    let r = r.and_then(|()| reconcile::reconcile(s, ctx));
    if r.is_err() {
        *s = before;
    }
    r
}

pub(crate) fn expr(s: &mut Schema, text: &str) -> Result<ast::Expr, OpError> {
    let mut tree = query_syntax::parse_expr(text, s.mode).map_err(OpError::Expr)?;
    s.max_depth = (s.max_depth + tree.max_depth).min(MAX_DEPTH);
    let null = ast::Expr {
        kind: ExprKind::Lit(Lit::Null),
        span: SP,
    };
    Ok(std::mem::replace(&mut tree.expr, null))
}

pub(crate) const SP: Span = crate::to_ast::SP;

pub(crate) fn null_item() -> ast::SelectItem {
    ast::SelectItem {
        kind: ast::ItemKind::Expr(ast::Expr {
            kind: ExprKind::Lit(Lit::Null),
            span: SP,
        }),
        alias: None,
        span: SP,
    }
}

pub(crate) fn is_null_filler(it: &ast::SelectItem) -> bool {
    it.alias.is_none()
        && matches!(&it.kind, ast::ItemKind::Expr(e) if matches!(e.kind, ExprKind::Lit(Lit::Null)))
}

pub fn query_mut<'a>(s: &'a mut Schema, q: &QueryRef) -> Result<&'a mut SelectQuery, OpError> {
    let mut cur = match s.batch.get_mut(q.stmt) {
        Some(Stmt::Select(sq)) => sq,
        Some(Stmt::Drop(_)) => return Err(OpError::NotSelect(q.stmt)),
        None => return Err(OpError::NoStmt(q.stmt)),
    };
    for (oi, alias) in &q.nested {
        let c = cur;
        let op = c.operators.get_mut(*oi).ok_or(OpError::NoOperator(*oi))?;
        let src = find_source_mut(&mut op.sources, alias)
            .ok_or_else(|| OpError::NoSource(alias.clone()))?;
        cur = match &mut src.table {
            Table::Subquery(b) => b,
            _ => return Err(OpError::NoSource(alias.clone())),
        };
    }
    Ok(cur)
}

pub fn operator_mut<'a>(
    s: &'a mut Schema,
    q: &QueryRef,
    op: usize,
) -> Result<&'a mut Operator, OpError> {
    query_mut(s, q)?
        .operators
        .get_mut(op)
        .ok_or(OpError::NoOperator(op))
}

pub fn find_source_mut<'a>(sources: &'a mut [Source], alias: &str) -> Option<&'a mut Source> {
    let path = find_path(sources, alias)?;
    source_at_mut(sources, &path)
}

pub(crate) fn find_path(sources: &[Source], alias: &str) -> Option<Vec<usize>> {
    let key = fold_name(alias);
    for (i, r) in sources.iter().enumerate() {
        let mut path = vec![i];
        if path_in(r, &key, &mut path) {
            return Some(path);
        }
    }
    None
}

fn path_in(src: &Source, key: &str, path: &mut Vec<usize>) -> bool {
    if canon::alias_of(src).first().is_some_and(|a| a == key) {
        return true;
    }
    for (i, j) in src.joins.iter().enumerate() {
        path.push(i);
        if path_in(&j.right, key, path) {
            return true;
        }
        path.pop();
    }
    false
}

pub(crate) fn source_at_mut<'a>(
    sources: &'a mut [Source],
    path: &[usize],
) -> Option<&'a mut Source> {
    let (first, rest) = path.split_first()?;
    let mut cur = sources.get_mut(*first)?;
    for j in rest {
        cur = &mut cur.joins.get_mut(*j)?.right;
    }
    Some(cur)
}

pub(crate) fn shown_alias(s: &Source) -> String {
    if let Some(a) = &s.alias {
        return a.name.text.to_string();
    }
    match &s.table {
        Table::Named { path, .. } => path
            .parts
            .last()
            .map(|p| p.text.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub(crate) fn alias_of(name: &str) -> ast::Alias {
    ast::Alias {
        name: crate::to_ast::ident(name),
        as_kw: Some(SP),
    }
}

pub(crate) fn column_name(op: &Operator, item: &ast::SelectItem) -> Option<String> {
    let q = SelectQuery {
        operators: vec![Operator {
            fields: vec![item.clone()],
            sources: op.sources.clone(),
            ..Operator::empty()
        }],
        ..SelectQuery::empty()
    };
    names::derive(&q).into_iter().next().flatten()
}

pub(crate) fn unique(base: &str, taken: &dyn Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    let mut n = 1u32;
    loop {
        let cand = format!("{base}{n}");
        if !taken(&cand) {
            return cand;
        }
        n += 1;
    }
}

pub(crate) fn default_alias(qs: &query_ir::QuerySchema, parts: &[&str]) -> Option<String> {
    use query_syntax::ScriptVariant;
    let ru = qs.script() == ScriptVariant::Russian;
    let last = parts.last().map(|p| p.to_string());
    let Some(id) = qs.find_table(parts) else {
        return last;
    };
    if let Some(a) = spell::virtual_alias(qs, id, ru) {
        return Some(a);
    }
    let t = qs.table(id);
    let name = if ru { &t.name_ru } else { &t.name_en };
    name.rsplit('.').next().map(str::to_string).or(last)
}
