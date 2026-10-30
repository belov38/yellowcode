use query_syntax::{Lang, MsgId, QueryError, ScriptVariant};

use crate::host::HostError;

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeError {
    pub chain: Vec<QueryError>,
}

impl RuntimeError {
    pub fn sql(detail: QueryError) -> RuntimeError {
        let computing = QueryError::new(MsgId::DBERROR_COMPUTING, None).with_cause(detail.clone());
        RuntimeError {
            chain: vec![
                QueryError::new(MsgId::SELECT_ERROR_RUNTIMEERROR, None),
                computing,
                detail,
            ],
        }
    }

    pub fn unique_index() -> RuntimeError {
        let dup = QueryError::new(MsgId::DBERROR_INDEXUNIQUE, None);
        RuntimeError {
            chain: vec![
                QueryError::new(MsgId::SELECT_ERROR_RUNTIMEERROR, None),
                QueryError::new(MsgId::DBERROR_UNIQUEINDEX, None).with_cause(dup.clone()),
                dup,
            ],
        }
    }

    pub fn with_tail(mut self, e: QueryError) -> RuntimeError {
        self.chain.push(e);
        self
    }

    pub fn messages(&self, lang: Lang, script: ScriptVariant) -> Vec<String> {
        self.chain
            .iter()
            .map(|e| {
                let mut s = e.message(lang, script);
                if matches!(e.msg, MsgId::DBERROR_COMPUTING | MsgId::DBERROR_UNIQUEINDEX)
                    && let Some(c) = &e.cause
                {
                    s.push('\n');
                    s.push_str(&c.message(lang, script));
                }
                s
            })
            .collect()
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.messages(Lang::Ru, ScriptVariant::Russian).join("\n"))
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeError {
    RestartRequired {
        memory: Option<memory_budget::MemoryLimit>,
    },
    Status(i32),
}

impl NativeError {
    pub const fn public_text(self) -> &'static str {
        match self {
            NativeError::RestartRequired { memory: Some(_) } => {
                "MEMORY_LIMIT: native: restart the application"
            }
            NativeError::RestartRequired { memory: None } => {
                "native ICU allocation failed: restart the application"
            }
            NativeError::Status(_) => "native ICU error",
        }
    }
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NativeError::Status(code) => write!(f, "{} {code}", self.public_text()),
            _ => f.write_str(self.public_text()),
        }
    }
}

#[derive(Clone, Debug)]
pub enum ExecError {
    Runtime(RuntimeError),
    Rights(RuntimeError),
    Host(HostError),
    Fatal(String),
    Later {
        what: &'static str,
        stage: &'static str,
    },
    Memory(memory_budget::MemoryLimit),
    Structure(memory_budget::StructureLimit),
    Native(NativeError),
}

impl ExecError {
    pub const fn structure_text(limit: memory_budget::StructureLimit) -> &'static str {
        match limit {
            memory_budget::StructureLimit::Depth => "nesting depth limit 128 exceeded",
            memory_budget::StructureLimit::Statements => "statements limit 256 exceeded",
            memory_budget::StructureLimit::Columns => "columns limit 4096 exceeded",
        }
    }
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Runtime(e) | ExecError::Rights(e) => e.fmt(f),
            ExecError::Host(h) => f.write_str(&h.message),
            ExecError::Fatal(s) => f.write_str(s),
            ExecError::Later { what, stage } => write!(f, "{what}: {stage}"),
            ExecError::Memory(m) => f.write_str(m.public_text()),
            ExecError::Native(n) => n.fmt(f),
            ExecError::Structure(l) => {
                write!(
                    f,
                    "MEMORY_LIMIT: execution: {}",
                    ExecError::structure_text(*l)
                )
            }
        }
    }
}

impl std::error::Error for ExecError {}

const _: fn() = || {
    fn is<T: Send + Sync>() {}
    is::<ExecError>();
};
