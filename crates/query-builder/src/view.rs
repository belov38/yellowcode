use crate::canon;
use crate::model::*;
use crate::ops::{OpError, QueryRef};
use crate::pretty::piece;
use bsl_prim::fold_name;
use query_syntax::ast::{self, ControlPoint, GroupKind, IntoKind};
use query_syntax::kw::Kw;
use query_syntax::{ScriptVariant, with_deep_stack};
use serde::Serialize;

pub const MAX_TEXT: usize = 500;
pub const MAX_ROWS: usize = 2000;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct View {
    pub stmt: usize,
    pub nested: Vec<(usize, String)>,
    pub op_index: usize,
    pub kind: &'static str,
    pub drop_table: Option<String>,
    pub union_all: bool,
    pub distinct: bool,
    pub top: Option<String>,
    pub allowed: bool,
    pub into: Option<IntoView>,
    pub for_update: Vec<String>,
    pub tabs: Tabs,
    pub truncated: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IntoView {
    pub kind: &'static str,
    pub table: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Tabs {
    pub fields: Vec<FieldRow>,
    pub sources: Vec<SourceRow>,
    pub links: Vec<LinkRow>,
    pub conditions: Conditions,
    pub group: GroupTab,
    pub unions: Vec<OperatorRow>,
    pub order: OrderTab,
    pub totals: TotalsTab,
    pub batch: Vec<BatchRow>,
    pub dcs: DcsTab,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldRow {
    pub index: usize,
    pub expr_text: String,
    pub alias: Option<String>,
    pub auto: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceRow {
    pub alias: String,
    pub table: String,
    pub kind: &'static str,
    pub params: Vec<String>,
    pub joins: Vec<JoinRow>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JoinRow {
    pub kind: String,
    pub optional: bool,
    pub on_text: String,
    pub on: Vec<String>,
    pub right: Box<SourceRow>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LinkRow {
    pub left: String,
    pub right: String,
    pub kind: String,
    pub optional: bool,
    pub on_text: String,
    pub on: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Conditions {
    pub filter: Vec<Row>,
    pub having: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Row {
    pub index: usize,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GroupTab {
    pub kind: Option<&'static str>,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OperatorRow {
    pub index: usize,
    pub union_all: bool,
    pub distinct: bool,
    pub top: Option<String>,
    pub fields: usize,
    pub sources: usize,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OrderTab {
    pub rows: Vec<OrderRow>,
    pub autoorder: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OrderRow {
    pub index: usize,
    pub expr_text: String,
    pub dir: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TotalsTab {
    pub items: Vec<TotalRow>,
    pub points: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TotalRow {
    pub index: usize,
    pub expr_text: String,
    pub alias: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BatchRow {
    pub index: usize,
    pub kind: &'static str,
    pub title: String,
    pub operators: usize,
    pub into: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DcsTab {
    pub operator: Vec<String>,
    pub query: Vec<String>,
}

pub fn cut(text: String) -> String {
    let mut chars = text.char_indices().skip(MAX_TEXT);
    let Some((end, _)) = chars.next() else {
        return text;
    };
    let more = text.get(end..).map_or(0, |t| t.chars().count());
    let mut out = text.get(..end).unwrap_or_default().to_string();
    out.push_str(&format!("…[обрезано: ещё {more}]"));
    out
}

fn rows<T>(
    tab: &'static str,
    truncated: &mut Vec<&'static str>,
    it: impl Iterator<Item = T>,
) -> Vec<T> {
    let mut v: Vec<T> = it.take(MAX_ROWS + 1).collect();
    if v.len() > MAX_ROWS {
        v.truncate(MAX_ROWS);
        truncated.push(tab);
    }
    v
}

pub fn select_query<'a>(s: &'a Schema, q: &QueryRef) -> Result<&'a SelectQuery, OpError> {
    let mut cur = match s.batch.get(q.stmt) {
        Some(Stmt::Select(sq)) => sq,
        Some(Stmt::Drop(_)) => return Err(OpError::NotSelect(q.stmt)),
        None => return Err(OpError::NoStmt(q.stmt)),
    };
    for (oi, alias) in &q.nested {
        let op = cur.operators.get(*oi).ok_or(OpError::NoOperator(*oi))?;
        let key = fold_name(alias);
        let src = find(&op.sources, &key).ok_or_else(|| OpError::NoSource(alias.clone()))?;
        cur = match &src.table {
            Table::Subquery(b) => b,
            _ => return Err(OpError::NoSource(alias.clone())),
        };
    }
    Ok(cur)
}

fn find<'a>(sources: &'a [Source], key: &str) -> Option<&'a Source> {
    for s in sources {
        if canon::alias_of(s).first().is_some_and(|a| a == key) {
            return Some(s);
        }
        for j in &s.joins {
            if let Some(f) = find(std::slice::from_ref(&j.right), key) {
                return Some(f);
            }
        }
    }
    None
}

pub fn view(s: &Schema, q: &QueryRef, op_index: usize) -> Result<View, OpError> {
    let script = s.script;
    if let Some(Stmt::Drop(t)) = s.batch.get(q.stmt)
        && q.nested.is_empty()
    {
        let mut v = empty(q, op_index, "drop");
        v.drop_table = Some(t.clone());
        v.tabs.batch = batch(s, script, &mut v.truncated);
        return Ok(v);
    }
    let sq = select_query(s, q)?;
    let op = sq
        .operators
        .get(op_index)
        .ok_or(OpError::NoOperator(op_index))?;
    Ok(with_deep_stack(s.max_depth, || {
        let mut v = empty(q, op_index, "select");
        v.union_all = op.union_all;
        v.distinct = op.distinct;
        v.top = op.top.clone();
        v.allowed = sq.allowed;
        v.into = sq.into.as_ref().map(|p| IntoView {
            kind: match p.kind {
                IntoKind::Put => "put",
                IntoKind::Add => "add",
            },
            table: p.table.clone(),
        });
        v.for_update = op
            .for_update
            .iter()
            .flatten()
            .map(|p| cut(piece::path(p, script)))
            .collect();
        let t = &mut v.truncated;
        v.tabs = Tabs {
            fields: rows(
                "fields",
                t,
                op.fields.iter().enumerate().map(|(i, it)| {
                    field(
                        i,
                        it,
                        sq.names.get(i).and_then(|n| n.as_deref()),
                        op,
                        script,
                    )
                }),
            ),
            sources: rows(
                "sources",
                t,
                op.sources.iter().map(|x| source_row(x, script)),
            ),
            links: {
                let mut out = Vec::new();
                for r in &op.sources {
                    links(r, script, &mut out, MAX_ROWS + 1);
                }
                if out.len() > MAX_ROWS {
                    out.truncate(MAX_ROWS);
                    t.push("links");
                }
                out
            },
            conditions: Conditions {
                filter: rows("conditions", t, condition_rows(&op.filter, script)),
                having: rows("conditions", t, condition_rows(&op.having, script)),
            },
            group: group(op.group.as_ref(), script, t),
            unions: rows(
                "unions",
                t,
                sq.operators
                    .iter()
                    .enumerate()
                    .map(|(i, o)| operator_row(i, o, script)),
            ),
            order: OrderTab {
                rows: rows(
                    "order",
                    t,
                    sq.order.iter().enumerate().map(|(i, o)| OrderRow {
                        index: i,
                        expr_text: cut(piece::expr(&canon::expr(&o.expr), script)),
                        dir: dir_name(o.dir),
                    }),
                ),
                autoorder: sq.autoorder,
            },
            totals: totals(sq.totals.as_ref(), script, t),
            batch: batch(s, script, t),
            dcs: DcsTab {
                operator: rows(
                    "dcs",
                    t,
                    canon::dcs(&op.dcs)
                        .iter()
                        .map(|b| cut(piece::dcs_block(b, script))),
                ),
                query: rows(
                    "dcs",
                    t,
                    sq.dcs.iter().map(|b| cut(piece::dcs_block(b, script))),
                ),
            },
        };
        if op.sources.iter().any(joins_over_cap) {
            v.truncated.push("links");
        }
        v.truncated.sort_unstable();
        v.truncated.dedup();
        v
    }))
}

fn empty(q: &QueryRef, op_index: usize, kind: &'static str) -> View {
    View {
        stmt: q.stmt,
        nested: q.nested.clone(),
        op_index,
        kind,
        drop_table: None,
        union_all: false,
        distinct: false,
        top: None,
        allowed: false,
        into: None,
        for_update: Vec::new(),
        tabs: Tabs::default(),
        truncated: Vec::new(),
    }
}

fn field(
    i: usize,
    it: &ast::SelectItem,
    column: Option<&str>,
    op: &Operator,
    script: ScriptVariant,
) -> FieldRow {
    let alias = it
        .alias
        .as_ref()
        .map(|a| a.name.text.to_string())
        .or_else(|| {
            let named = matches!(
                it.kind,
                ast::ItemKind::Expr(_) | ast::ItemKind::Nested { .. }
            );
            column.filter(|_| named).map(str::to_string)
        });
    let auto = alias.as_ref().is_some_and(|a| {
        let bare = ast::SelectItem {
            alias: None,
            ..it.clone()
        };
        crate::ops::column_name(op, &bare).is_some_and(|d| fold_name(&d) == fold_name(a))
    });
    let mut item = it.clone();
    if let ast::ItemKind::Expr(e) = &mut item.kind {
        *e = canon::expr(e);
    }
    FieldRow {
        index: i,
        expr_text: cut(piece::item(&item, script)),
        alias,
        auto,
    }
}

fn source_name(s: &Source) -> String {
    if let Some(a) = &s.alias {
        return a.name.text.to_string();
    }
    match &s.table {
        Table::Named { path, .. } => path
            .parts
            .last()
            .map(|p| p.text.trim_start_matches('#').to_string())
            .unwrap_or_default(),
        Table::Subquery(_) | Table::Param(_) => String::new(),
    }
}

fn table_parts(s: &Source, script: ScriptVariant) -> (String, &'static str, Vec<String>) {
    match &s.table {
        Table::Named { path, args } => {
            let mut t = piece::path(path, script);
            let params: Vec<String> = args
                .iter()
                .flatten()
                .map(|a| cut(piece::vt_arg(&a.clone().map(canon::vt_arg), script)))
                .collect();
            if args.is_some() {
                t.push('(');
                t.push_str(&params.join(", "));
                t.push(')');
            }
            (
                cut(t),
                if s.table.is_temp() { "temp" } else { "table" },
                params,
            )
        }
        Table::Subquery(_) => ("(…)".to_string(), "subquery", Vec::new()),
        Table::Param(p) => (piece::param(p, script), "param", Vec::new()),
    }
}

fn table_text(s: &Source, script: ScriptVariant) -> String {
    table_parts(s, script).0
}

fn source_row(s: &Source, script: ScriptVariant) -> SourceRow {
    let (table, kind, params) = table_parts(s, script);
    SourceRow {
        alias: source_name(s),
        table,
        kind,
        params,
        joins: s.joins.iter().map(|j| join_row(j, script)).collect(),
    }
}

fn joins_over_cap(s: &Source) -> bool {
    s.joins
        .iter()
        .any(|j| j.on.len() > MAX_ROWS || joins_over_cap(&j.right))
}

fn on_texts(j: &Join, script: ScriptVariant) -> (String, Vec<String>) {
    let on: Vec<String> =
        j.on.iter()
            .take(MAX_ROWS)
            .map(|c| cut(piece::expr(&canon::expr(&c.expr), script)))
            .collect();
    let and = format!(" {} ", Kw::And.name(script));
    (cut(on.join(&and)), on)
}

fn join_row(j: &Join, script: ScriptVariant) -> JoinRow {
    let (on_text, on) = on_texts(j, script);
    JoinRow {
        kind: piece::join_kind(j.kind, script),
        optional: j.optional,
        on_text,
        on,
        right: Box::new(source_row(&j.right, script)),
    }
}

fn links(s: &Source, script: ScriptVariant, out: &mut Vec<LinkRow>, cap: usize) {
    for j in &s.joins {
        if out.len() >= cap {
            return;
        }
        let (on_text, on) = on_texts(j, script);
        out.push(LinkRow {
            left: source_name(s),
            right: source_name(&j.right),
            kind: piece::join_kind(j.kind, script),
            optional: j.optional,
            on_text,
            on,
        });
        links(&j.right, script, out, cap);
    }
}

fn condition_rows(list: &[ast::Expr], script: ScriptVariant) -> impl Iterator<Item = Row> + '_ {
    list.iter().enumerate().map(move |(i, e)| Row {
        index: i,
        text: cut(piece::expr(&canon::expr(e), script)),
    })
}

fn group(g: Option<&GroupKind>, script: ScriptVariant, t: &mut Vec<&'static str>) -> GroupTab {
    match g {
        None => GroupTab::default(),
        Some(GroupKind::Exprs(es)) => GroupTab {
            kind: Some("exprs"),
            rows: rows(
                "group",
                t,
                es.iter()
                    .enumerate()
                    .filter(|(_, e)| !matches!(e.kind, ast::ExprKind::Param(_)))
                    .map(|(i, e)| Row {
                        index: i,
                        text: cut(piece::expr(&canon::expr(e), script)),
                    }),
            ),
        },
        Some(GroupKind::Sets(sets)) => GroupTab {
            kind: Some("sets"),
            rows: rows(
                "group",
                t,
                sets.iter().enumerate().map(|(i, set)| Row {
                    index: i,
                    text: cut(piece::group_set(
                        &set.iter().map(canon::expr).collect::<Vec<_>>(),
                        script,
                    )),
                }),
            ),
        },
    }
}

fn dir_name(d: ast::OrderDir) -> &'static str {
    match d {
        ast::OrderDir::Asc => "asc",
        ast::OrderDir::Desc => "desc",
        ast::OrderDir::Hierarchy => "hierarchy",
        ast::OrderDir::HierarchyDesc => "hierarchy_desc",
    }
}

fn totals(tot: Option<&Totals>, script: ScriptVariant, t: &mut Vec<&'static str>) -> TotalsTab {
    let Some(tot) = tot else {
        return TotalsTab::default();
    };
    TotalsTab {
        items: rows(
            "totals",
            t,
            tot.items.iter().enumerate().map(|(i, it)| TotalRow {
                index: i,
                expr_text: cut(piece::expr(&canon::expr(&it.expr), script)),
                alias: it.alias.as_ref().map(|a| a.name.text.to_string()),
            }),
        ),
        points: rows(
            "totals",
            t,
            tot.points.iter().enumerate().map(|(i, cp)| Row {
                index: i,
                text: cut(point_text(cp, script)),
            }),
        ),
    }
}

fn point_text(cp: &ControlPoint, script: ScriptVariant) -> String {
    match cp {
        ControlPoint::Field {
            expr,
            hierarchy,
            periods,
            alias,
            span,
        } => piece::control_point(
            &ControlPoint::Field {
                expr: canon::expr(expr),
                hierarchy: hierarchy.clone(),
                periods: periods.clone(),
                alias: alias.clone(),
                span: *span,
            },
            script,
        ),
        other => piece::control_point(other, script),
    }
}

fn operator_row(i: usize, o: &Operator, script: ScriptVariant) -> OperatorRow {
    OperatorRow {
        index: i,
        union_all: o.union_all,
        distinct: o.distinct,
        top: o.top.clone(),
        fields: o.fields.len(),
        sources: o.sources.len(),
        title: o
            .sources
            .first()
            .map(|s| table_text(s, script))
            .unwrap_or_default(),
    }
}

fn batch(s: &Schema, script: ScriptVariant, t: &mut Vec<&'static str>) -> Vec<BatchRow> {
    rows(
        "batch",
        t,
        s.batch.iter().enumerate().map(|(i, st)| match st {
            Stmt::Drop(table) => BatchRow {
                index: i,
                kind: "drop",
                title: cut(table.clone()),
                operators: 0,
                into: None,
            },
            Stmt::Select(q) => BatchRow {
                index: i,
                kind: "select",
                title: q
                    .into
                    .as_ref()
                    .map(|p| p.table.clone())
                    .or_else(|| {
                        q.operators
                            .first()
                            .and_then(|o| o.sources.first())
                            .map(|x| table_text(x, script))
                    })
                    .map(cut)
                    .unwrap_or_default(),
                operators: q.operators.len(),
                into: q.into.as_ref().map(|p| p.table.clone()),
            },
        }),
    )
}
