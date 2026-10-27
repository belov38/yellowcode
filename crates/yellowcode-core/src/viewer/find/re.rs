use regex_automata::{Input, MatchKind, meta, nfa::thompson::WhichCaptures, util::syntax};
use std::fmt;

pub const MAX_PATTERN_BYTES: usize = 1024;
pub const NFA_SIZE_LIMIT: usize = 4 << 20;
pub const LAZY_DFA_CACHE: usize = 1 << 20;
pub const ALLOWANCE: u64 = 32 << 20;

#[derive(Debug)]
pub enum RegexError {
    TooLong,
    TooBig(usize),
    Build(Box<meta::BuildError>),
}

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => write!(f, "regex is longer than {MAX_PATTERN_BYTES} bytes"),
            Self::TooBig(limit) => write!(f, "Compiled regex exceeds size limit of {limit} bytes."),
            Self::Build(error) => match error.syntax_error() {
                Some(syntax) => syntax.fmt(f),
                None => error.fmt(f),
            },
        }
    }
}

impl std::error::Error for RegexError {}

pub fn check_len(pattern: &str) -> Result<(), RegexError> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(RegexError::TooLong);
    }
    Ok(())
}

#[derive(Debug)]
pub struct FindRegex {
    regex: meta::Regex,
    cache: meta::Cache,
}

impl FindRegex {
    pub fn compile(pattern: &str) -> Result<Self, RegexError> {
        check_len(pattern)?;
        let syntax = syntax::Config::new()
            .case_insensitive(true)
            .unicode(true)
            .utf8(true)
            .nest_limit(250);
        let config = meta::Config::new()
            .match_kind(MatchKind::LeftmostFirst)
            .utf8_empty(true)
            .which_captures(WhichCaptures::Implicit)
            .nfa_size_limit(Some(NFA_SIZE_LIMIT))
            .hybrid_cache_capacity(LAZY_DFA_CACHE)
            .backtrack(false)
            .onepass(false)
            .dfa(false);
        let regex = meta::Builder::new()
            .configure(config)
            .syntax(syntax)
            .build(pattern)
            .map_err(|error| match error.size_limit() {
                Some(limit) => RegexError::TooBig(limit),
                None => RegexError::Build(Box::new(error)),
            })?;
        let cache = regex.create_cache();
        Ok(Self { regex, cache })
    }

    pub fn is_match(&mut self, line: &str) -> bool {
        self.regex
            .search_half_with(&mut self.cache, &Input::new(line).earliest(true))
            .is_some()
    }
}
