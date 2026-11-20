use super::super::shell_xml::N;
use crate::{
    Result,
    xml::{XmlAttr, XmlNode},
};
use std::mem::size_of;

pub(crate) struct FragmentAdmission<'a> {
    pub pay: &'a dyn Fn(u64) -> Result<()>,
    pub check: &'a dyn Fn() -> Result<()>,
}
impl FragmentAdmission<'_> {
    pub const LEGACY: Self = Self {
        pay: &|_| Ok(()),
        check: &|| Ok(()),
    };
}

fn allocation() -> crate::Error {
    std::io::Error::new(
        std::io::ErrorKind::OutOfMemory,
        "native XML fragment allocation",
    )
    .into()
}
fn add(total: &mut u64, bytes: usize) -> Result<()> {
    *total = total
        .checked_add(u64::try_from(bytes).map_err(|_| allocation())?)
        .ok_or_else(allocation)?;
    Ok(())
}

pub(super) fn measure(n: N<'_, '_>, check: &dyn Fn() -> Result<()>) -> Result<u64> {
    check()?;
    let mut bytes = 0;
    add(&mut bytes, size_of::<XmlNode>())?;
    add(&mut bytes, n.local().len())?;
    for text in n.ns().into_iter().chain(n.prefix()).chain(n.text()) {
        add(&mut bytes, text.len())?;
    }
    for attr in n.attrs() {
        check()?;
        add(&mut bytes, size_of::<XmlAttr>())?;
        add(&mut bytes, attr.local().len())?;
        add(&mut bytes, attr.value().len())?;
        for text in attr.ns().into_iter().chain(attr.prefix()) {
            add(&mut bytes, text.len())?;
        }
    }
    for (prefix, uri) in n.ns_decls() {
        check()?;
        add(&mut bytes, size_of::<(Option<String>, String)>())?;
        add(&mut bytes, uri.len())?;
        add(&mut bytes, prefix.map_or(0, str::len))?;
    }
    for child in n.children() {
        bytes = bytes
            .checked_add(measure(child, check)?)
            .ok_or_else(allocation)?;
    }
    check()?;
    Ok(bytes)
}

fn count<T>(mut items: impl Iterator<Item = T>, check: &dyn Fn() -> Result<()>) -> Result<usize> {
    items.try_fold(0usize, |count, _| {
        check()?;
        count.checked_add(1).ok_or_else(allocation)
    })
}
fn slots<T>(len: usize) -> Result<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| allocation())?;
    Ok(out)
}
fn string(text: &str, check: &dyn Fn() -> Result<()>) -> Result<String> {
    check()?;
    let mut out = String::new();
    out.try_reserve_exact(text.len())
        .map_err(|_| allocation())?;
    let mut start = 0;
    while start < text.len() {
        check()?;
        let mut end = start.saturating_add(64 << 10).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str(&text[start..end]);
        start = end;
    }
    check()?;
    Ok(out)
}
fn optional(text: Option<&str>, check: &dyn Fn() -> Result<()>) -> Result<Option<String>> {
    text.map(|s| string(s, check)).transpose()
}

pub(super) fn copy(n: N<'_, '_>, check: &dyn Fn() -> Result<()>) -> Result<XmlNode> {
    check()?;
    let mut attrs = slots(count(n.attrs(), check)?)?;
    for attr in n.attrs() {
        check()?;
        attrs.push(XmlAttr {
            ns: optional(attr.ns(), check)?,
            local: string(attr.local(), check)?,
            prefix: optional(attr.prefix(), check)?,
            value: string(attr.value(), check)?,
        });
    }
    let mut ns_decls = slots(count(n.ns_decls(), check)?)?;
    for (prefix, uri) in n.ns_decls() {
        check()?;
        ns_decls.push((optional(prefix, check)?, string(uri, check)?));
    }
    let mut children = slots(count(n.children(), check)?)?;
    for child in n.children() {
        children.push(copy(child, check)?);
    }
    let out = XmlNode {
        ns: optional(n.ns(), check)?,
        local: string(n.local(), check)?,
        prefix: optional(n.prefix(), check)?,
        attrs,
        ns_decls,
        children,
        text: optional(n.text(), check)?,
    };
    check()?;
    Ok(out)
}
