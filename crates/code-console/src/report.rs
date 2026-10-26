use bsl_vm::{ErrorInfo, FatalError, StackEntry, UserMessage};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Done,
    Error,
    Cancelled,
    Budget,
    Busy,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageOut {
    pub text: String,
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackOut {
    pub module: String,
    pub line: u32,
    pub source_line: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeError {
    pub chain: Vec<String>,
    pub module: String,
    pub line: u32,
    pub source_line: String,
    pub stack: Vec<StackOut>,
    pub at: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub outcome: Outcome,
    pub messages: Vec<MessageOut>,
    pub dropped: usize,
    pub error: Option<CodeError>,
    pub elapsed_ms: u64,
}

impl RunReport {
    pub(crate) fn empty(outcome: Outcome) -> RunReport {
        RunReport {
            outcome,
            messages: Vec::new(),
            dropped: 0,
            error: None,
            elapsed_ms: 0,
        }
    }

    pub(crate) fn bare(outcome: Outcome, text: &str) -> RunReport {
        RunReport {
            error: Some(CodeError::text(text)),
            ..RunReport::empty(outcome)
        }
    }

    pub(crate) fn with_messages(mut self, kept: Vec<UserMessage>, dropped: usize) -> RunReport {
        self.messages = kept
            .into_iter()
            .map(|m| MessageOut {
                text: m.text,
                status: m.status,
            })
            .collect();
        self.dropped = dropped;
        self
    }
}

impl CodeError {
    pub(crate) fn text(text: &str) -> CodeError {
        CodeError {
            chain: vec![text.to_string()],
            module: String::new(),
            line: 0,
            source_line: String::new(),
            stack: Vec::new(),
            at: None,
        }
    }

    pub(crate) fn of_info(info: &ErrorInfo, fragment: &str) -> CodeError {
        let mut chain = vec![info.description.to_string_lossy()];
        let mut next = info.cause.clone();
        while let Some(c) = next {
            chain.push(c.description.to_string_lossy());
            next = c.cause.clone();
        }
        let stack: Vec<StackOut> = info.stack.iter().map(stack_out).collect();
        let at = info
            .stack
            .last()
            .filter(|f| f.module_name.to_string_lossy() == fragment)
            .map(|f| f.line);
        CodeError {
            chain,
            module: info.module_name.to_string_lossy(),
            line: info.line,
            source_line: info.source_line.to_string_lossy(),
            stack,
            at,
        }
    }

    pub(crate) fn of_fatal(f: &FatalError, fragment: &str) -> CodeError {
        match &f.cause {
            Some(cause) => {
                let mut e = CodeError::of_info(cause, fragment);
                e.chain.insert(0, f.text.clone());
                e
            }
            None => CodeError::text(&f.text),
        }
    }
}

fn stack_out(f: &StackEntry) -> StackOut {
    StackOut {
        module: f.module_name.to_string_lossy(),
        line: f.line,
        source_line: f.source_line.to_string_lossy(),
    }
}
