use memory_budget::{BudgetError, BudgetedBox, BudgetedText, BudgetedVec, Category, MemoryBudget};
use query_syntax::{ErrAt, Func, Kw, MsgArg, MsgId, QueryError};

pub(crate) enum Argument<'a> {
    Text(&'a str),
    Kw(Kw),
    Func(Func),
    Words(&'a str, &'a str),
}
impl Argument<'_> {
    pub(crate) fn legacy(self) -> MsgArg {
        match self {
            Self::Text(text) => MsgArg::text(text),
            Self::Kw(keyword) => MsgArg::Kw(keyword),
            Self::Func(function) => MsgArg::Func(function),
            Self::Words(a, b) => MsgArg::text(&format!("{a} {b}")),
        }
    }
    fn owned(self, budget: &MemoryBudget) -> Result<OwnedArgument, BudgetError> {
        match self {
            Self::Text(text) => text_owned(budget, text).map(OwnedArgument::Text),
            Self::Kw(keyword) => Ok(OwnedArgument::Kw(keyword)),
            Self::Func(function) => Ok(OwnedArgument::Func(function)),
            Self::Words(a, b) => {
                let mut text = BudgetedText::new(budget, Category::Execution, u64::MAX)?;
                text.try_push_str(a)?;
                text.try_push(' ')?;
                text.try_push_str(b)?;
                Ok(OwnedArgument::Text(text))
            }
        }
    }
}
pub(super) enum OwnedArgument {
    Text(BudgetedText),
    Kw(Kw),
    Func(Func),
}
impl OwnedArgument {
    pub(super) fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text.as_str()),
            _ => None,
        }
    }
    pub(super) fn keyword(&self) -> Option<Kw> {
        match self {
            Self::Kw(keyword) => Some(*keyword),
            _ => None,
        }
    }
    pub(super) fn function(&self) -> Option<Func> {
        match self {
            Self::Func(function) => Some(*function),
            _ => None,
        }
    }
}
fn text_owned(budget: &MemoryBudget, text: &str) -> Result<BudgetedText, BudgetError> {
    let mut owned =
        BudgetedText::try_with_capacity(budget, Category::Execution, u64::MAX, text.len())?;
    owned.try_push_str(text)?;
    Ok(owned)
}

pub(crate) struct Diagnostic {
    budget: MemoryBudget,
    pub(super) msg: MsgId,
    pub(super) at: Option<ErrAt>,
    pub(super) subst: BudgetedVec<OwnedArgument>,
    pub(super) quoted: Option<OwnedArgument>,
    pub(super) detail: Option<BudgetedText>,
    pub(super) cause: Option<BudgetedBox<Diagnostic>>,
    depth: usize,
}
impl Diagnostic {
    fn try_new(
        budget: &MemoryBudget,
        msg: MsgId,
        at: Option<ErrAt>,
    ) -> Result<BudgetedBox<Self>, BudgetError> {
        let value = Self {
            budget: budget.clone(),
            msg,
            at,
            subst: BudgetedVec::new(budget, Category::Execution)?,
            quoted: None,
            detail: None,
            cause: None,
            depth: 1,
        };
        BudgetedBox::try_new(budget, Category::Execution, value)
    }
}

pub(crate) enum Failure {
    Legacy(QueryError),
    Managed(BudgetedBox<Diagnostic>),
    Resource(BudgetError),
}
impl Failure {
    pub(super) fn same_scope(&self, budget: &MemoryBudget) -> bool {
        match self {
            Self::Managed(diagnostic) => diagnostic.budget.same_scope(budget),
            Self::Resource(_) => true,
            Self::Legacy(_) => false,
        }
    }
    pub(super) fn new(budget: Option<&MemoryBudget>, msg: MsgId, at: Option<ErrAt>) -> Self {
        match budget {
            None => Self::Legacy(QueryError::new(msg, at)),
            Some(budget) => match Diagnostic::try_new(budget, msg, at) {
                Ok(value) => Self::Managed(value),
                Err(error) => Self::Resource(error),
            },
        }
    }
    fn update(self, change: impl FnOnce(&mut Diagnostic) -> Result<(), BudgetError>) -> Self {
        match self {
            Self::Managed(mut diagnostic) => match change(&mut diagnostic) {
                Ok(()) => Self::Managed(diagnostic),
                Err(error) => {
                    drop(diagnostic);
                    Self::Resource(error)
                }
            },
            other => other,
        }
    }
    pub(super) fn with_subst(self, argument: Argument<'_>) -> Self {
        match self {
            Self::Legacy(error) => Self::Legacy(error.with_subst(argument.legacy())),
            other => other.update(|diagnostic| {
                let argument = argument.owned(&diagnostic.budget)?;
                diagnostic.subst.try_push(argument)
            }),
        }
    }
    pub(super) fn with_quoted(self, argument: Argument<'_>) -> Self {
        match self {
            Self::Legacy(error) => Self::Legacy(error.with_quoted(argument.legacy())),
            other => other.update(|diagnostic| {
                diagnostic.quoted = Some(argument.owned(&diagnostic.budget)?);
                Ok(())
            }),
        }
    }
    pub(super) fn with_quoted_parts<'a>(
        self,
        parts: impl IntoIterator<Item = &'a str>,
        separator: &str,
    ) -> Self {
        match self {
            Self::Legacy(error) => {
                let mut text = String::new();
                for (index, part) in parts.into_iter().enumerate() {
                    if index > 0 {
                        text.push_str(separator);
                    }
                    text.push_str(part);
                }
                Self::Legacy(error.with_quoted(MsgArg::text(&text)))
            }
            other => other.update(|diagnostic| {
                let mut text = text_owned(&diagnostic.budget, "")?;
                for (index, part) in parts.into_iter().enumerate() {
                    if index > 0 {
                        text.try_push_str(separator)?;
                    }
                    text.try_push_str(part)?;
                }
                diagnostic.quoted = Some(OwnedArgument::Text(text));
                Ok(())
            }),
        }
    }
    pub(super) fn with_quoted_chars(self, chars: impl IntoIterator<Item = char>) -> Self {
        match self {
            Self::Legacy(error) => {
                let text: String = chars.into_iter().collect();
                Self::Legacy(error.with_quoted(MsgArg::text(&text)))
            }
            other => other.update(|diagnostic| {
                let mut text = text_owned(&diagnostic.budget, "")?;
                for character in chars {
                    text.try_push(character)?;
                }
                diagnostic.quoted = Some(OwnedArgument::Text(text));
                Ok(())
            }),
        }
    }
    pub(super) fn with_detail(self, text: &str) -> Self {
        match self {
            Self::Legacy(error) => Self::Legacy(error.with_detail(text)),
            other => other.update(|diagnostic| {
                diagnostic.detail = Some(text_owned(&diagnostic.budget, text)?);
                Ok(())
            }),
        }
    }
    pub(super) fn with_cause(self, cause: Self) -> Self {
        match (self, cause) {
            (Self::Resource(original), _) => Self::Resource(original),
            (_, Self::Resource(original)) => Self::Resource(original),
            (Self::Legacy(error), Self::Legacy(cause)) => Self::Legacy(error.with_cause(cause)),
            (Self::Managed(mut diagnostic), Self::Managed(cause)) => {
                if !diagnostic.budget.same_scope(&cause.budget) || cause.depth >= 128 {
                    return Self::Resource(BudgetError::Overflow);
                }
                diagnostic.depth = cause.depth + 1;
                diagnostic.cause = Some(cause);
                Self::Managed(diagnostic)
            }
            _ => Self::Resource(BudgetError::Overflow),
        }
    }
    pub(super) fn resource(&self) -> Option<BudgetError> {
        match self {
            Self::Resource(error) => Some(*error),
            _ => None,
        }
    }
    pub(super) fn managed(&self) -> Option<&Diagnostic> {
        match self {
            Self::Managed(diagnostic) => Some(diagnostic),
            _ => None,
        }
    }
    pub(super) fn into_legacy(self) -> Result<QueryError, Self> {
        match self {
            Self::Legacy(error) => Ok(error),
            other => Err(other),
        }
    }
}

pub(crate) enum ProjectedArgument<'a> {
    Text(&'a str),
    Kw(Kw),
    Func(Func),
    Msg(query_syntax::MsgId),
}

#[derive(Clone, Copy)]
pub(crate) struct DiagnosticProjection<'a>(Projection<'a>);
#[derive(Clone, Copy)]
enum Projection<'a> {
    Syntax(&'a QueryError),
    Managed(&'a Diagnostic),
}
impl Failure {
    pub(crate) fn projection(&self) -> Option<DiagnosticProjection<'_>> {
        match self {
            Self::Managed(value) => Some(DiagnosticProjection(Projection::Managed(value))),
            _ => None,
        }
    }
}
impl<'a> DiagnosticProjection<'a> {
    pub(crate) fn syntax(error: &'a QueryError) -> Self {
        Self(Projection::Syntax(error))
    }
    pub(crate) fn message(self) -> MsgId {
        match self.0 {
            Projection::Syntax(v) => v.msg,
            Projection::Managed(v) => v.msg,
        }
    }
    pub(crate) fn at(self) -> Option<ErrAt> {
        match self.0 {
            Projection::Syntax(v) => v.at,
            Projection::Managed(v) => v.at,
        }
    }
    pub(crate) fn detail(self) -> Option<&'a str> {
        match self.0 {
            Projection::Syntax(v) => v.detail.as_deref(),
            Projection::Managed(v) => v.detail.as_ref().map(BudgetedText::as_str),
        }
    }
    pub(crate) fn count(self) -> usize {
        match self.0 {
            Projection::Syntax(v) => v.subst.len(),
            Projection::Managed(v) => v.subst.len(),
        }
    }
    pub(crate) fn argument(self, index: usize) -> Option<ProjectedArgument<'a>> {
        match self.0 {
            Projection::Syntax(v) => v.subst.get(index).map(project_native),
            Projection::Managed(v) => v.subst.as_slice().get(index).map(project_owned),
        }
    }
    pub(crate) fn quoted(self) -> Option<ProjectedArgument<'a>> {
        match self.0 {
            Projection::Syntax(v) => v.quoted.as_ref().map(project_native),
            Projection::Managed(v) => v.quoted.as_ref().map(project_owned),
        }
    }
    pub(crate) fn cause(self) -> Option<Self> {
        match self.0 {
            Projection::Syntax(v) => v.cause.as_deref().map(Self::syntax),
            Projection::Managed(v) => v.cause.as_deref().map(|v| Self(Projection::Managed(v))),
        }
    }
}
fn project_native(value: &MsgArg) -> ProjectedArgument<'_> {
    match value {
        MsgArg::Text(v) => ProjectedArgument::Text(v),
        MsgArg::Kw(v) => ProjectedArgument::Kw(*v),
        MsgArg::Func(v) => ProjectedArgument::Func(*v),
        MsgArg::Msg(v) => ProjectedArgument::Msg(*v),
    }
}
fn project_owned(value: &OwnedArgument) -> ProjectedArgument<'_> {
    match value {
        OwnedArgument::Text(v) => ProjectedArgument::Text(v.as_str()),
        OwnedArgument::Kw(v) => ProjectedArgument::Kw(*v),
        OwnedArgument::Func(v) => ProjectedArgument::Func(*v),
    }
}
