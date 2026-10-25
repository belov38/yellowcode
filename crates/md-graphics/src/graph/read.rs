use super::scalar;
use super::values::GraphError;
use crate::templates::allocation::Allocation;
use brace::Node;
use memory_budget::BudgetError;

#[derive(Debug)]
pub(crate) enum ReadError {
    Memory(BudgetError),
    Semantic {
        code: &'static str,
        native_detail: Option<String>,
    },
}
impl From<BudgetError> for ReadError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl ReadError {
    pub(crate) fn legacy(self) -> GraphError {
        GraphError(match self {
            Self::Memory(error) => error.to_string(),
            Self::Semantic {
                code,
                native_detail,
            } => native_detail.unwrap_or_else(|| code.to_string()),
        })
    }
}
pub(crate) type R<T> = Result<T, ReadError>;
#[derive(Clone, Copy)]
pub(crate) struct Read<'a> {
    pub(crate) allocation: Allocation<'a>,
}
impl<'a> Read<'a> {
    pub(crate) fn new(allocation: Allocation<'a>) -> Self {
        Self { allocation }
    }
    pub(crate) fn semantic(self, code: &'static str, native: impl FnOnce() -> String) -> ReadError {
        let native_detail = match self.allocation {
            Allocation::Native => Some(native()),
            Allocation::Managed(_) => None,
        };
        ReadError::Semantic {
            code,
            native_detail,
        }
    }
    pub(crate) fn invalid(self, code: &'static str) -> ReadError {
        ReadError::Semantic {
            code,
            native_detail: None,
        }
    }
    pub(crate) fn scalar(self, node: &Node) -> R<&str> {
        node.text()
            .ok_or_else(|| self.invalid("list where a scalar is expected"))
    }
    pub(crate) fn int(self, node: &Node) -> R<i64> {
        let text = self.scalar(node)?;
        scalar::integer(text)
            .ok_or_else(|| self.semantic("not an integer", || format!("not an integer: {text}")))
    }
    pub(crate) fn plain_int(self, node: &Node) -> R<i64> {
        self.scalar(node)?
            .trim()
            .parse()
            .map_err(|_| self.invalid("not an integer"))
    }
    pub(crate) fn list(self, node: &Node) -> R<&[Node]> {
        node.as_list()
            .ok_or_else(|| self.invalid("scalar where a list is expected"))
    }
    pub(crate) fn offset(self, base: usize, step: usize, index: usize) -> R<usize> {
        step.checked_mul(index)
            .and_then(|span| base.checked_add(span))
            .ok_or_else(|| self.invalid("offset overflow"))
    }
    pub(crate) fn index<T>(self, values: &[T], index: usize) -> R<&T> {
        values.get(index).ok_or_else(|| {
            self.semantic("index out of range", || {
                format!("index {index} out of range")
            })
        })
    }
    pub(crate) fn at<T>(self, values: &[T], index: i64) -> R<&T> {
        let at = if index < 0 {
            i64::try_from(values.len())
                .ok()
                .and_then(|len| len.checked_add(index))
        } else {
            Some(index)
        };
        at.and_then(|at| usize::try_from(at).ok())
            .and_then(|at| values.get(at))
            .ok_or_else(|| {
                self.semantic("index out of range", || {
                    format!("index {index} out of range")
                })
            })
    }
    pub(crate) fn item(self, node: &Node, index: i64) -> R<&Node> {
        self.at(self.list(node)?, index)
    }
}

pub(crate) type NameResolver<'text, 'scope> = dyn Fn(&str) -> Option<&'text str> + 'scope;
