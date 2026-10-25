use std::collections::BTreeMap;

use crate::props::Props;
use crate::types::{Resolved, TypeDesc, TypeKind, TypeResolver};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct LStr(pub Vec<(String, String)>);

impl LStr {
    pub fn get(&self, lang: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(l, _)| l == lang)
            .map(|(_, t)| t.as_str())
    }
    pub fn ru(&self) -> Option<&str> {
        self.get("ru")
    }
    pub fn any(&self) -> Option<&str> {
        self.ru()
            .or_else(|| self.0.first().map(|(_, t)| t.as_str()))
    }
}

macro_rules! item_kinds {
    ($($v:ident),* $(,)?) => {
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
        pub enum ItemKind { $($v,)* Stub(String) }
        impl ItemKind {
            pub fn from_xml(tag: &str) -> ItemKind {
                match tag { $(stringify!($v) => ItemKind::$v,)* _ => ItemKind::Stub(tag.to_string()) }
            }
            pub fn xml(&self) -> &str {
                match self { $(ItemKind::$v => stringify!($v),)* ItemKind::Stub(s) => s }
            }
        }
    };
}

item_kinds!(
    UsualGroup,
    InputField,
    Table,
    LabelField,
    CheckBoxField,
    LabelDecoration,
    Pages,
    Page,
    PictureDecoration,
    ButtonGroup,
    ColumnGroup,
    PictureField,
    RadioButtonField,
    CommandBar,
    Popup,
    SpreadSheetDocumentField,
    Button,
    AutoCommandBar,
    ContextMenu,
    ExtendedTooltip,
    SearchStringAddition,
    SearchControlAddition,
    ViewStatusAddition,
);

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Item {
    pub id: i32,
    pub name: String,
    pub kind: ItemKind,
    pub props: Props,
    pub children: Vec<Item>,
    pub parts: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum PictureRef {
    Std(String),
    Config(String),
    Embedded(String),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Attribute {
    pub id: i32,
    pub name: String,
    pub title: Option<LStr>,
    pub ty: TypeDesc,
    pub main: bool,
    pub columns: Vec<Column>,
    pub main_table: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Column {
    pub name: String,
    pub title: Option<LStr>,
    pub ty: TypeDesc,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Command {
    pub name: String,
    pub title: Option<LStr>,
    pub picture: Option<PictureRef>,
    pub representation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ParseStats {
    pub skipped: BTreeMap<String, u32>,
    pub stubs: BTreeMap<String, u32>,
    pub bad_values: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FormDesc {
    pub title: Option<LStr>,
    pub width: Option<u32>,
    pub auto_command_bar: Option<Item>,
    pub items: Vec<Item>,
    pub attributes: Vec<Attribute>,
    pub commands: Vec<Command>,
    pub events: Vec<(String, String)>,
    pub stats: ParseStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<crate::stdcmd::FormRole>,
    #[serde(default)]
    pub name_lang: crate::stdcmd::NameLang,
}

fn find_in<'a>(items: &'a [Item], name: &str) -> Option<&'a Item> {
    for it in items {
        if it.name == name {
            return Some(it);
        }
        if let Some(f) = find_in(&it.children, name).or_else(|| find_in(&it.parts, name)) {
            return Some(f);
        }
    }
    None
}

impl FormDesc {
    pub fn find(&self, name: &str) -> Option<&Item> {
        if let Some(b) = &self.auto_command_bar {
            if b.name == name {
                return Some(b);
            }
            if let Some(f) = find_in(&b.children, name).or_else(|| find_in(&b.parts, name)) {
                return Some(f);
            }
        }
        find_in(&self.items, name)
    }

    pub fn attribute(&self, name: &str) -> Option<&Attribute> {
        self.attributes.iter().find(|a| a.name == name)
    }

    pub fn resolve(&self, data_path: &str, r: &dyn TypeResolver) -> Option<Resolved> {
        let segs: Vec<&str> = data_path.split('.').collect();
        let (first, rest) = segs.split_first()?;
        let a = self.attribute(first)?;
        if rest.is_empty() {
            return Some(Resolved {
                ty: a.ty.clone(),
                title: a.title.clone(),
            });
        }
        match a.ty.kind() {
            TypeKind::ValueTable | TypeKind::ValueTree => {
                let c = a.columns.iter().find(|c| c.name == rest[0])?;
                (rest.len() == 1).then(|| Resolved {
                    ty: c.ty.clone(),
                    title: c.title.clone(),
                })
            }
            TypeKind::DynamicList if a.main_table.is_some() => {
                r.resolve(a.main_table.as_deref()?, rest)
            }
            _ => r.resolve(a.ty.types.first()?, rest),
        }
    }
}
