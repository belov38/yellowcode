use md_model::{Child, ChildTag};

use super::Names;
use super::ctx::{Ctx, Props};
use super::fields::item_name;
use crate::error::{Error, Result};
use crate::logical::{Index, Table};
use crate::names::{field, purpose as p};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tok<'a> {
    F(&'a str),
    Owner,
    Attr,
    AddOrder(&'a str),
    AddOrderAll,
    Dims,
    DimsInTotals,
    OtherDims,
    BaseDims,
    Addressing,
    List(Vec<String>),
}

pub(crate) fn index(
    name: impl Into<String>,
    unique: bool,
    fields: Vec<String>,
    flags: (bool, bool),
) -> Index {
    let mut ix = Index::new(name, unique, fields);
    (ix.clustered, ix.flag2) = flags;
    ix
}

pub(crate) fn indexed(c: &Child) -> bool {
    matches!(
        c.text("Indexing"),
        Some("Index" | "IndexWithAdditionalOrder")
    )
}

pub(crate) fn add_order(c: &Child) -> bool {
    c.text("Indexing") == Some("IndexWithAdditionalOrder")
}

pub(crate) fn table_purpose(name: &str) -> &str {
    name.trim_end_matches(|c: char| c.is_ascii_digit())
}

fn need<'c>(item: Option<&'c Child>, what: &str) -> Result<&'c Child> {
    item.ok_or_else(|| Error::Unsupported(format!("index rule {what} without an item")))
}

pub(crate) fn name(template: &str, item: Option<&Child>, names: &mut Names) -> Result<String> {
    if let Some((pre, post)) = template.split_once("{n}") {
        let it = need(item, template)?;
        let n = names.number(it.id, pre)?;
        Ok(format!("{pre}{n}{post}"))
    } else if template.contains("{F}") {
        let it = need(item, template)?;
        Ok(template.replace("{F}", &item_name(it, names)?))
    } else {
        Ok(template.to_string())
    }
}

fn push_all<'c>(
    out: &mut Vec<String>,
    items: impl Iterator<Item = &'c Child>,
    names: &mut Names,
) -> Result<()> {
    for c in items {
        out.push(item_name(c, names)?);
    }
    Ok(())
}

pub(crate) fn fields(
    toks: &[Tok],
    t: &Table,
    item: Option<&Child>,
    cx: &Ctx,
    names: &mut Names,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for tok in toks {
        match tok {
            Tok::F(n) => {
                if t.field(n).is_some() {
                    out.push((*n).to_string());
                }
            }
            Tok::Owner => out.push(field::ID.to_string()),
            Tok::Attr => out.push(item_name(need(item, "@attr")?, names)?),
            Tok::AddOrder(n) => {
                if item.is_some_and(add_order) && t.field(n).is_some() {
                    out.push((*n).to_string());
                }
            }
            Tok::AddOrderAll => push_all(
                &mut out,
                cx.obj.attributes().filter(|a| add_order(a)),
                names,
            )?,
            Tok::Dims => push_all(&mut out, cx.obj.dimensions(), names)?,
            Tok::DimsInTotals => push_all(
                &mut out,
                cx.obj
                    .dimensions()
                    .filter(|d| d.bool_or("UseInTotals", true)),
                names,
            )?,
            Tok::OtherDims => {
                let id = need(item, "@other_dims")?.id;
                push_all(&mut out, cx.obj.dimensions().filter(|d| d.id != id), names)?
            }
            Tok::BaseDims => push_all(
                &mut out,
                cx.obj.dimensions().filter(|d| d.flag("BaseDimension")),
                names,
            )?,
            Tok::Addressing => push_all(
                &mut out,
                cx.obj
                    .children
                    .iter()
                    .filter(|c| c.tag == ChildTag::AddressingAttribute),
                names,
            )?,
            Tok::List(v) => out.extend(v.iter().cloned()),
        }
    }
    Ok(out)
}

pub(crate) fn flags(purpose: &str, index: &str, cx: &Ctx) -> (bool, bool) {
    const C: (bool, bool) = (true, false);
    const NONE: (bool, bool) = (false, false);
    match (purpose, index) {
        (p::CONST, "ByKey") => C,
        (x, "ByDataKey") if x.ends_with(p::CHNGR_SUFFIX) => C,
        (p::DOCUMENT_JOURNAL, "ByDoc") => C,
        (p::SEQ, "ByRecorder") | (p::SEQ_B, "ByDims") => C,
        (p::INFO_RG, "ByDims") => (!cx.sub || cx.periodic, false),
        (p::INFO_RG, "ByRecorder") => (!cx.periodic, true),
        (p::INFO_RG_SL | p::INFO_RG_SF, "ByDims") => C,
        (p::ACCUM_RG | p::ACC_RG, "ByPeriod") => C,
        (p::ACCUM_RG | p::ACC_RG, "ByRecorder") => (false, true),
        (p::ACCUM_RG_T | p::ACCUM_RG_TN, "ByDims") => C,
        (p::ACCUM_RG_OPT | p::ACC_RG_OPT, "ByRegID") => C,
        (p::ACCUM_RG_AGG_GRID_K | p::ACCUM_RG_AGG_OPT_K, "ByID") => C,
        (x, "ByKey") if x.starts_with(p::ACCUM_RG_AGG_DICT) => C,
        (p::ACCUM_RG_ST, "ById") => C,
        (x, "ByPeriod") if x.starts_with(p::ACC_RG_AT) => C,
        (p::ACC_RG_CT | p::ACC_RG_ED, "ByPeriod") => C,
        (p::CRG, "ByRecorder") => (true, true),
        (p::CRG_ACT_P, "ByRecorder") => C,
        _ => NONE,
    }
}
