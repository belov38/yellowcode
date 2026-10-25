use crate::CompileError;
use crate::schema::{QuerySchema, ValuePath};
use query_syntax::{
    MsgArg, MsgId, ParseMode, Parsed, PublicParseLimit, QueryError, SourceMap, ast,
    parse_batch_public, parse_batch_values,
};

pub fn compile_batch(
    text: &str,
    mode: ParseMode,
    schema: &QuerySchema,
) -> Result<ast::Batch, CompileError> {
    resolve_values(text, schema, parse_batch_values(text, mode))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileRefusal {
    Structure(PublicParseLimit),
}

pub fn compile_batch_public(
    text: &str,
    mode: ParseMode,
    schema: &QuerySchema,
) -> Result<Result<ast::Batch, CompileError>, CompileRefusal> {
    let parsed = parse_batch_public(text, mode).map_err(CompileRefusal::Structure)?;
    Ok(resolve_values(text, schema, parsed))
}

fn resolve_values(
    text: &str,
    schema: &QuerySchema,
    parsed: Parsed,
) -> Result<ast::Batch, CompileError> {
    if let Ok(batch) = &parsed.batch {
        reject_unresolved(batch, text)?;
    }
    let syntax_at = parsed
        .batch
        .as_ref()
        .err()
        .and_then(|e| e.at.map(|a| a.offset));
    let bad = parsed.values.iter().find(|v| {
        let parts: Vec<&str> = v.path.split('.').collect();
        matches!(schema.value_path(&parts), ValuePath::Unknown)
            && syntax_at.is_none_or(|s| v.at < s)
    });
    if let Some(v) = bad {
        let at = SourceMap::new(text).at(text, v.at);
        return Err(
            QueryError::new(MsgId::SELECT_ERROR_WRONGPARAMETER, Some(at))
                .with_quoted(MsgArg::text(&v.path)),
        );
    }
    parsed.batch
}

pub(crate) const UNRESOLVED_TAIL: &str =
    "internal: unresolved adjacent DCS parameter; execution and discovery unsupported";

pub(crate) fn reject_unresolved(batch: &ast::Batch, text: &str) -> Result<(), CompileError> {
    if let Some(ast::DcsUnresolvedTail::AdjacentParam(param)) =
        query_syntax::unresolved_dcs_tails(batch).first()
    {
        let at = SourceMap::new(text).at(text, param.span.start);
        return Err(QueryError::new(MsgId::SELECT_ERROR_SYNTAXERROR, Some(at))
            .with_quoted(MsgArg::text(UNRESOLVED_TAIL)));
    }
    Ok(())
}
