use super::{Asset, BuildError, Frame, InverseLookup, R, encode_tree, nodes::Cx, xml};
use brace::Node;
use mdconf_core::xml::view::NodeRef;
use memory_budget::{Charged, MemoryBudget};
use std::sync::atomic::{AtomicBool, Ordering};

const FORM: &str = "http://v8.1c.ru/8.3/xcf/logform";
const SHEET_TYPE: &str = "e603103e-a318-4edc-a014-b1c6cf94d49f";

pub fn build_form_value(
    source: NodeRef<'_, '_>,
    assets: &[Asset<'_>],
    lookup: &dyn InverseLookup,
    budget: &MemoryBudget,
    cancel: &AtomicBool,
) -> R<Charged<Node>> {
    build_form_value_checked(source, assets, lookup, budget, &|| {
        cancel.load(Ordering::Relaxed)
    })
}

pub fn build_form_value_checked(
    source: NodeRef<'_, '_>,
    assets: &[Asset<'_>],
    lookup: &dyn InverseLookup,
    budget: &MemoryBudget,
    cancelled: &dyn Fn() -> bool,
) -> R<Charged<Node>> {
    let cx = Cx { budget, cancelled };
    cx.check()?;
    admit(source, cx)?;
    if source.local() != "Settings" || source.ns() != Some(FORM) {
        return Err(BuildError::Invalid("form settings namespace"));
    }
    let kind = xml::attr(source, Some(xml::XSI), "type")
        .ok_or(BuildError::Invalid("form settings type"))?;
    if xml::qname(source, kind)? != (xml::NS, "SpreadsheetDocument") {
        return Err(BuildError::Unsupported("form settings type"));
    }
    if source.text().is_some_and(|text| {
        !text
            .bytes()
            .all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
    }) {
        return Err(BuildError::Invalid("form settings text"));
    }
    let document = encode_tree(source, assets, lookup, cx, Frame::FormSettings)?;
    let value = cx.list([cx.string("#")?, cx.atom(SHEET_TYPE)?, document])?;
    cx.check()?;
    Ok(value.managed()?)
}

struct Admission {
    nodes: usize,
    bytes: usize,
    text_work: usize,
}
impl Admission {
    fn bytes(&mut self, text: &str, cx: Cx<'_>) -> R<()> {
        self.bytes = self
            .bytes
            .checked_add(text.len())
            .ok_or(BuildError::Invalid("XML byte limit"))?;
        if self.bytes > 64 << 20 {
            return Err(BuildError::Invalid("XML byte limit"));
        }
        for _ in text.as_bytes().chunks(4096) {
            cx.check()?;
        }
        Ok(())
    }
    fn declarations(&mut self, node: NodeRef<'_, '_>, cx: Cx<'_>) -> R<usize> {
        let mut count = 0;
        for (prefix, uri) in node.ns_decls() {
            cx.check()?;
            count += 1;
            if count > 64 {
                return Err(BuildError::Invalid("XML attribute limit"));
            }
            if let Some(prefix) = prefix {
                self.bytes(prefix, cx)?;
            }
            self.bytes(uri, cx)?;
        }
        Ok(count)
    }
    fn node(&mut self, node: NodeRef<'_, '_>, depth: usize, cx: Cx<'_>) -> R<()> {
        cx.check()?;
        if depth >= 64 {
            return Err(BuildError::Invalid("XML depth limit"));
        }
        self.nodes += 1;
        if self.nodes > 1_000_000 {
            return Err(BuildError::Invalid("XML node limit"));
        }
        self.bytes(node.local(), cx)?;
        if let Some(ns) = node.ns() {
            self.bytes(ns, cx)?;
        }
        let declarations = self.declarations(node, cx)?;
        for (index, attr) in node.attrs().enumerate() {
            cx.check()?;
            if index >= 64 - declarations {
                return Err(BuildError::Invalid("XML attribute limit"));
            }
            self.bytes(attr.local(), cx)?;
            if let Some(ns) = attr.ns() {
                self.bytes(ns, cx)?;
            }
            self.bytes(attr.value(), cx)?;
            for previous in node.attrs().take(index) {
                cx.check()?;
                if (attr.ns(), attr.local()) == (previous.ns(), previous.local()) {
                    return Err(BuildError::Invalid("duplicate expanded attribute"));
                }
            }
        }
        if let Some(text) = node.text() {
            self.bytes(text, cx)?;
            self.text_work = text
                .len()
                .checked_mul(depth + 1)
                .and_then(|work| self.text_work.checked_add(work))
                .ok_or(BuildError::Invalid("XML text work limit"))?;
            if self.text_work > 256 << 20 {
                return Err(BuildError::Invalid("XML text work limit"));
            }
        }
        for child in node.children() {
            self.node(child, depth + 1, cx)?;
        }
        Ok(())
    }
}
fn admit(source: NodeRef<'_, '_>, cx: Cx<'_>) -> R<()> {
    let mut admission = Admission {
        nodes: 0,
        bytes: 0,
        text_work: 0,
    };
    let mut ancestor = source.parent();
    let mut depth = 0;
    while let Some(node) = ancestor {
        cx.check()?;
        if depth >= 64 {
            return Err(BuildError::Invalid("XML namespace depth limit"));
        }
        admission.declarations(node, cx)?;
        ancestor = node.parent();
        depth += 1;
    }
    admission.node(source, 0, cx)
}
