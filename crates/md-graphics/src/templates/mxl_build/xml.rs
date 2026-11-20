use super::{BuildError, R, nodes::Cx};
use mdconf_core::xml::view::NodeRef;
pub(super) type X<'a, 's> = NodeRef<'a, 's>;
pub(super) const NS: &str = "http://v8.1c.ru/8.2/data/spreadsheet";
pub(super) const V8: &str = "http://v8.1c.ru/8.1/data/core";
pub(super) const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
pub(super) const XS: &str = "http://www.w3.org/2001/XMLSchema";
pub(super) fn parse<'src>(
    xml: &'src [u8],
    cx: Cx<'_>,
    cancelled: &dyn Fn() -> bool,
) -> R<mdconf_core::xml::view::XmlArena<'src>> {
    admit_checked(xml, cancelled)?;
    mdconf_core::xml::read::parse_arena_checked(
        xml,
        crate::allocation::Allocation::Managed(cx.budget),
        cancelled,
    )
    .map_err(|e| match e {
        mdconf_core::xml::read::CheckedReadError::Cancelled => BuildError::Cancelled,
        mdconf_core::xml::read::CheckedReadError::Read(
            mdconf_core::xml::read::ReadError::Memory(e),
        ) => BuildError::Memory(e),
        _ => BuildError::Invalid("XML syntax"),
    })
}
fn check(cancelled: &dyn Fn() -> bool) -> R<()> {
    if cancelled() {
        Err(BuildError::Cancelled)
    } else {
        Ok(())
    }
}
fn find(bytes: &[u8], needle: &[u8], cancelled: &dyn Fn() -> bool) -> R<Option<usize>> {
    for (at, candidate) in bytes.windows(needle.len()).enumerate() {
        if at.is_multiple_of(4096) {
            check(cancelled)?;
        }
        if candidate == needle {
            return Ok(Some(at));
        }
    }
    Ok(None)
}
pub(super) fn admit_checked(xml: &[u8], cancelled: &dyn Fn() -> bool) -> R<()> {
    if xml.len() > 64 << 20 {
        return Err(BuildError::Invalid("XML byte limit"));
    }
    let mut at = 0usize;
    let mut depth = 0usize;
    let mut count = 0usize;
    let mut work = 0usize;
    let mut lengths = [0usize; 64];
    while at < xml.len() {
        check(cancelled)?;
        let tail = xml.get(at..).ok_or(BuildError::Invalid("XML position"))?;
        if !tail.starts_with(b"<") {
            let len = find(tail, b"<", cancelled)?.unwrap_or(tail.len());
            work = charge_text(len, depth, &mut lengths, work)?;
            at += len;
            continue;
        }
        if tail.starts_with(b"<!--") || tail.starts_with(b"<![CDATA[") || tail.starts_with(b"<?") {
            let (prefix, end, cdata) = if tail.starts_with(b"<!--") {
                (4, &b"-->"[..], false)
            } else if tail.starts_with(b"<?") {
                (2, &b"?>"[..], false)
            } else {
                (9, &b"]]>"[..], true)
            };
            let body = tail.get(prefix..).ok_or(BuildError::Invalid("XML token"))?;
            let len =
                find(body, end, cancelled)?.ok_or(BuildError::Invalid("XML token terminator"))?;
            if cdata {
                work = charge_text(len, depth, &mut lengths, work)?;
            }
            at += prefix + len + end.len();
            continue;
        }
        if tail.starts_with(b"<!") {
            return Err(BuildError::Unsupported("DTD"));
        }
        let closing = tail.starts_with(b"</");
        let mut quote = None;
        let mut end = None;
        let mut attrs = 0usize;
        let mut last = None;
        for (i, b) in tail.iter().copied().enumerate().skip(1) {
            if i % 4096 == 0 {
                check(cancelled)?;
            }
            if quote == Some(b) {
                quote = None;
            } else if quote.is_none() {
                match b {
                    b'\'' | b'"' => quote = Some(b),
                    b'=' => attrs += 1,
                    b'>' => {
                        end = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            if !b.is_ascii_whitespace() {
                last = Some(b);
            }
        }
        let end = end.ok_or(BuildError::Invalid("XML token terminator"))?;
        if attrs > 64 {
            return Err(BuildError::Invalid("XML attribute limit"));
        }
        if closing {
            depth = depth
                .checked_sub(1)
                .ok_or(BuildError::Invalid("XML nesting"))?;
        } else {
            count += 1;
            if count > 1_000_000 {
                return Err(BuildError::Invalid("XML node limit"));
            }
            if depth >= 64 {
                return Err(BuildError::Invalid("XML depth limit"));
            }
            let empty = last == Some(b'/');
            if !empty {
                *lengths
                    .get_mut(depth)
                    .ok_or(BuildError::Invalid("XML depth limit"))? = 0;
                depth += 1;
            }
        }
        at += end + 1;
    }
    Ok(())
}
fn charge_text(len: usize, depth: usize, lengths: &mut [usize], work: usize) -> R<usize> {
    let mut work = work;
    if let Some(total) = depth.checked_sub(1).and_then(|i| lengths.get_mut(i)) {
        *total = total
            .checked_add(len)
            .ok_or(BuildError::Invalid("XML text work limit"))?;
        work = work
            .checked_add(*total)
            .ok_or(BuildError::Invalid("XML text work limit"))?;
        if work > 256 << 20 {
            return Err(BuildError::Invalid("XML text work limit"));
        }
    }
    Ok(work)
}
pub(super) fn attr<'a>(e: X<'a, '_>, ns: Option<&str>, name: &str) -> Option<&'a str> {
    e.attrs()
        .find(|a| a.ns() == ns && a.local() == name)
        .map(|a| a.value())
}
pub(super) fn child<'a, 's>(e: X<'a, 's>, name: &str) -> Option<X<'a, 's>> {
    e.children().find(|c| c.local() == name)
}
pub(super) fn text<'a>(e: X<'a, '_>, name: &str, default: &'a str) -> R<&'a str> {
    child(e, name)
        .map(leaf)
        .transpose()
        .map(|v| v.unwrap_or(default))
}
pub(super) fn leaf<'a>(e: X<'a, '_>) -> R<&'a str> {
    if e.children().next().is_some() || e.attrs().next().is_some() {
        return Err(BuildError::Invalid("expected scalar element"));
    }
    Ok(e.text().unwrap_or_default())
}
pub(super) fn integer(s: &str) -> R<i64> {
    s.parse().map_err(|_| BuildError::Invalid("integer"))
}
pub(super) fn number(e: X<'_, '_>, name: &str, default: i64) -> R<i64> {
    child(e, name)
        .map(|e| integer(leaf(e)?))
        .transpose()
        .map(|v| v.unwrap_or(default))
}
pub(super) fn boolean(s: &str) -> R<bool> {
    match s {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(BuildError::Invalid("boolean")),
    }
}
pub(super) fn flag(e: X<'_, '_>, name: &str, default: bool) -> R<bool> {
    child(e, name)
        .map(|e| boolean(leaf(e)?))
        .transpose()
        .map(|v| v.unwrap_or(default))
}
pub(super) fn shape(
    e: X<'_, '_>,
    names: &[&str],
    repeated: &[&str],
    attrs: &[(Option<&str>, &str)],
) -> R<()> {
    for (index, attribute) in e.attrs().enumerate() {
        if e.attrs().take(index).any(|previous| {
            (previous.ns(), previous.local()) == (attribute.ns(), attribute.local())
        }) {
            return Err(BuildError::Invalid("duplicate expanded attribute"));
        }
    }
    if e.attrs().any(|a| !attrs.contains(&(a.ns(), a.local()))) {
        return Err(BuildError::Unsupported("element attribute"));
    }
    if !names.is_empty() && e.text().is_some_and(|t| !t.trim().is_empty()) {
        return Err(BuildError::Invalid("mixed element text"));
    }
    for c in e.children() {
        let namespace = if [
            "item",
            "lang",
            "content",
            "Type",
            "TypeSet",
            "StringQualifiers",
            "NumberQualifiers",
            "DateQualifiers",
            "BinaryDataQualifiers",
            "Length",
            "AllowedLength",
            "Digits",
            "FractionDigits",
            "AllowedSign",
            "DateFractions",
        ]
        .contains(&c.local())
        {
            V8
        } else {
            NS
        };
        if !names.contains(&c.local()) || c.ns() != Some(namespace) {
            return Err(BuildError::Unsupported("element or namespace"));
        }
        if !repeated.contains(&c.local())
            && e.children().filter(|n| n.local() == c.local()).count() != 1
        {
            return Err(BuildError::Invalid("duplicate element"));
        }
    }
    Ok(())
}
pub(super) fn qname<'a>(e: X<'a, '_>, value: &'a str) -> R<(&'a str, &'a str)> {
    let (prefix, local) = value
        .split_once(':')
        .map_or((None, value), |(p, l)| (Some(p), l));
    if !mdschema::is_ncname(local) || prefix.is_some_and(|p| !mdschema::is_ncname(p)) {
        return Err(BuildError::Invalid("QName"));
    }
    let mut at = Some(e);
    while let Some(e) = at {
        if let Some((_, uri)) = e.ns_decls().find(|(p, _)| *p == prefix) {
            return Ok((uri, local));
        }
        at = e.parent();
    }
    Err(BuildError::Invalid("unbound QName"))
}
