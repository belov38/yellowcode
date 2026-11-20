use super::{Template, TemplateType};
use brace::Node;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("template build cancelled")]
    Cancelled,
    #[error("template: {0}")]
    Invalid(String),
}
pub trait InverseLookup {
    fn object_id(&self, name: &str) -> Option<brace::Guid>;
}
fn invalid(s: impl Into<String>) -> BuildError {
    BuildError::Invalid(s.into())
}
fn check(cancel: &AtomicBool) -> Result<(), BuildError> {
    if cancel.load(Ordering::Relaxed) {
        Err(BuildError::Cancelled)
    } else {
        Ok(())
    }
}
fn safe(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
        && !path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.chars().any(char::is_control))
}
fn output(raw: &[u8], memory: &MemoryBudget) -> Result<Charged<Vec<u8>>, BuildError> {
    let mut out = BudgetedVec::try_with_capacity(memory, Category::Metadata, raw.len())?;
    out.try_extend_from_slice(raw)?;
    Ok(out.into_charged_vec())
}
pub fn build(
    template: &Template,
    lookup: &dyn InverseLookup,
    memory: &MemoryBudget,
    cancel: &AtomicBool,
) -> Result<Charged<Vec<u8>>, BuildError> {
    check(cancel)?;
    let kind = template
        .kind
        .ok_or_else(|| invalid("missing TemplateType"))?;
    let size = template.files.iter().try_fold(4096u64, |n, (name, b)| {
        n.checked_add(name.len() as u64)
            .and_then(|n| n.checked_add(b.len() as u64))
            .ok_or_else(|| invalid("size overflow"))
    })?;
    let _work = memory
        .try_reserve(
            Category::Metadata,
            size.checked_mul(24)
                .ok_or_else(|| invalid("size overflow"))?,
        )
        .map_err(BudgetError::from)?;
    let mut files = BTreeMap::new();
    for (path, bytes) in &template.files {
        if !safe(path) || files.insert(path.as_str(), bytes.as_slice()).is_some() {
            return Err(invalid(format!("unsafe or duplicate template path {path}")));
        }
    }
    let need = |name: &str| {
        files
            .get(name)
            .copied()
            .ok_or_else(|| invalid(format!("missing {name}")))
    };
    let raw = match kind {
        TemplateType::TextDocument | TemplateType::DataCompositionAppearanceTemplate => {
            let name = if kind == TemplateType::TextDocument {
                "Template.txt"
            } else {
                "Template.xml"
            };
            if files.len() != 1 {
                return Err(invalid("unexpected verbatim template assets"));
            }
            let raw = need(name)?;
            if kind == TemplateType::DataCompositionAppearanceTemplate {
                mdconf_core::xml_in::parse_part(
                    raw,
                    Path::new(name),
                    &Default::default(),
                    memory,
                    &mut 0,
                )
                .map_err(|e| invalid(e.to_string()))?;
            }
            return output(raw, memory);
        }
        TemplateType::BinaryData | TemplateType::AddIn => {
            if files.len() != 1 {
                return Err(invalid("unexpected binary template assets"));
            }
            use base64::Engine;
            let node = Node::list(vec![
                Node::int(1),
                Node::list(vec![Node::atom(format!(
                    "#base64:{}",
                    base64::engine::general_purpose::STANDARD.encode(need("Template.bin")?)
                ))]),
            ]);
            brace::write_doc(&brace::Doc {
                root: node,
                bom: true,
                lead: String::new(),
                trailer: String::new(),
            })
        }
        TemplateType::HTMLDocument => {
            let xml = need("Template.xml")?;
            let arena = mdconf_core::xml_in::parse_part(
                xml,
                Path::new("Template.xml"),
                &Default::default(),
                memory,
                &mut 0,
            )
            .map_err(|e| invalid(e.to_string()))?;
            let root = arena
                .root()
                .filter(|n| {
                    n.local() == "Help" && n.ns() == Some("http://v8.1c.ru/8.3/xcf/extrnprops")
                })
                .ok_or_else(|| invalid("expected Help template page list"))?;
            let mut help = md_model::parts::Help::default();
            let mut used = HashSet::from(["Template.xml".to_string()]);
            for (path, data) in &files {
                if let Some(name) = path.strip_prefix("Template/_files/") {
                    used.insert((*path).into());
                    help.files.push((name.into(), data.to_vec()));
                }
            }
            for page in root.children() {
                check(cancel)?;
                let language = page.text().unwrap_or("");
                if page.local() != "Page"
                    || page.children().next().is_some()
                    || !safe(language)
                    || language.contains('/')
                {
                    return Err(invalid("invalid HTML page language"));
                }
                let path = format!("Template/{language}.html");
                if !used.insert(path.clone()) {
                    return Err(invalid("duplicate HTML page"));
                }
                help.pages.push((
                    language.into(),
                    native_html_with_files(need(&path)?, lookup, &help.files)?,
                ));
            }
            if files.keys().any(|path| !used.contains(*path)) {
                return Err(invalid("unconsumed HTML asset"));
            }
            let node = mdconf_core::external::build::help_node(&help)
                .map_err(|e| invalid(e.to_string()))?;
            brace::write_doc(&brace::Doc {
                root: node,
                bom: true,
                lead: String::new(),
                trailer: String::new(),
            })
        }
        _ => {
            return Err(invalid(format!(
                "{kind:?} requires its structural inverse codec"
            )));
        }
    };
    check(cancel)?;
    output(&raw, memory)
}
pub fn native_html(bytes: &[u8], lookup: &dyn InverseLookup) -> Result<Vec<u8>, BuildError> {
    native_html_with_files(bytes, lookup, &[])
}
pub fn native_html_with_files(
    bytes: &[u8],
    lookup: &dyn InverseLookup,
    files: &[(String, Vec<u8>)],
) -> Result<Vec<u8>, BuildError> {
    std::str::from_utf8(bytes).map_err(|_| invalid("HTML is not UTF-8"))?;
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let rest = html_bytes(bytes, at, bytes.len())?;
        if rest.starts_with(b"<!--") {
            let end = html_bytes(bytes, at + 4, bytes.len())?
                .windows(3)
                .position(|p| p == b"-->")
                .map_or(bytes.len(), |n| at + 4 + n + 3);
            out.extend_from_slice(html_bytes(bytes, at, end)?);
            at = end;
            continue;
        }
        if *bytes
            .get(at)
            .ok_or_else(|| invalid("HTML tokenizer position"))?
            != b'<'
        {
            out.push(
                *bytes
                    .get(at)
                    .ok_or_else(|| invalid("HTML tokenizer position"))?,
            );
            at += 1;
            continue;
        }
        let name_start = at + 1;
        let opaque = rest.get(1).is_some_and(|byte| matches!(byte, b'!' | b'?'))
            || rest.get(1) == Some(&b'/') && rest.get(2).is_some_and(u8::is_ascii_alphabetic);
        if opaque {
            let Some(end) = html_tag_end(bytes, name_start) else {
                out.extend_from_slice(rest);
                break;
            };
            out.extend_from_slice(html_bytes(bytes, at, end)?);
            at = end;
            continue;
        }
        if !bytes.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            out.push(b'<');
            at = name_start;
            continue;
        }
        let name_end = name_start
            + html_bytes(bytes, name_start, bytes.len())?
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b':' | b'_'))
                .count();
        let name = html_bytes(bytes, name_start, name_end)?;
        let Some(end) = html_tag_end(bytes, name_end) else {
            out.extend_from_slice(html_bytes(bytes, at, bytes.len())?);
            break;
        };
        if [b"script".as_slice(), b"style", b"textarea", b"title"]
            .iter()
            .any(|n| name.eq_ignore_ascii_case(n))
        {
            let close = html_raw_end(bytes, end, name).unwrap_or(bytes.len());
            out.extend_from_slice(html_bytes(bytes, at, close)?);
            at = close;
            continue;
        }
        let attribute = if name.eq_ignore_ascii_case(b"a") {
            Some(b"href".as_slice())
        } else if name.eq_ignore_ascii_case(b"img") {
            Some(b"src".as_slice())
        } else {
            None
        };
        if let Some((start, finish)) =
            attribute.and_then(|a| html_attribute(bytes, name_end, end, a))
        {
            let value = std::str::from_utf8(html_bytes(bytes, start, finish)?)
                .map_err(|_| invalid("HTML attribute is not UTF-8"))?;
            out.extend_from_slice(html_bytes(bytes, at, start)?);
            match native_link(value, lookup, files)? {
                Some(value) => out.extend_from_slice(value.as_bytes()),
                None => out.extend_from_slice(html_bytes(bytes, start, finish)?),
            }
            out.extend_from_slice(html_bytes(bytes, finish, end)?);
        } else {
            out.extend_from_slice(html_bytes(bytes, at, end)?);
        }
        at = end;
    }
    let text = String::from_utf8(out).map_err(|_| invalid("HTML is not UTF-8"))?;
    Ok(text
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .into_bytes())
}
fn html_bytes(bytes: &[u8], start: usize, end: usize) -> Result<&[u8], BuildError> {
    bytes
        .get(start..end)
        .ok_or_else(|| invalid("HTML tokenizer range"))
}
fn html_tag_end(bytes: &[u8], mut at: usize) -> Option<usize> {
    let mut quote = None;
    while let Some(&byte) = bytes.get(at) {
        if quote == Some(byte) {
            quote = None;
        } else if quote.is_none() {
            if matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
            } else if byte == b'>' {
                return Some(at + 1);
            }
        }
        at += 1;
    }
    None
}
fn html_raw_end(bytes: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let width = name.len().checked_add(2)?;
    let relative = bytes
        .get(from..)?
        .windows(width)
        .enumerate()
        .find_map(|(at, part)| {
            (part.starts_with(b"</")
                && part.get(2..)?.eq_ignore_ascii_case(name)
                && bytes
                    .get(from + at + width)
                    .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>')))
            .then_some(at)
        })?;
    html_tag_end(bytes, from + relative + width)
}
fn html_attribute(
    bytes: &[u8],
    mut at: usize,
    end: usize,
    wanted: &[u8],
) -> Option<(usize, usize)> {
    let bytes = bytes.get(..end)?;
    while at < end {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if bytes.get(at).is_none_or(|b| matches!(b, b'>' | b'/')) {
            break;
        }
        let name = at;
        while bytes
            .get(at)
            .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'=' | b'>' | b'/'))
        {
            at += 1;
        }
        let matching = bytes.get(name..at)?.eq_ignore_ascii_case(wanted);
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if bytes.get(at) != Some(&b'=') {
            continue;
        }
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let quote = bytes.get(at).copied().filter(|b| matches!(b, b'\'' | b'"'));
        if quote.is_some() {
            at += 1;
        }
        let start = at;
        while bytes.get(at).is_some_and(|b| match quote {
            Some(quote) => *b != quote,
            None => !b.is_ascii_whitespace() && *b != b'>',
        }) {
            at += 1;
        }
        if matching {
            return Some((start, at));
        }
        if quote.is_some() {
            at += 1;
        }
    }
    None
}

fn native_link(
    value: &str,
    lookup: &dyn InverseLookup,
    files: &[(String, Vec<u8>)],
) -> Result<Option<String>, BuildError> {
    if let Some(name) = value.strip_prefix("_files/") {
        use unicode_normalization::UnicodeNormalization;
        let path = name.split(['#', '?']).next().unwrap_or_default();
        if !safe(path) || !files.iter().any(|(file, _)| file.nfc().eq(path.nfc())) {
            return Err(invalid(format!("missing HTML attachment {path}")));
        }
        return Ok(Some(format!(
            "fbdc1947-c8ea-53b4-9973-7e8c427f9a8c_files/{name}"
        )));
    }
    if let Some((name, suffix)) = value.split_once('/')
        && name.contains('.')
        && let Some(rest) = suffix
            .strip_prefix("Help")
            .or_else(|| suffix.strip_prefix("Template"))
        && (rest.is_empty() || rest.starts_with(['#', '?']))
    {
        let id = lookup
            .object_id(name)
            .ok_or_else(|| invalid(format!("unresolved HTML reference {name}")))?;
        return Ok(Some(format!(
            "../id{id}/00000000-0000-0000-0000-000000000000{rest}"
        )));
    }
    if let Some(name) = value.strip_prefix("StdPicture.") {
        for table in ["templates/tables.json", "ui.json", "std_pictures.json"] {
            for (key, v) in mdschema::static_schema()
                .table(table)
                .and_then(|t| t.get("std_pictures"))
                .and_then(|v| v.as_object())
                .unwrap_or_default()
            {
                if v.as_str() == Some(name) {
                    return Ok(Some(if key.starts_with('-') {
                        format!("../../mdpicture/idn{key}")
                    } else {
                        format!("../../mdpicture/id{key}/00000000-0000-0000-0000-000000000000")
                    }));
                }
            }
        }
        return Err(invalid(format!("unresolved HTML reference {value}")));
    }
    if value.starts_with("CommonPicture.") {
        let id = lookup
            .object_id(value)
            .ok_or_else(|| invalid(format!("unresolved HTML reference {value}")))?;
        return Ok(Some(format!(
            "../../mdpicture/id{id}/00000000-0000-0000-0000-000000000000"
        )));
    }
    Ok(None)
}
