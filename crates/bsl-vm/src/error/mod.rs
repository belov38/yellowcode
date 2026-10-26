mod category;
pub(crate) mod info;
mod present;
mod wire;
pub(crate) mod wrap;

pub use category::{Category, CategorySet};
pub use wire::{ErrorWireIdentity, ErrorWireIdentityParts, ErrorWireMetadata};

use crate::value::Value;
use bsl_prim::Str;
use platform_catalog::messages::{self, Lang};
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum Exception {
    Catchable(Rc<ErrorInfo>),
    Fatal(Rc<FatalError>),
}

#[derive(Debug)]
pub struct FatalError {
    pub kind: FatalKind,
    pub text: String,
    pub cause: Option<Rc<ErrorInfo>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatalKind {
    StackOverflow,
    ModuleInit,
    Budget,
    Cancelled,
    Memory,
    Internal,
    Later {
        what: &'static str,
        stage: &'static str,
    },
}

#[derive(Debug)]
pub struct ErrorInfo {
    pub description: Str,
    pub module_name: Str,
    pub line: u32,
    pub source_line: Str,
    pub cause: Option<Rc<ErrorInfo>>,
    pub code: Str,
    pub additional: Value,
    pub categories: CategorySet,
    pub stack: Vec<StackEntry>,
    pub key: Option<&'static str>,
    pub(crate) wire: Option<Box<ErrorWireMetadata>>,
}

impl Drop for ErrorInfo {
    fn drop(&mut self) {
        let mut next = self.cause.take();
        while let Some(rc) = next {
            next = match Rc::try_unwrap(rc) {
                Ok(mut info) => info.cause.take(),
                Err(_) => None,
            };
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackEntry {
    pub module_name: Str,
    pub line: u32,
    pub source_line: Str,
}

#[derive(Debug)]
pub struct HostError {
    pub msg: MsgRef,
    pub arg: Option<String>,
    pub categories: Option<CategorySet>,
    pub cause: Option<Cause>,
    pub raw: bool,
}

#[derive(Debug)]
pub enum MsgRef {
    Key(MsgKey),
    Text { ru: String, en: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MsgKey {
    pub key: &'static str,
    pub args: Vec<String>,
    pub suffix: String,
}

#[derive(Debug)]
pub enum Cause {
    Host(Box<HostError>),
    Bsl(Rc<ErrorInfo>),
}

impl HostError {
    pub fn key(key: &'static str) -> HostError {
        HostError::from_msg(MsgRef::Key(MsgKey {
            key,
            args: Vec::new(),
            suffix: String::new(),
        }))
    }

    pub fn text(ru: &str, en: &str) -> HostError {
        HostError::from_msg(MsgRef::Text {
            ru: ru.into(),
            en: en.into(),
        })
    }

    pub fn not_implemented(type_name: &str, member: &str) -> HostError {
        let mut e = HostError::text(
            &format!("Не реализовано в реплике ({type_name}.{member})"),
            &format!("Not implemented in the replica ({type_name}.{member})"),
        );
        e.categories = Some(CategorySet::of(&[
            Category::AllErrors,
            Category::ScriptRuntimeError,
            Category::OtherError,
        ]));
        e
    }

    pub fn with_arg(mut self, a: impl Into<String>) -> HostError {
        self.arg = Some(a.into());
        self
    }

    pub fn raw(mut self) -> HostError {
        self.raw = true;
        self
    }

    pub fn caused_by(mut self, c: Cause) -> HostError {
        self.cause = Some(c);
        self
    }

    fn from_msg(msg: MsgRef) -> HostError {
        HostError {
            msg,
            arg: None,
            categories: None,
            cause: None,
            raw: false,
        }
    }

    pub(crate) fn render(&self, lang: Lang) -> String {
        let mut text = self.msg.render(lang);
        if let Some(a) = self.arg.as_deref().filter(|a| !a.is_empty()) {
            text.push_str(" (");
            text.push_str(a);
            text.push(')');
        }
        text
    }
}

impl MsgRef {
    fn render(&self, lang: Lang) -> String {
        match self {
            MsgRef::Text { ru, en } => match lang {
                Lang::Ru => ru.clone(),
                Lang::En => en.clone(),
            },
            MsgRef::Key(k) => catalog_text(k.key, &k.args, &k.suffix, lang),
        }
    }
}

pub(crate) fn catalog_text(key: &str, args: &[String], suffix: &str, lang: Lang) -> String {
    messages::render_key(key, args, suffix, lang).unwrap_or_else(|| {
        let mut text = messages::fill(key, args);
        text.push_str(suffix);
        text
    })
}
