use std::collections::HashMap;

use md_model::{MdKind, MdObject, Value};

use super::ctx::{Ctx, Lookup, Props, full_name};
use super::types::{classes, logical_types};
use super::{Names, TableBuilder};
use crate::error::{Error, Result};
use crate::logical::{Field, LType};
use crate::names::purpose;
use crate::values::Uuid16;

pub(crate) fn is_separable(kind: MdKind) -> bool {
    use MdKind::*;
    matches!(
        kind,
        Catalog
            | Document
            | Constant
            | ChartOfCharacteristicTypes
            | ChartOfAccounts
            | ChartOfCalculationTypes
            | ExchangePlan
            | InformationRegister
            | AccumulationRegister
            | AccountingRegister
            | CalculationRegister
            | BusinessProcess
            | Task
            | Sequence
            | DocumentJournal
    )
}

pub(crate) fn is_separator(ca: &MdObject) -> bool {
    ca.text("DataSeparation") == Some("Separate")
}

pub(crate) fn is_independent(ca: &MdObject) -> bool {
    ca.text("SeparatedDataUse")
        .is_none_or(|u| u == "Independently")
}

struct Rule<'a> {
    ca: &'a MdObject,
    auto: bool,
    content: HashMap<String, Option<bool>>,
}

#[derive(Default)]
pub(crate) struct CommonAttrs<'a> {
    rules: Vec<Rule<'a>>,
}

impl<'a> CommonAttrs<'a> {
    pub fn new(cas: &[&'a MdObject]) -> CommonAttrs<'a> {
        let rules = cas
            .iter()
            .map(|&ca| Rule {
                ca,
                auto: ca.text("AutoUse").is_none_or(|u| u == "Use"),
                content: content(ca),
            })
            .collect();
        CommonAttrs { rules }
    }

    pub fn applied(&self, full_name: &str) -> Vec<&'a MdObject> {
        self.rules
            .iter()
            .filter(|r| {
                r.content
                    .get(full_name)
                    .copied()
                    .flatten()
                    .unwrap_or(r.auto)
            })
            .map(|r| r.ca)
            .collect()
    }
}

fn content(ca: &MdObject) -> HashMap<String, Option<bool>> {
    let mut out = HashMap::new();
    if let Some(Value::Xml(e)) = ca.value("Content") {
        for it in e.children.iter().filter(|c| c.name == "xr:Item") {
            let text = |n: &str| {
                it.children
                    .iter()
                    .find(|c| c.name == n)
                    .and_then(|c| c.text.as_deref())
            };
            let Some(name) = text("xr:Metadata") else {
                continue;
            };
            let used = match text("xr:Use") {
                Some("Use") => Some(true),
                Some("DontUse") => Some(false),
                _ => None,
            };
            out.entry(name.to_string()).or_insert(used);
        }
    }
    out
}

pub(crate) fn applied<'a>(cx: &Ctx<'a>) -> Vec<&'a MdObject> {
    if is_separable(cx.kind) {
        cx.md.common.applied(&cx.full_name)
    } else {
        Vec::new()
    }
}

pub(crate) fn separators<'a>(cx: &Ctx<'a>) -> Vec<&'a MdObject> {
    let mut v = applied(cx);
    v.retain(|ca| is_separator(ca));
    v
}

fn value_types(cx: &Ctx, ca: &MdObject, names: &mut Names) -> Result<Vec<LType>> {
    let at = full_name(ca);
    let Some(Value::Type(td)) = ca.value("Type") else {
        return Err(Error::Unsupported(format!("{at}: no type")));
    };
    let cl = classes(cx, td, &at, names)?;
    logical_types(&cl, false).map_err(|e| super::in_object(&at, e))
}

pub(crate) fn common_group(b: &mut TableBuilder, cx: &Ctx, names: &mut Names) -> Result<()> {
    add(b, cx, names, applied(cx))
}

pub(crate) fn separators_group(b: &mut TableBuilder, cx: &Ctx, names: &mut Names) -> Result<()> {
    add(b, cx, names, separators(cx))
}

fn add(b: &mut TableBuilder, cx: &Ctx, names: &mut Names, cas: Vec<&MdObject>) -> Result<()> {
    let mut cols = Vec::with_capacity(cas.len());
    for ca in cas {
        cols.push((names.number(ca.id, purpose::FLD)?, ca));
    }
    let (mut all, mut independent) = (Vec::new(), Vec::new());
    for (n, ca) in cols {
        let name = format!("{}{n}", purpose::FLD);
        let ca_cx = Ctx::new(cx.md, ca)?;
        let types = value_types(&ca_cx, ca, names)?;
        b.item(
            Field::new(name.as_str(), false, types),
            ca.id,
            full_name(ca),
        );
        if is_separator(ca) {
            if is_independent(ca) {
                independent.push(name.clone());
            }
            all.push(name);
        }
    }
    if !all.is_empty() {
        b.table.splitters[0] = vec![all];
    }
    if !independent.is_empty() && cx.kind != MdKind::ExchangePlan {
        b.table.splitters[1] = vec![independent];
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ServiceSep {
    pub id: Uuid16,
    pub md_name: String,
    pub fld: String,
    pub types: Vec<LType>,
    pub independent: bool,
}

pub(crate) fn config_separators(md: &Lookup, names: &mut Names) -> Result<Vec<ServiceSep>> {
    let mut cols = Vec::new();
    for &ca in md.of_kind(MdKind::CommonAttribute) {
        if is_separator(ca) {
            cols.push((names.number(ca.id, purpose::FLD)?, ca));
        }
    }
    cols.into_iter()
        .map(|(n, ca)| {
            let cx = Ctx::new(md, ca)?;
            Ok(ServiceSep {
                id: ca.id,
                md_name: full_name(ca),
                fld: format!("{}{n}", purpose::FLD),
                types: value_types(&cx, ca, names)?,
                independent: is_independent(ca),
            })
        })
        .collect()
}
