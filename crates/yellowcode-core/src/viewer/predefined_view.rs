use brace::Guid;
use mdconf::parts::{AccountType, PredefinedCode, PredefinedItem};
use mdconf::{ChildTag, MdKind, MdObject, MdRef};
use serde_json::{Value, json};

use super::out::Lang;
use super::path::{self, PathError, Target};
use super::types::type_text;

const KINDS: [MdKind; 4] = [
    MdKind::Catalog,
    MdKind::ChartOfCharacteristicTypes,
    MdKind::ChartOfAccounts,
    MdKind::ChartOfCalculationTypes,
];

#[derive(Debug)]
pub enum PredefinedError {
    Path(PathError),
    Kind,
}

impl std::fmt::Display for PredefinedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PredefinedError::Path(e) => e.fmt(f),
            PredefinedError::Kind => f.write_str("the object has no predefined data"),
        }
    }
}

pub const MAX_ITEMS: usize = 20_000;
pub const MAX_DEPTH: usize = 64;

pub fn predefined(
    cfg: &mdconf::Configuration,
    p: &str,
    lang: Lang,
) -> Result<Value, PredefinedError> {
    let o = match path::resolve(cfg, p).map_err(PredefinedError::Path)? {
        Target::Object(o) if KINDS.contains(&o.kind) => o,
        _ => return Err(PredefinedError::Kind),
    };
    Ok(view(o, lang, MAX_ITEMS, MAX_DEPTH))
}

fn view(o: &MdObject, lang: Lang, max_items: usize, max_depth: usize) -> Value {
    let flags = flag_columns(o);
    let mut w = Walk {
        flags: &flags,
        lang,
        left: max_items,
        max_depth,
        truncated: false,
    };
    let items = o
        .predefined()
        .map(|p| w.items(&p.items, 1))
        .unwrap_or_default();
    json!({
        "kind": o.kind.as_str(),
        "tree": o.kind != MdKind::ChartOfCalculationTypes,
        "available": o.predefined().is_some(),
        "truncated": w.truncated,
        "flags": flags.iter().map(|f| f.name).collect::<Vec<_>>(),
        "items": items,
    })
}

struct FlagColumn<'a> {
    id: Guid,
    name: &'a str,
    tail: String,
}

impl FlagColumn<'_> {
    fn is(&self, r: &MdRef) -> bool {
        match r.id {
            Some(id) if id != Guid::NIL && self.id != Guid::NIL => id == self.id,
            _ => r.name.ends_with(&self.tail),
        }
    }
}

fn flag_columns(o: &MdObject) -> Vec<FlagColumn<'_>> {
    if o.kind != MdKind::ChartOfAccounts {
        return Vec::new();
    }
    o.children
        .iter()
        .filter(|c| c.tag == ChildTag::AccountingFlag)
        .map(|c| FlagColumn {
            id: c.id,
            name: &c.name,
            tail: format!(".AccountingFlag.{}", c.name),
        })
        .collect()
}

fn code_text(c: &PredefinedCode) -> &str {
    match c {
        PredefinedCode::Str(s) | PredefinedCode::Number(s) => s,
    }
}

struct Walk<'a> {
    flags: &'a [FlagColumn<'a>],
    lang: Lang,
    left: usize,
    max_depth: usize,
    truncated: bool,
}

impl Walk<'_> {
    fn items(&mut self, items: &[PredefinedItem], depth: usize) -> Vec<Value> {
        let mut out = Vec::new();
        for i in items {
            if self.left == 0 {
                self.truncated = true;
                break;
            }
            self.left -= 1;
            out.push(self.item(i, depth));
        }
        out
    }

    fn item(&mut self, i: &PredefinedItem, depth: usize) -> Value {
        let mut o = serde_json::Map::new();
        o.insert("name".into(), json!(i.name));
        o.insert("code".into(), json!(code_text(&i.code)));
        o.insert("description".into(), json!(i.description));
        if i.is_folder {
            o.insert("folder".into(), json!(true));
        }
        if let Some(t) = &i.type_desc {
            o.insert("type".into(), json!(type_text(t, self.lang)));
        }
        if let Some(a) = &i.account {
            let account_type = match a.account_type {
                AccountType::Active => "Active",
                AccountType::Passive => "Passive",
                AccountType::ActivePassive => "ActivePassive",
            };
            let values: Vec<Option<bool>> = self
                .flags
                .iter()
                .map(|f| a.flags.iter().find(|(r, _)| f.is(r)).map(|(_, on)| *on))
                .collect();
            o.insert(
                "account".into(),
                json!({
                    "type": account_type,
                    "off_balance": a.off_balance,
                    "order": a.order,
                    "flags": values,
                }),
            );
        }
        if !i.children.is_empty() {
            if depth >= self.max_depth {
                self.truncated = true;
            } else {
                let children = self.items(&i.children, depth + 1);
                if !children.is_empty() {
                    o.insert("children".into(), Value::Array(children));
                }
            }
        }
        Value::Object(o)
    }
}
