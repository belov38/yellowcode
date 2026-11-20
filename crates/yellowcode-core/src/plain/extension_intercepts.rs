use bsl_compiler::{CatalogBuiltins, ModuleKind};
use bsl_syntax::{
    ExecContext, ParseMode, ParseOptions, SourceText, Token, TokenKind, fold_ident, parse, tokenize,
};
use memory_budget::{
    BudgetError, Category, LimitReason, Limits, MemoryBudget, MemoryLimit, Reservation,
};
use std::{
    cell::OnceCell,
    collections::{BTreeMap, btree_map::Entry},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterceptKind {
    Before,
    After,
    Around,
    ChangeAndValidate,
}

impl InterceptKind {
    fn from_name(name: &str) -> Option<Self> {
        match fold_ident(name).as_str() {
            "перед" | "before" => Some(Self::Before),
            "после" | "after" => Some(Self::After),
            "вместо" | "around" => Some(Self::Around),
            "изменениеиконтроль" | "changeandvalidate" => {
                Some(Self::ChangeAndValidate)
            }
            _ => None,
        }
    }

    pub fn annotation(self) -> &'static str {
        match self {
            Self::Before => "&Перед",
            Self::After => "&После",
            Self::Around => "&Вместо",
            Self::ChangeAndValidate => "&ИзменениеИКонтроль",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intercept {
    pub kind: InterceptKind,
    pub method: Arc<str>,
    pub target_method: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("BSL catalog memory: {0}")]
    Catalog(#[source] MemoryLimit),
    #[error("BSL working memory: {0}")]
    Working(#[source] BudgetError),
    #[error("BSL intercept memory: {0}")]
    Retained(#[source] MemoryLimit),
    #[error("extension output: {0}")]
    Output(#[source] MemoryLimit),
    #[error("BSL syntax at {line}:{column}: {code}")]
    Syntax {
        line: u32,
        column: u32,
        code: &'static str,
    },
    #[error("{} at {line}:{column} requires a method name as its string argument", kind.annotation())]
    Target {
        line: u32,
        column: u32,
        kind: InterceptKind,
    },
}

const PARSE_BYTES_PER_INPUT_BYTE: u64 = 300;
const PARSE_BASE: u64 = 8 << 20;
const MAX_MODULE_BYTES: u64 = 4 << 20;
const CATALOG_BUDGET: u64 = 30 << 20;
const RETAINED_BUDGET: u64 = 128 << 20;

pub(super) fn command_limits() -> Limits {
    let total = (81 << 20)
        + PARSE_BASE
        + MAX_MODULE_BYTES * PARSE_BYTES_PER_INPUT_BYTE
        + CATALOG_BUDGET
        + RETAINED_BUDGET;
    Limits::new(total).with_category(Category::Result, RETAINED_BUDGET)
}

pub(super) struct Analysis<'a> {
    budget: &'a MemoryBudget,
    builtins: OnceCell<Result<(CatalogBuiltins<'static>, Reservation), MemoryLimit>>,
}

impl<'a> Analysis<'a> {
    pub(super) fn new(budget: &'a MemoryBudget) -> Self {
        Self {
            budget,
            builtins: OnceCell::new(),
        }
    }

    fn builtins(&self) -> Result<&CatalogBuiltins<'static>, Error> {
        self.builtins
            .get_or_init(|| {
                let charge = self
                    .budget
                    .try_reserve(Category::Metadata, CATALOG_BUDGET)?;
                Ok((CatalogBuiltins(platform_catalog::catalog()), charge))
            })
            .as_ref()
            .map(|(table, _)| table)
            .map_err(|limit| Error::Catalog(*limit))
    }
}

const CONTEXTS: [ExecContext; 10] = [
    ExecContext::Server,
    ExecContext::ThinClient,
    ExecContext::WebClient,
    ExecContext::MobileClient,
    ExecContext::ThickClientManaged,
    ExecContext::ThickClientOrdinary,
    ExecContext::ExternalConnection,
    ExecContext::MobileAppClient,
    ExecContext::MobileAppServer,
    ExecContext::MobileStandaloneServer,
];

pub(super) struct Intercepts {
    pub rows: Vec<Intercept>,
    _retained: Reservation,
}

pub fn intercepts(text: &str) -> Result<Vec<Intercept>, Error> {
    let budget =
        crate::service::memory::intercepts_budget(command_limits()).map_err(Error::Working)?;
    let analysis = Analysis::new(&budget);
    Ok(intercepts_with_budget(text, ModuleKind::Object, &analysis, u64::MAX)?.rows)
}

pub(super) fn intercepts_with_budget(
    text: &str,
    kind: ModuleKind,
    analysis: &Analysis<'_>,
    wire_room: u64,
) -> Result<Intercepts, Error> {
    let bytes =
        PARSE_BASE.saturating_add((text.len() as u64).saturating_mul(PARSE_BYTES_PER_INPUT_BYTE));
    let _working = analysis
        .budget
        .try_reserve(Category::Execution, bytes)
        .map_err(|error| Error::Working(error.into()))?;
    let retained = analysis
        .budget
        .try_reserve(Category::Result, 4096)
        .map_err(Error::Retained)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let source = SourceText::from_str("", text);
    let contexts = if text.contains('#') && has_conditionals(&source) {
        &CONTEXTS[..]
    } else {
        &CONTEXTS[..1]
    };
    let builtins = analysis.builtins()?;
    let mut names = BTreeMap::<u32, Arc<str>>::new();
    let mut wire_used = 0u64;
    let mut found = BTreeMap::new();
    for &context in contexts {
        let parsed = parse(
            &source,
            &ParseOptions {
                mode: ParseMode::Module,
                context,
                methods_only: kind.methods_only(),
                extension: true,
                builtins,
            },
        );
        if let Some(error) = parsed.error {
            return Err(Error::Syntax {
                line: error.pos.line,
                column: error.pos.col,
                code: error.msg.0,
            });
        }
        for method in parsed.module.methods {
            for annotation in method.annotations {
                let Some(kind) = InterceptKind::from_name(&annotation.name.name) else {
                    continue;
                };
                let target = annotation
                    .arg
                    .and_then(|arg| target_name(arg.into_vec()))
                    .ok_or_else(|| {
                        let pos = source.pos(annotation.span.start);
                        Error::Target {
                            line: pos.line,
                            column: pos.col,
                            kind,
                        }
                    })?;
                let method_start = method.name.span.start;
                let key = (annotation.span.start, method_start, fold_ident(&target));
                if let Entry::Vacant(slot) = found.entry(key) {
                    let row_bytes = method.name.name.len() + target.len();
                    wire_used = wire_used.saturating_add(row_bytes as u64);
                    if wire_used > wire_room {
                        return Err(Error::Output(MemoryLimit {
                            category: Category::Response,
                            requested: wire_used,
                            reason: LimitReason::Category,
                        }));
                    }
                    retained
                        .try_grow(320 + slot.key().2.capacity() as u64 + target.capacity() as u64)
                        .map_err(Error::Retained)?;
                    let name = match names.entry(method_start) {
                        Entry::Occupied(name) => name.get().clone(),
                        Entry::Vacant(name) => {
                            retained
                                .try_grow(256 + method.name.name.len() as u64)
                                .map_err(Error::Retained)?;
                            name.insert(Arc::from(method.name.name.as_str())).clone()
                        }
                    };
                    slot.insert((kind, name, target));
                }
            }
        }
    }
    let rows = found
        .into_iter()
        .map(|(_, (kind, method, target_method))| Intercept {
            kind,
            method,
            target_method,
        })
        .collect();
    Ok(Intercepts {
        rows,
        _retained: retained,
    })
}

fn has_conditionals(source: &SourceText) -> bool {
    tokenize(source).tokens.into_iter().any(|token| {
        if token.kind != TokenKind::Directive {
            return false;
        }
        let directive = source.text(token.span);
        let word = directive
            .trim_start_matches('#')
            .trim_start_matches(|ch: char| {
                ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}' | '\u{180e}')
            })
            .split(|ch: char| !ch.is_alphabetic())
            .next();
        word.is_some_and(|word| matches!(fold_ident(word).as_str(), "если" | "if"))
    })
}

fn target_name(units: Vec<u16>) -> Option<String> {
    let source = SourceText::from_utf16("", units);
    match tokenize(&source).tokens.as_slice() {
        [
            Token {
                kind: TokenKind::Ident,
                span,
            },
            Token {
                kind: TokenKind::Eof,
                ..
            },
        ] if span.start == 0 && span.end as usize == source.units().len() => {
            Some(source.text(*span))
        }
        _ => None,
    }
}
