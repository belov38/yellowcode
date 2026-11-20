use super::{
    BuildError, InverseLookup, R,
    nodes::{Cx, N, Nodes},
    tables,
    xml::*,
};
const UI: &str = "http://v8.1c.ru/8.1/data/ui";
pub(super) fn lstr(e: Option<X<'_, '_>>, cx: Cx<'_>) -> R<N> {
    let mut out = Nodes::new(cx)?;
    out.push(cx.int(1)?)?;
    out.push(cx.int(e.map_or(0, |e| e.children().filter(|c| c.local() == "item").count()))?)?;
    if let Some(e) = e {
        shape(e, &["item"], &["item"], &[])?;
        for it in e.children() {
            shape(it, &["lang", "content"], &[], &[])?;
            out.push(cx.list([
                cx.string(text(it, "lang", "")?)?,
                crlf(text(it, "content", "")?, cx)?,
            ])?)?;
        }
    }
    out.finish(cx)
}
pub(super) fn crlf(s: &str, cx: Cx<'_>) -> R<N> {
    let a = crate::allocation::Allocation::Managed(cx.budget);
    let lf = a.replace_text(s, "\r\n", "\n")?;
    Ok(a.replace_text(&lf, "\n", "\r\n")?.map(brace::Node::Str))
}
pub(super) fn typed(e: X<'_, '_>, cx: Cx<'_>) -> R<N> {
    shape(e, &[], &[], &[(Some(XSI), "type"), (Some(XSI), "nil")])?;
    if let Some(nil) = attr(e, Some(XSI), "nil")
        && boolean(nil)?
    {
        if !e.text().unwrap_or_default().is_empty() {
            return Err(BuildError::Invalid("nil value content"));
        }
        return cx.list([cx.string("U")?]);
    }
    let (ns, kind) = qname(
        e,
        attr(e, Some(XSI), "type").ok_or(BuildError::Invalid("value type"))?,
    )?;
    if ns != XS {
        return Err(BuildError::Unsupported("typed reference or object value"));
    }
    let value = e.text().unwrap_or_default();
    let (tag, value) = match kind {
        "string" => ("S", crlf(value, cx)?),
        "decimal" => {
            decimal(value)?;
            ("N", cx.atom(value)?)
        }
        "boolean" => ("B", cx.int(i64::from(boolean(value)?))?),
        "dateTime" => {
            if value.len() != 19
                || !value.bytes().enumerate().all(|(i, b)| match i {
                    4 | 7 => b == b'-',
                    10 => b == b'T',
                    13 | 16 => b == b':',
                    _ => b.is_ascii_digit(),
                })
            {
                return Err(BuildError::Invalid("dateTime"));
            }
            let part = |a, b| {
                value
                    .get(a..b)
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or(BuildError::Invalid("dateTime"))
            };
            let year = part(0, 4)?;
            let month = part(5, 7)?;
            let day = part(8, 10)?;
            let days = match month {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 => {
                    if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                        29
                    } else {
                        28
                    }
                }
                _ => 0,
            };
            if year == 0
                || day == 0
                || day > days
                || part(11, 13)? > 23
                || part(14, 16)? > 59
                || part(17, 19)? > 59
            {
                return Err(BuildError::Invalid("dateTime"));
            }
            let mut bytes = memory_budget::BudgetedVec::try_with_capacity(
                cx.budget,
                memory_budget::Category::Metadata,
                14,
            )?;
            for b in value.bytes().filter(u8::is_ascii_digit) {
                bytes.try_push(b)?;
            }
            let compact = bytes
                .into_charged_vec()
                .map(String::from_utf8)
                .try_map(|v| v)
                .map_err(|_| BuildError::Invalid("dateTime"))?;
            ("D", cx.atom(&compact)?)
        }
        _ => return Err(BuildError::Unsupported("value type")),
    };
    cx.list([cx.string(tag)?, value])
}
fn decimal(value: &str) -> R<()> {
    let s = value
        .strip_prefix('-')
        .or_else(|| value.strip_prefix('+'))
        .unwrap_or(value);
    let mut parts = s.split('.');
    let whole = parts.next().unwrap_or_default();
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || parts
            .next()
            .is_some_and(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
        || parts.next().is_some()
    {
        return Err(BuildError::Invalid("decimal"));
    }
    Ok(())
}
pub(super) fn type_desc(e: X<'_, '_>, cx: Cx<'_>, lookup: &dyn InverseLookup) -> R<N> {
    shape(
        e,
        &[
            "Type",
            "TypeSet",
            "StringQualifiers",
            "NumberQualifiers",
            "DateQualifiers",
            "BinaryDataQualifiers",
        ],
        &["Type", "TypeSet"],
        &[],
    )?;
    for (qualifier, kind) in [
        ("StringQualifiers", "string"),
        ("NumberQualifiers", "decimal"),
        ("DateQualifiers", "dateTime"),
        ("BinaryDataQualifiers", "base64Binary"),
    ] {
        if child(e, qualifier).is_some() {
            let mut found = false;
            for t in e.children().filter(|c| c.local() == "Type") {
                if qname(t, leaf(t)?)? == (XS, kind) {
                    found = true;
                }
            }
            if !found {
                return Err(BuildError::Invalid("qualifiers without primitive type"));
            }
        }
    }
    let mut out = Nodes::new(cx)?;
    out.push(cx.string("Pattern")?)?;
    for t in e
        .children()
        .filter(|c| ["Type", "TypeSet"].contains(&c.local()))
    {
        cx.check()?;
        let (ns, name) = qname(t, leaf(t)?)?;
        if ns != XS {
            let mut found = None;
            if ns == "http://v8.1c.ru/8.1/data/enterprise/current-config" {
                found = lookup.type_id(name);
                cx.check()?;
            }
            if found.is_none() {
                let expanded = mdschema::ExpandedTypeName::new(ns, name)
                    .ok_or(BuildError::Invalid("type name"))?;
                let table = mdschema::static_template_tables();
                found = table
                    .type_guid_expanded(expanded, t.local() == "TypeSet")
                    .and_then(brace::Guid::parse);
            }
            out.push(cx.list([
                cx.string("#")?,
                cx.int(found.ok_or(BuildError::Invalid("unresolved type reference"))?)?,
            ])?)?;
            continue;
        }
        if t.local() == "TypeSet" {
            return Err(BuildError::Unsupported("primitive TypeSet"));
        }
        let mut rec = Nodes::new(cx)?;
        match name {
            "boolean" => rec.push(cx.string("B")?)?,
            "string" | "base64Binary" => {
                let tag = if name == "string" {
                    "StringQualifiers"
                } else {
                    "BinaryDataQualifiers"
                };
                rec.push(cx.string(if name == "string" { "S" } else { "R" })?)?;
                if let Some(q) = child(e, tag) {
                    shape(q, &["Length", "AllowedLength"], &[], &[])?;
                    let length = number(q, "Length", 0)?;
                    if u32::try_from(length).is_err() {
                        return Err(BuildError::Invalid("type length"));
                    }
                    let variable = match text(q, "AllowedLength", "Variable")? {
                        "Variable" => true,
                        "Fixed" => false,
                        _ => return Err(BuildError::Invalid("allowed length")),
                    };
                    if length != 0 || !variable {
                        rec.push(cx.int(length)?)?;
                        rec.push(cx.int(i64::from(variable))?)?;
                    }
                }
            }
            "decimal" => {
                rec.push(cx.string("N")?)?;
                if let Some(q) = child(e, "NumberQualifiers") {
                    shape(q, &["Digits", "FractionDigits", "AllowedSign"], &[], &[])?;
                    let digits = number(q, "Digits", 0)?;
                    let fraction = number(q, "FractionDigits", 0)?;
                    if u32::try_from(digits).is_err()
                        || u32::try_from(fraction).is_err()
                        || fraction > digits
                    {
                        return Err(BuildError::Invalid("number qualifiers"));
                    }
                    let nonnegative = match text(q, "AllowedSign", "Any")? {
                        "Any" => false,
                        "Nonnegative" => true,
                        _ => return Err(BuildError::Invalid("allowed sign")),
                    };
                    if digits != 0 || fraction != 0 || nonnegative {
                        rec.push(cx.int(digits)?)?;
                        rec.push(cx.int(fraction)?)?;
                        rec.push(cx.int(i64::from(nonnegative))?)?;
                    }
                }
            }
            "dateTime" => {
                rec.push(cx.string("D")?)?;
                if let Some(q) = child(e, "DateQualifiers") {
                    shape(q, &["DateFractions"], &[], &[])?;
                    match text(q, "DateFractions", "DateTime")? {
                        "DateTime" => {}
                        "Date" => rec.push(cx.string("D")?)?,
                        "Time" => rec.push(cx.string("T")?)?,
                        _ => return Err(BuildError::Invalid("date fractions")),
                    }
                }
            }
            _ => return Err(BuildError::Unsupported("primitive type")),
        }
        out.push(rec.finish(cx)?)?;
    }
    out.finish(cx)
}
pub(super) fn font(e: X<'_, '_>, cx: Cx<'_>, lookup: &dyn InverseLookup) -> R<N> {
    shape(
        e,
        &[],
        &[],
        &[
            "kind",
            "faceName",
            "height",
            "bold",
            "italic",
            "underline",
            "strikeout",
            "scale",
            "lineHeight",
            "ref",
        ]
        .map(|n| (None, n)),
    )?;
    let a = |key| attr(e, None, key);
    let num = |key, d| a(key).map(integer).transpose().map(|v| v.unwrap_or(d));
    let flag = |key| a(key).map(boolean).transpose().map(|v| v.unwrap_or(false));
    let h = |v: &str| -> R<i64> {
        decimal(v)?;
        let mut p = v.split('.');
        let whole = integer(p.next().unwrap_or_default())?;
        let frac = p.next().unwrap_or("0");
        if frac.len() > 1 {
            return Err(BuildError::Unsupported("font height precision"));
        }
        whole
            .checked_mul(10)
            .and_then(|n| {
                n.checked_add(if v.starts_with('-') {
                    -integer(frac).ok()?
                } else {
                    integer(frac).ok()?
                })
            })
            .ok_or(BuildError::Invalid("font height"))
    };
    let kind = a("kind").ok_or(BuildError::Invalid("font kind"))?;
    if e.text().is_some_and(|s| !s.trim().is_empty()) || kind == "Absolute" && a("ref").is_some() {
        return Err(BuildError::Invalid("font content"));
    }
    if kind == "AutoFont" {
        if e.attrs().count() != 1 {
            return Err(BuildError::Unsupported("AutoFont overrides"));
        }
        return cx.ints([8, 3, 0, 1, 100]);
    }
    let mut out = Nodes::new(cx)?;
    out.push(cx.int(8)?)?;
    if kind == "Absolute" {
        out.push(cx.int(0)?)?;
        out.push(cx.int(575 | if a("lineHeight").is_some() { 256 } else { 0 })?)?;
        for value in [
            h(a("height").ok_or(BuildError::Invalid("font height"))?)?,
            0,
            0,
            0,
            if flag("bold")? { 700 } else { 400 },
            i64::from(flag("italic")?),
            i64::from(flag("underline")?),
            i64::from(flag("strikeout")?),
            0,
            0,
            0,
            0,
            0,
        ] {
            out.push(cx.int(value)?)?;
        }
        out.push(cx.string(a("faceName").ok_or(BuildError::Invalid("font face"))?)?)?;
        for v in [1, num("scale", 100)?, num("lineHeight", 0)?] {
            out.push(cx.int(v)?)?;
        }
    } else {
        let rf = a("ref").ok_or(BuildError::Invalid("font reference"))?;
        let reference = match kind {
            "StyleItem" => {
                out.push(cx.int(2)?)?;
                tables::style_ref(cx, lookup, "style_fonts", e, rf)?
            }
            "WindowsFont" => {
                out.push(cx.int(1)?)?;
                let (ns, name) = qname(e, rf)?;
                if ns != "http://v8.1c.ru/8.1/data/ui/fonts/system" {
                    return Err(BuildError::Invalid("font namespace"));
                }
                cx.list([cx.atom(
                    tables::inverse("sys_fonts", name).ok_or(BuildError::Invalid("system font"))?,
                )?])?
            }
            _ => return Err(BuildError::Unsupported("font kind")),
        };
        let mut mask = 0u32;
        for (bit, key) in [
            (1, "height"),
            (2, "bold"),
            (3, "italic"),
            (4, "underline"),
            (5, "strikeout"),
            (0, "faceName"),
            (9, "scale"),
            (8, "lineHeight"),
        ] {
            if a(key).is_some() {
                mask |= 1 << bit;
            }
        }
        out.push(cx.int(mask)?)?;
        out.push(reference)?;
        for key in [
            "height",
            "bold",
            "italic",
            "underline",
            "strikeout",
            "faceName",
        ] {
            if let Some(value) = a(key) {
                out.push(match key {
                    "height" => cx.int(h(value)?)?,
                    "bold" => cx.int(if boolean(value)? { 700 } else { 400 })?,
                    "faceName" => cx.string(value)?,
                    _ => cx.int(i64::from(boolean(value)?))?,
                })?;
            }
        }
        out.push(cx.int(1)?)?;
        out.push(cx.int(num("scale", 100)?)?)?;
        if a("lineHeight").is_some() {
            out.push(cx.int(num("lineHeight", 0)?)?)?;
        }
    }
    out.finish(cx)
}
pub(super) fn line(e: X<'_, '_>, cx: Cx<'_>) -> R<N> {
    if e.children()
        .any(|c| c.local() != "style" || c.ns() != Some(UI))
        || e.children().count() > 1
        || e.attrs()
            .any(|a| a.ns().is_some() || !["width", "gap"].contains(&a.local()))
    {
        return Err(BuildError::Unsupported("line shape"));
    }
    let (guid, style) = if let Some(style) = child(e, "style") {
        let (ns, name) = qname(
            style,
            attr(style, Some(XSI), "type").ok_or(BuildError::Invalid("line type"))?,
        )?;
        if ns != UI || style.attrs().count() != 1 || style.children().next().is_some() {
            return Err(BuildError::Invalid("line type"));
        }
        let (guid, names): (&str, &[&str]) = match name {
            "SpreadsheetDocumentCellLineType" => (
                "f527dc88-1d39-40b3-bcbb-d98b690ead68",
                &[
                    "None",
                    "Solid",
                    "Dotted",
                    "Double",
                    "ThinDashed",
                    "ThickDashed",
                    "LargeDashed",
                ],
            ),
            "SpreadsheetDocumentDrawingLineType" => (
                "b7438842-27cc-42a3-846f-2250cd9c1bc3",
                &[
                    "None",
                    "Solid",
                    "Dashed",
                    "Dotted",
                    "DashDotted",
                    "DashDottedDotted",
                ],
            ),
            _ => return Err(BuildError::Unsupported("line type")),
        };
        (
            guid,
            names
                .iter()
                .position(|n| Some(*n) == style.text())
                .ok_or(BuildError::Invalid("line style"))?,
        )
    } else {
        (super::ZERO, 0)
    };
    cx.list([
        cx.int(4)?,
        cx.int(0)?,
        cx.ints([0])?,
        cx.int(style)?,
        cx.int(integer(attr(e, None, "width").unwrap_or("0"))?)?,
        cx.int(0)?,
        cx.atom(guid)?,
        cx.int(i64::from(boolean(attr(e, None, "gap").unwrap_or("false"))?))?,
    ])
}

pub(super) fn parameter(text: &str, cx: Cx<'_>) -> R<N> {
    let allocation = crate::allocation::Allocation::Managed(cx.budget);
    Ok(allocation
        .replace_text(text, "\n", "\r\n")?
        .map(brace::Node::Str))
}
