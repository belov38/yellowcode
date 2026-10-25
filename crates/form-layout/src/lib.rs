#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

pub mod check;
mod flow;
mod geom;
mod model;
pub mod rules;
pub mod scene;
mod title;
pub mod width;

pub use geom::{Rect, Size};
pub use model::*;
pub use ui_style::Interface;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    Workspace,
    Floating,
}

#[derive(Clone, Copy)]
pub struct LayoutInput<'a> {
    pub form: &'a form_model::FormDesc,
    pub types: &'a dyn form_model::TypeResolver,
    pub interface: Interface,
    pub content: Size,
    pub opened: Size,
    pub state: &'a ItemState,
    pub window: WindowMode,
    pub viewport: Size,
}

#[derive(Debug, Clone, Default)]
pub struct ItemState {
    pub hidden: std::collections::BTreeSet<String>,
    pub current_page: std::collections::BTreeMap<String, String>,
    pub choices: std::collections::BTreeMap<String, Vec<String>>,
    pub table_rows: std::collections::BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutError {
    Unsupported(&'static str),
}

pub fn layout(input: &LayoutInput) -> Result<Layout, LayoutError> {
    let rules = rules::rules(input.interface)?;
    flow::layout(input, rules)
}

pub fn content_box(viewport: Size) -> Rect {
    let r = &rules::TAXI;
    Rect {
        x: 0.0,
        y: r.content_top,
        w: viewport.w.max(0.0),
        h: (viewport.h - r.content_chrome).max(0.0),
    }
}
