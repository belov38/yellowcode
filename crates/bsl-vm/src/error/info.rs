use super::wrap;
use super::{Category, CategorySet, Cause, ErrorInfo, HostError, MsgRef, StackEntry};
use crate::runtime::Execution;
use crate::value::Value;
use bsl_prim::Str;
use platform_catalog::messages::{self, Lang, Wrapper};
use std::rc::Rc;

pub(crate) enum CallKind {
    Vm,
    Global { builtin: bool },
    Method,
    Ctor { type_name: String },
    GetAttr,
    SetAttr,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Position {
    pub module_name: Str,
    pub line: u32,
    pub source_line: Str,
    pub stack: Vec<StackEntry>,
}

pub(crate) fn from_host(ex: &Execution, e: HostError, kind: CallKind, name: &str) -> Rc<ErrorInfo> {
    build(ex.lang(), ex.position(), e, &kind, name)
}

pub(crate) fn build(
    lang: Lang,
    pos: Position,
    e: HostError,
    kind: &CallKind,
    name: &str,
) -> Rc<ErrorInfo> {
    let own_key = key_of(&e);
    let wrapper = if e.raw {
        None
    } else {
        wrap::wrapper(kind, name, own_key)
    };
    let w = wrapper.map_or(Wrapper::None, |(w, _)| w);
    let own = wrap::own_categories(w);
    let categories = match e.categories {
        Some(c) => c.with(Category::AllErrors),
        None => {
            let chain = wrap::chain_categories(
                w,
                innermost_key(&e),
                wrapper.is_some() || e.cause.is_some(),
            );
            CategorySet(chain.0 & !own.0).with(Category::AllErrors)
        }
    };
    let Some((w, arg)) = wrapper else {
        return Rc::new(host_info(lang, pos, e, categories));
    };
    let mut description = wrap::wrapper_key(w)
        .and_then(|k| messages::render_key(k, &[], "", lang))
        .unwrap_or_default();
    description.push_str(" (");
    description.push_str(arg);
    description.push(')');
    let cause = host_info(lang, Position::default(), e, categories);
    Rc::new(ErrorInfo {
        description: Str::from(description),
        module_name: pos.module_name,
        line: pos.line,
        source_line: pos.source_line,
        cause: Some(Rc::new(cause)),
        code: Str::new(),
        additional: Value::Undefined,
        categories: own,
        stack: pos.stack,
        key: wrap::wrapper_key(w),
        wire: None,
    })
}

pub(crate) fn compile_failure(ex: &Execution, cause_text: String) -> Rc<ErrorInfo> {
    let w = Wrapper::Compile;
    let own = wrap::own_categories(w);
    let rest = CategorySet(wrap::default_categories(w, true).0 & !own.0).with(Category::AllErrors);
    let key = wrap::wrapper_key(w);
    let cause = link(Str::from(cause_text), Position::default(), rest, None, None);
    let pos = ex.position();
    let description = key
        .and_then(|k| messages::render_key(k, &[], "", ex.lang()))
        .unwrap_or_default();
    Rc::new(ErrorInfo {
        description: Str::from(description),
        module_name: pos.module_name,
        line: pos.line,
        source_line: pos.source_line,
        cause: Some(Rc::new(cause)),
        code: Str::new(),
        additional: Value::Undefined,
        categories: own,
        stack: pos.stack,
        key,
        wire: None,
    })
}

fn host_info(lang: Lang, pos: Position, e: HostError, categories: CategorySet) -> ErrorInfo {
    let description = Str::from(e.render(lang));
    let key = key_of(&e);
    let mut cause = None;
    let mut chain = Vec::new();
    let mut next = e.cause;
    while let Some(c) = next.take() {
        match c {
            Cause::Bsl(info) => cause = Some(info),
            Cause::Host(h) => {
                let h = *h;
                let own = h.categories.unwrap_or_default().with(Category::AllErrors);
                chain.push((Str::from(h.render(lang)), key_of(&h), own));
                next = h.cause;
            }
        }
    }
    for (description, key, categories) in chain.into_iter().rev() {
        cause = Some(Rc::new(link(
            description,
            Position::default(),
            categories,
            key,
            cause,
        )));
    }
    link(description, pos, categories, key, cause)
}

fn link(
    description: Str,
    pos: Position,
    categories: CategorySet,
    key: Option<&'static str>,
    cause: Option<Rc<ErrorInfo>>,
) -> ErrorInfo {
    ErrorInfo {
        description,
        module_name: pos.module_name,
        line: pos.line,
        source_line: pos.source_line,
        cause,
        code: Str::new(),
        additional: Value::Undefined,
        categories,
        stack: pos.stack,
        key,
        wire: None,
    }
}

fn key_of(e: &HostError) -> Option<&'static str> {
    match &e.msg {
        MsgRef::Key(k) => Some(k.key),
        MsgRef::Text { .. } => None,
    }
}

fn innermost_key(e: &HostError) -> Option<&'static str> {
    let mut cur = e;
    while let Some(Cause::Host(h)) = &cur.cause {
        cur = h;
    }
    key_of(cur)
}
