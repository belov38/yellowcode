mod baltic;
mod bg;
mod fi;
mod fr;
mod hu;
mod ka;
mod lt;
mod lv;
mod ro;
mod tr;
mod vi;
mod zh;

use crate::data::SplNode;
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub(crate) trait Grammar: Send + Sync {
    fn spell(&self, n: u64, gender: u8, extra: &[String]) -> String;
    fn fraction_words(&self, digits: &str, gender: u8, extra: &[String]) -> String {
        self.spell(digits.parse().unwrap_or(0), gender, extra)
    }
    fn fraction_digits(&self, digits: &str) -> String {
        digits.to_string()
    }
    fn layout(&self) -> TreeLayout;
    fn subject_shape(&self) -> SubjectShape;
    fn gender(&self, _field: &str) -> u8 {
        0
    }
    fn form(&self, _n: u64, _part: Part) -> usize {
        0
    }
    fn joint(&self) -> Option<&str> {
        None
    }
    fn capital(&self) -> Capital {
        Capital::Upper
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SubjectShape {
    pub int_forms: usize,
    pub frac_forms: usize,
    pub gendered: bool,
    pub extra: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Int,
    FractionDigits,
    FractionWords,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Capital {
    Upper,
    Turkic,
    Keep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TreeLayout {
    Single,
    Spaced { round: bool },
    Comma,
    Units,
}

type Build = fn(&SplNode) -> Option<Box<dyn Grammar>>;
const BUILDERS: &[(&str, Build)] = &[
    ("bg", bg::build),
    ("fi", fi::build),
    ("fr", fr::build),
    ("hu", hu::build),
    ("ka", ka::build),
    ("lt", lt::build),
    ("lv", lv::build),
    ("ro", ro::build),
    ("tr", tr::build),
    ("vi", vi::build),
    ("zh", zh::build),
];

fn registry() -> &'static BTreeMap<&'static str, Box<dyn Grammar>> {
    static GRAMMARS: OnceLock<BTreeMap<&'static str, Box<dyn Grammar>>> = OnceLock::new();
    GRAMMARS.get_or_init(|| {
        let trees = &crate::format::words().spell_other;
        BUILDERS
            .iter()
            .filter_map(|(language, build)| Some((*language, build(trees.get(*language)?)?)))
            .collect()
    })
}

pub(crate) fn grammar(language: &str) -> Option<&'static dyn Grammar> {
    registry().get(language).map(Box::as_ref)
}

pub fn languages() -> Vec<&'static str> {
    registry().keys().copied().collect()
}

pub(super) fn list(node: &SplNode) -> Option<&[SplNode]> {
    match node {
        SplNode::List(items) => Some(items),
        _ => None,
    }
}
pub(super) fn text(node: &SplNode) -> Option<&str> {
    match node {
        SplNode::Str(value) => Some(value),
        _ => None,
    }
}
pub(super) fn int(node: &SplNode) -> Option<i64> {
    match node {
        SplNode::Int(value) => Some(*value),
        _ => None,
    }
}
pub(super) fn texts(node: &SplNode, len: usize) -> Option<Vec<String>> {
    let values = list(node)?;
    (values.len() == len).then_some(())?;
    values
        .iter()
        .map(|node| Some(text(node)?.to_string()))
        .collect()
}
pub(super) fn top(tree: &SplNode, len: usize) -> Option<&[SplNode]> {
    let items = list(tree)?;
    (items.len() == len && items.iter().all(|item| list(item).is_some())).then_some(items)
}
