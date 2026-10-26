use crate::{SchemaError, StaticValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FixedNamespace {
    Xs,
    V8,
    Cfg,
    Dcsset,
}
impl FixedNamespace {
    pub fn uri(self) -> &'static str {
        match self {
            Self::Xs => "http://www.w3.org/2001/XMLSchema",
            Self::V8 => "http://v8.1c.ru/8.1/data/core",
            Self::Cfg => "http://v8.1c.ru/8.1/data/enterprise/current-config",
            Self::Dcsset => "http://v8.1c.ru/8.1/data-composition-system/settings",
        }
    }
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Xs => "xs",
            Self::V8 => "v8",
            Self::Cfg => "cfg",
            Self::Dcsset => "dcsset",
        }
    }
    pub fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "xs" => Some(Self::Xs),
            "v8" => Some(Self::V8),
            "cfg" => Some(Self::Cfg),
            "dcsset" => Some(Self::Dcsset),
            _ => None,
        }
    }
    pub fn from_uri(uri: &str) -> Option<Self> {
        [Self::Xs, Self::V8, Self::Cfg, Self::Dcsset]
            .into_iter()
            .find(|ns| ns.uri() == uri)
    }
}

pub fn is_ncname(name: &str) -> bool {
    fn start(c: char) -> bool {
        matches!(c, 'A'..='Z' | '_' | 'a'..='z' | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{2ff}' | '\u{370}'..='\u{37d}' | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
    }
    let mut chars = name.chars();
    chars.next().is_some_and(start) && chars.all(|c| start(c) || matches!(c, '-' | '.' | '0'..='9' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}'))
}
pub fn valid_type_namespace(uri: &str) -> bool {
    !uri.is_empty()
        && !uri.chars().any(|c| c.is_whitespace() || c.is_control())
        && uri != "http://www.w3.org/2000/xmlns/"
        && uri != "http://www.w3.org/XML/1998/namespace"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExpandedTypeName<'a> {
    pub namespace_uri: &'a str,
    pub local: &'a str,
}
impl<'a> ExpandedTypeName<'a> {
    pub fn new(namespace_uri: &'a str, local: &'a str) -> Option<Self> {
        (valid_type_namespace(namespace_uri) && is_ncname(local)).then_some(Self {
            namespace_uri,
            local,
        })
    }
    pub fn from_qname(qname: &'a str) -> Option<Self> {
        let (prefix, local) = qname.split_once(':')?;
        Self::new(FixedNamespace::from_prefix(prefix)?.uri(), local)
    }
    pub fn legacy_qname(self) -> Option<String> {
        Some(format!(
            "{}:{}",
            FixedNamespace::from_uri(self.namespace_uri)?.prefix(),
            self.local
        ))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorrowedPlatformType<'a> {
    pub name: ExpandedTypeName<'a>,
    pub set: bool,
}

fn canonical_guid(guid: &str) -> bool {
    guid.len() == 36
        && guid.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn reserved_namespace(prefix: &str) -> Option<&'static str> {
    if let Some(ns) = FixedNamespace::from_prefix(prefix) {
        return Some(ns.uri());
    }
    Some(match prefix {
        "app" => "http://v8.1c.ru/8.2/managed-application/core",
        "cmi" => "http://v8.1c.ru/8.2/managed-application/cmi",
        "ent" => "http://v8.1c.ru/8.1/data/enterprise",
        "lf" => "http://v8.1c.ru/8.2/managed-application/logform",
        "pal" => "http://v8.1c.ru/8.1/data/ui/colors/palette",
        "style" => "http://v8.1c.ru/8.1/data/ui/style",
        "sys" => "http://v8.1c.ru/8.1/data/ui/fonts/system",
        "v8ui" => "http://v8.1c.ru/8.1/data/ui",
        "web" => "http://v8.1c.ru/8.1/data/ui/colors/web",
        "win" => "http://v8.1c.ru/8.1/data/ui/colors/windows",
        "xen" => "http://v8.1c.ru/8.3/xcf/enums",
        "xpr" => "http://v8.1c.ru/8.3/xcf/predef",
        "xr" => "http://v8.1c.ru/8.3/xcf/readable",
        "xsi" => "http://www.w3.org/2001/XMLSchema-instance",
        _ => return None,
    })
}
pub fn platform_type_row<'a>(
    guid: &str,
    kind: &str,
    qname: &'a str,
    uri: Option<&'a str>,
) -> Option<BorrowedPlatformType<'a>> {
    if !canonical_guid(guid) || !matches!(kind, "Type" | "TypeSet") {
        return None;
    }
    let (prefix, local) = qname.split_once(':')?;
    if (prefix != "@" && !is_ncname(prefix))
        || !is_ncname(local)
        || matches!(prefix, "xml" | "xmlns")
    {
        return None;
    }
    if let (Some(uri), Some(expected)) = (uri, reserved_namespace(prefix))
        && uri != expected
    {
        return None;
    }
    let namespace_uri = match (uri, FixedNamespace::from_prefix(prefix)) {
        (Some(uri), Some(fixed)) if uri == fixed.uri() => uri,
        (Some(_), Some(_)) => return None,
        (Some(uri), None) => uri,
        (None, Some(fixed)) => fixed.uri(),
        (None, None) => return None,
    };
    Some(BorrowedPlatformType {
        name: ExpandedTypeName::new(namespace_uri, local)?,
        set: kind == "TypeSet",
    })
}

pub fn static_platform_types(
    table: &StaticValue,
) -> Option<impl Iterator<Item = (&'static str, BorrowedPlatformType<'static>)>> {
    let rows = table.as_object()?;
    for (i, (guid, value)) in rows.iter().enumerate() {
        if rows.get(..i)?.iter().any(|(previous, _)| previous == guid) {
            return None;
        }
        if !guid.starts_with('_') {
            static_platform_type_row(guid, value)?;
        }
    }
    Some(rows.iter().filter_map(|(guid, value)| {
        (!guid.starts_with('_'))
            .then(|| static_platform_type_row(guid, value).map(|ty| (*guid, ty)))
            .flatten()
    }))
}
pub fn static_platform_type_row(
    guid: &str,
    value: &StaticValue,
) -> Option<BorrowedPlatformType<'static>> {
    let row = value.as_array()?;
    if !matches!(row.len(), 2 | 3) {
        return None;
    }
    let uri = match row.get(2) {
        None | Some(StaticValue::Null) => None,
        Some(v) => Some(v.as_str()?),
    };
    platform_type_row(guid, row.first()?.as_str()?, row.get(1)?.as_str()?, uri)
}

pub(crate) fn parse_platform_types(
    raw: &str,
) -> Result<std::collections::HashMap<String, crate::PlatformType>, SchemaError> {
    let rows: crate::spec::Ordered<serde_json::Value> =
        serde_json::from_str(raw).map_err(|e| SchemaError(format!("types.json: {e}")))?;
    let mut seen = std::collections::HashSet::new();
    let mut out = std::collections::HashMap::new();
    for (guid, value) in rows.0 {
        if !seen.insert(guid.clone()) {
            return Err(SchemaError("types.json: duplicate key".into()));
        }
        if guid.starts_with('_') {
            continue;
        }
        let bad = || SchemaError(format!("types.json: bad entry {guid}"));
        let row = value.as_array().ok_or_else(bad)?;
        if !matches!(row.len(), 2 | 3) {
            return Err(bad());
        }
        let uri = match row.get(2) {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(v.as_str().ok_or_else(bad)?),
        };
        let ty = platform_type_row(
            &guid,
            row.first().and_then(|v| v.as_str()).ok_or_else(bad)?,
            row.get(1).and_then(|v| v.as_str()).ok_or_else(bad)?,
            uri,
        )
        .ok_or_else(bad)?;
        out.insert(
            guid,
            crate::PlatformType {
                set: ty.set,
                namespace_uri: ty.name.namespace_uri.into(),
                local: ty.name.local.into(),
            },
        );
    }
    Ok(out)
}

pub(crate) fn unique_type_guid<'a>(
    rows: impl IntoIterator<Item = (&'a str, BorrowedPlatformType<'a>)>,
    name: ExpandedTypeName<'_>,
    set: bool,
) -> Option<&'a str> {
    let mut found = None;
    for (guid, ty) in rows {
        if ty.name == name && ty.set == set {
            if found.is_some() {
                return None;
            }
            found = Some(guid);
        }
    }
    found
}
