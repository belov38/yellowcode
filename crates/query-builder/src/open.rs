use crate::error::OpenError;
use crate::{Schema, expand, from_ast, names, shape, spell};
use bsl_prim::fold_name;
use query_ir::{CompiledStmt, QuerySchema, TempSchema, TempSchemas};
use query_syntax::{MsgId, QueryError, ast};
use query_syntax::{ParseMode, with_deep_stack};
use std::collections::HashSet;
use std::sync::Arc;

pub struct Ctx<'a> {
    pub schema: &'a QuerySchema,
    pub temp: &'a dyn TempSchemas,
}

pub struct Opened {
    pub schema: Schema,
    pub compiled: Vec<Arc<CompiledStmt>>,
}

pub fn open(text: &str, mode: ParseMode, ctx: &Ctx<'_>) -> Result<Opened, OpenError> {
    if mode == ParseMode::Query && text.contains('{') {
        match open_as(text, ParseMode::Dcs, ctx, false) {
            Err(OpenError::Syntax(_)) => {}
            other => return other,
        }
    }
    open_as(text, mode, ctx, mode == ParseMode::Dcs)
}

fn open_as(text: &str, mode: ParseMode, ctx: &Ctx<'_>, strict: bool) -> Result<Opened, OpenError> {
    let mut schema = from_ast::parse(text, mode)?;
    let depth = schema.max_depth;
    with_deep_stack(depth, || shape::check(&schema))?;
    let compiled = analyze(text, mode, ctx, strict).map_err(OpenError::Semantic)?;
    schema.script = ctx.schema.script();
    with_deep_stack(depth, || crate::refuse::typed(&schema, &compiled, text))
        .map_err(OpenError::Semantic)?;
    with_deep_stack(depth, || {
        names::from_analysis(&mut schema, &compiled, ctx.schema);
        spell::apply(&mut schema, &compiled, ctx.schema);
        expand::apply(&mut schema, &compiled, ctx.schema, mode == ParseMode::Dcs);
    });
    Ok(Opened { schema, compiled })
}

pub(crate) fn analyze_modes(
    text: &str,
    mode: ParseMode,
    ctx: &Ctx<'_>,
) -> Result<Vec<Arc<CompiledStmt>>, QueryError> {
    if mode == ParseMode::Query && text.contains('{') {
        match analyze(text, ParseMode::Dcs, ctx, false) {
            Err(e) if e.msg == MsgId::SELECT_ERROR_SYNTAXERROR => {}
            other => return other,
        }
    }
    analyze(text, mode, ctx, mode == ParseMode::Dcs)
}

fn analyze(
    text: &str,
    mode: ParseMode,
    ctx: &Ctx<'_>,
    strict: bool,
) -> Result<Vec<Arc<CompiledStmt>>, QueryError> {
    let mut b = query_ir::compile_batch(text, mode, ctx.schema)?;
    let depth = b.max_depth;
    with_deep_stack(depth, || {
        crate::periods::batch(&mut b, text);
        crate::totals::batch(&mut b);
    });
    with_deep_stack(depth, || crate::refuse::syntax(&b, text, strict))?;
    match analyze_batch(&b, text, ctx) {
        Err(e) if e.msg == MsgId::SELECT_ERROR_FIELDNOTFOUND => {
            let mut b = b;
            let depth = b.max_depth;
            if with_deep_stack(depth, || tabular_stars(&mut b)) {
                analyze_batch(&b, text, ctx).map_err(|_| e)
            } else {
                Err(e)
            }
        }
        other => other,
    }
}

struct Foreign<'a> {
    base: &'a dyn TempSchemas,
    made: HashSet<String>,
}

impl TempSchemas for Foreign<'_> {
    fn get(&self, name: &str) -> Option<Arc<TempSchema>> {
        self.base.get(name).or_else(|| {
            if self.made.contains(&fold_name(name)) {
                return None;
            }
            Some(Arc::new(TempSchema {
                name: name.to_string(),
                columns: Vec::new(),
                open: true,
            }))
        })
    }
}

fn analyze_batch(
    b: &ast::Batch,
    text: &str,
    ctx: &Ctx<'_>,
) -> Result<Vec<Arc<CompiledStmt>>, QueryError> {
    let made = b
        .stmts
        .iter()
        .filter_map(|st| match st {
            ast::Stmt::Query(q) => Some(q.query.parts.iter().filter_map(|p| p.into.as_ref())),
            _ => None,
        })
        .flatten()
        .map(|i| fold_name(&i.name.text))
        .collect();
    let foreign = Foreign {
        base: ctx.temp,
        made,
    };
    query_ir::analyze_batch_static(b, text, ctx.schema, &foreign)
}

fn tabular_stars(b: &mut ast::Batch) -> bool {
    let mut changed = false;
    for st in &mut b.stmts {
        if let ast::Stmt::Query(q) = st {
            changed |= stars_in_query(&mut q.query);
        }
    }
    changed
}

fn stars_in_query(q: &mut ast::Query) -> bool {
    let mut changed = false;
    for p in &mut q.parts {
        for it in &mut p.items {
            if let ast::ItemKind::PathStar(path) = &it.kind
                && path.parts.len() >= 2
            {
                let table = ast::Expr {
                    span: path.span(),
                    kind: ast::ExprKind::Path(path.clone()),
                };
                let star = ast::SelectItem {
                    kind: ast::ItemKind::Star,
                    alias: None,
                    span: it.span,
                };
                it.kind = ast::ItemKind::Nested {
                    table,
                    fields: vec![star],
                };
                changed = true;
            }
        }
        if let Some(f) = &mut p.from {
            for s in &mut f.sources {
                changed |= stars_in_source(s);
            }
        }
    }
    changed
}

fn stars_in_source(s: &mut ast::Source) -> bool {
    let mut changed = false;
    if let ast::TableRef::Subquery { query, .. } = &mut s.table {
        changed |= stars_in_query(query);
    }
    for j in &mut s.joins {
        changed |= stars_in_source(&mut j.right);
    }
    changed
}
