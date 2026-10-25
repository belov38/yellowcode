use crate::{
    ErrAt, MsgId, ParseMode, Parsed, PublicParseLimit, SourceMap, SyntaxError, ast, estimate_depth,
    lexer, parser, preflight_public, resource, with_deep_stack,
};
use memory_budget::{
    BudgetError, RequestBudget, ResourceCause, ResourceFailure, ResourcePhase, StructureLimit,
};

pub struct WorkerParsed<'a, 'r, 'w> {
    parsed: Parsed,
    max_depth: u32,
    error_map: Option<SourceMap>,
    source: &'a str,
    _scope: resource::ParseScope,
    _request: &'a RequestBudget<'r, 'w>,
}

impl<'a, 'r, 'w> WorkerParsed<'a, 'r, 'w> {
    pub unsafe fn consume_on_worker<'p, R>(
        &'p self,
        consume: impl FnOnce(
            &'p Parsed,
            Option<&'p SourceMap>,
            &'a str,
            &'a RequestBudget<'r, 'w>,
            u32,
        ) -> R,
    ) -> R {
        let map = self
            .parsed
            .batch
            .as_ref()
            .ok()
            .map(|batch| &batch.map)
            .or(self.error_map.as_ref());
        consume(
            &self.parsed,
            map,
            self.source,
            self._request,
            self.max_depth,
        )
    }

    pub fn statement_count(&self) -> Option<usize> {
        self.parsed
            .batch
            .as_ref()
            .ok()
            .map(|batch| batch.stmts.len())
    }

    pub fn max_depth(&self) -> u32 {
        self.max_depth
    }

    pub fn syntax_message(&self) -> Option<MsgId> {
        self.parsed.batch.as_ref().err().map(|error| error.msg)
    }

    pub fn syntax_at(&self) -> Option<ErrAt> {
        self.parsed.batch.as_ref().err().and_then(|error| error.at)
    }

    pub fn value_count(&self) -> usize {
        self.parsed.values.len()
    }

    pub fn value_use(&self, index: usize) -> Option<(&str, u32)> {
        self.parsed
            .values
            .get(index)
            .map(|value| (value.path.as_str(), value.at))
    }
}

fn memory_failure(error: BudgetError) -> ResourceFailure {
    ResourceFailure {
        phase: ResourcePhase::Parse,
        cause: ResourceCause::Memory(error),
    }
}

fn structure_failure(limit: PublicParseLimit) -> ResourceFailure {
    ResourceFailure {
        phase: ResourcePhase::Parse,
        cause: ResourceCause::Structure(match limit {
            PublicParseLimit::Depth => StructureLimit::Depth,
            PublicParseLimit::Statements => StructureLimit::Statements,
            PublicParseLimit::Columns => StructureLimit::Columns,
        }),
    }
}

pub fn parse_batch_on_worker<'a, 'r, 'w>(
    text: &'a str,
    mode: ParseMode,
    request: &'a RequestBudget<'r, 'w>,
) -> Result<WorkerParsed<'a, 'r, 'w>, ResourceFailure> {
    request.checkpoint(ResourcePhase::Parse)?;
    if text.len() > (u32::MAX / 2) as usize {
        return Err(request.record(memory_failure(BudgetError::Overflow)));
    }
    preflight_public(text).map_err(|limit| request.record(structure_failure(limit)))?;
    let scope = resource::ParseScope::new(request.memory())
        .map_err(|error| request.record(memory_failure(error)))?;
    let result = parse_paid(text, mode, &scope, true);
    if let Some(error) = scope.error() {
        return Err(request.record(memory_failure(error)));
    }
    match result {
        Ok((_, Some(limit), _, _)) => Err(request.record(structure_failure(limit))),
        Ok((parsed, None, max_depth, error_map)) => Ok(WorkerParsed {
            parsed,
            max_depth,
            error_map,
            source: text,
            _scope: scope,
            _request: request,
        }),
        Err(error) => Ok(WorkerParsed {
            parsed: Parsed {
                batch: Err(error),
                values: Vec::new(),
            },
            max_depth: 0,
            error_map: None,
            source: text,
            _scope: scope,
            _request: request,
        }),
    }
}

pub(super) fn parse_paid(
    text: &str,
    mode: ParseMode,
    scope: &resource::ParseScope,
    current_worker: bool,
) -> Result<(Parsed, Option<PublicParseLimit>, u32, Option<SourceMap>), SyntaxError> {
    let mut lines = resource::list(Some(scope), [0u32])?;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' || (b == b'\r' && text.as_bytes().get(i + 1) != Some(&b'\n')) {
            resource::push(Some(scope), &mut lines, (i + 1) as u32)?;
        }
    }
    let mut lexer = lexer::Tokens::new(text);
    let mut toks = Vec::new();
    for token in lexer.by_ref() {
        resource::push(Some(scope), &mut toks, token)?;
    }
    if lexer.comment_eof() && lines.last().is_none_or(|&start| start < text.len() as u32) {
        resource::push(Some(scope), &mut lines, text.len() as u32)?;
    }
    let map = SourceMap::from_lines(lines, text.len() as u32);
    let depth = estimate_depth(&toks);
    let parse = || {
        let mut parser = parser::Parser::new_with_memory(text, toks, map, mode, Some(scope))?;
        parser.public_limits = true;
        let result = parser.batch();
        let parser::Parser {
            map,
            max_depth,
            values,
            limit_error,
            ..
        } = parser;
        let (batch, error_map) = match result {
            Ok(stmts) => (
                Ok(ast::Batch {
                    stmts,
                    map,
                    mode,
                    max_depth: if current_worker { 0 } else { max_depth },
                }),
                None,
            ),
            Err(error) => (Err(error), Some(map)),
        };
        Ok((Parsed { batch, values }, limit_error, max_depth, error_map))
    };
    if current_worker {
        parse()
    } else {
        with_deep_stack(depth, parse)
    }
}
