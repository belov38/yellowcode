use super::super::xdto::scope::{Prefix, QName, Scope, Scoped};
use super::tree::Value;
use super::*;
use crate::graph::scalar::Text;
use base64::Engine as _;
use std::fmt::Write as _;

const HEAD: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>";
const SETTINGS_ROOT: [(Option<&str>, &str); 11] = [
    (None, SETTINGS),
    (Some("dcscor"), CORE),
    (Some("pal"), "http://v8.1c.ru/8.1/data/ui/colors/palette"),
    (Some("style"), STYLE),
    (Some("sys"), "http://v8.1c.ru/8.1/data/ui/fonts/system"),
    (Some("v8"), V8),
    (Some("v8ui"), V8UI),
    (Some("web"), "http://v8.1c.ru/8.1/data/ui/colors/web"),
    (Some("win"), "http://v8.1c.ru/8.1/data/ui/colors/windows"),
    (Some("xs"), XS),
    (Some("xsi"), XSI),
];
fn scoped_error(error: crate::graph::read::ReadError) -> BuildError {
    match error {
        crate::graph::read::ReadError::Memory(error) => BuildError::Memory(error),
        crate::graph::read::ReadError::Semantic { code, .. } => BuildError::Invalid(code),
    }
}
struct Internal<'a> {
    cx: Context<'a>,
    bytes: Vector<u8>,
    embedded: bool,
}
#[derive(Clone, Copy)]
struct Preferences {
    font_ref: bool,
    embedded: bool,
}
impl<'a> Internal<'a> {
    fn new(cx: Context<'a>) -> Result<Self> {
        Ok(Self {
            cx,
            bytes: cx.allocation.vector(0)?,
            embedded: false,
        })
    }
    fn raw(&mut self, text: &str) -> Result<()> {
        self.bytes.extend(text.as_bytes())?;
        Ok(())
    }
    fn line(&mut self, depth: usize) -> Result<()> {
        self.cx.check()?;
        if !self.bytes.is_empty() {
            self.raw("\r\n")?;
        }
        for _ in 1..depth {
            self.bytes.push(b'\t')?;
        }
        Ok(())
    }
    fn escape(&mut self, text: &str, attribute: bool) -> Result<()> {
        let mut from = 0;
        for (at, byte) in text.bytes().enumerate() {
            if at % 4096 == 0 {
                self.cx.check()?;
            }
            let replacement = match byte {
                b'&' => "&amp;",
                b'<' => "&lt;",
                b'>' => "&gt;",
                b'\r' if self.embedded => "&#xD;",
                b'\r' => "&#13;",
                b'"' if attribute => "&quot;",
                b'\n' if attribute && self.embedded => "&#xA;",
                b'\n' if attribute => "&#10;",
                b'\t' if attribute && self.embedded => "&#x9;",
                b'\t' if attribute => "&#9;",
                _ => continue,
            };
            self.raw(text.get(from..at).ok_or(BudgetError::Overflow)?)?;
            self.raw(replacement)?;
            from = at + 1;
        }
        self.raw(text.get(from..).ok_or(BudgetError::Overflow)?)
    }
    fn name(&mut self, name: QName<'_>) -> Result<()> {
        if let Some(prefix) = name.prefix {
            prefix.with_text(|p| self.raw(p))?;
            self.raw(":")?;
        }
        self.raw(name.local)
    }
    fn qname<'n>(
        scope: &mut Scope<'n, '_>,
        ns: &'n str,
        local: &'n str,
        depth: usize,
        ordinal: &mut usize,
        attribute: bool,
        preferences: Preferences,
    ) -> Result<QName<'n>> {
        if ns == XML_NS {
            return Ok(QName {
                prefix: Some(Prefix::Borrowed("xml")),
                local,
            });
        }
        if ns.is_empty() || (!attribute && scope.uri(None) == Some(ns)) {
            return Ok(QName {
                prefix: None,
                local,
            });
        }
        let prefix = if let Some(prefix) = scope.prefix(ns) {
            prefix
        } else {
            let preferred = match ns {
                SETTINGS => Some("dcsset"),
                CORE => Some("dcscor"),
                AREA => Some("dcsat"),
                SCHEMA if preferences.embedded => Some("dcssch"),
                "http://v8.1c.ru/8.1/data-composition-system/common" => Some("dcscom"),
                "http://v8.1c.ru/8.2/data/spreadsheet" if !preferences.embedded => Some("mxl"),
                STYLE if preferences.font_ref => Some("style"),
                "http://v8.1c.ru/8.1/data/ui/fonts/system" if preferences.font_ref => Some("sys"),
                _ => None,
            };
            let prefix = if let Some(preferred) =
                preferred.filter(|p| scope.uri(Some(Prefix::Borrowed(p))).is_none())
            {
                Prefix::Borrowed(preferred)
            } else {
                *ordinal = ordinal.checked_add(1).ok_or(BudgetError::Overflow)?;
                Prefix::Generated(depth, *ordinal)
            };
            scope.declare(Some(prefix), ns).map_err(scoped_error)?;
            prefix
        };
        Ok(QName {
            prefix: Some(prefix),
            local,
        })
    }
    fn prepare<'n>(
        value: Value<'n>,
        scope: &mut Scope<'n, '_>,
        depth: usize,
        ordinal: &mut usize,
        preferences: Preferences,
    ) -> Result<Prepared<'n>> {
        Ok(match value {
            Value::QName(ns, local) => Prepared::QName(Self::qname(
                scope,
                ns,
                local,
                depth,
                ordinal,
                false,
                preferences,
            )?),
            other => Prepared::Other(other),
        })
    }
    fn value(&mut self, value: Prepared<'_>, attribute: bool) -> Result<()> {
        match value {
            Prepared::QName(name) => self.name(name),
            Prepared::Other(Value::Text(text)) => self.escape(text, attribute),
            Prepared::Other(Value::Guid(id) | Value::Style(id)) => {
                if matches!(value, Prepared::Other(Value::Style(_))) {
                    self.raw("0:")?;
                }
                let mut text = Text::<64>::new();
                write!(text, "{id}").map_err(|_| BudgetError::Overflow)?;
                self.raw(text.as_str())
            }
            Prepared::Other(Value::Index(index)) => {
                let mut text = Text::<64>::new();
                write!(text, "{index}").map_err(|_| BudgetError::Overflow)?;
                self.raw(text.as_str())
            }
            Prepared::Other(Value::Embedded(bytes)) => {
                for (ordinal, chunk) in bytes.chunks(48).enumerate() {
                    self.cx.check()?;
                    if ordinal != 0 {
                        self.raw("\r\n")?;
                    }
                    let mut encoded = [0; 64];
                    let count = base64::engine::general_purpose::STANDARD
                        .encode_slice(chunk, &mut encoded)
                        .map_err(|_| BudgetError::Overflow)?;
                    self.bytes
                        .extend(encoded.get(..count).ok_or(BudgetError::Overflow)?)?;
                }
                Ok(())
            }
            Prepared::Other(Value::QName(..)) => Err(BuildError::Invalid("unprepared QName")),
        }
    }
    fn write<'n>(
        &mut self,
        node: &Node<'n>,
        depth: usize,
        parent: &dyn Scoped<'n>,
        root: Option<&[(Option<&'n str>, &'n str)]>,
    ) -> Result<()> {
        self.cx.check()?;
        if depth > MAX_DEPTH {
            return Err(BuildError::Invalid("element nesting too deep"));
        }
        let mut scope = Scope::new(self.cx.allocation, Some(parent)).map_err(scoped_error)?;
        for &(p, u) in root.unwrap_or_default() {
            scope
                .declare(p.map(Prefix::Borrowed), u)
                .map_err(scoped_error)?;
        }
        let prefix = if node.ns == XML_NS {
            Some(Prefix::Borrowed("xml"))
        } else if scope.uri(None).unwrap_or("") == node.ns {
            None
        } else if let Some(prefix) = scope.prefix(node.ns) {
            Some(prefix)
        } else {
            scope.declare(None, node.ns).map_err(scoped_error)?;
            None
        };
        let name = QName {
            prefix,
            local: node.local,
        };
        if !self.embedded
            && root.is_none()
            && node.local == "settings"
            && matches!(node.ns, SETTINGS | SCHEMA)
        {
            for &(prefix, uri) in &SETTINGS_ROOT {
                if let Some(prefix) = prefix
                    && parent.uri(Some(Prefix::Borrowed(prefix))) != Some(uri)
                {
                    scope
                        .declare(Some(Prefix::Borrowed(prefix)), uri)
                        .map_err(scoped_error)?;
                }
            }
        }
        let mut ordinal = 0;
        let mut attrs = self.cx.allocation.vector(node.attrs.len())?;
        let font = node.attrs.iter().any(|a| {
            (a.ns, a.local) == (XSI, "type") && matches!(a.value, Value::QName(V8UI, "Font"))
        });
        let is_type = |attr: &&tree::Attribute<'_>| (attr.ns, attr.local) == (XSI, "type");
        let ordered_attrs = node
            .attrs
            .iter()
            .filter(|attr| !self.embedded || is_type(attr))
            .chain(
                node.attrs
                    .iter()
                    .filter(|attr| self.embedded && !is_type(attr)),
            );
        for attr in ordered_attrs {
            let preferences = Preferences {
                font_ref: font && attr.local == "ref",
                embedded: self.embedded,
            };
            let value = Self::prepare(attr.value, &mut scope, depth, &mut ordinal, preferences)?;
            let name = Self::qname(
                &mut scope,
                attr.ns,
                attr.local,
                depth,
                &mut ordinal,
                true,
                Preferences {
                    font_ref: false,
                    embedded: self.embedded,
                },
            )?;
            attrs.push((name, value))?;
        }
        let text = node
            .text
            .map(|value| {
                Self::prepare(
                    value,
                    &mut scope,
                    depth,
                    &mut ordinal,
                    Preferences {
                        font_ref: false,
                        embedded: self.embedded,
                    },
                )
            })
            .transpose()?;
        self.line(depth)?;
        self.raw("<")?;
        self.name(name)?;
        for binding in scope.bindings.iter() {
            self.raw(" xmlns")?;
            if let Some(prefix) = binding.prefix {
                self.raw(":")?;
                prefix.with_text(|p| self.raw(p))?;
            }
            self.raw("=\"")?;
            self.escape(binding.uri, true)?;
            self.raw("\"")?;
        }
        for &(name, value) in attrs.iter() {
            self.raw(" ")?;
            self.name(name)?;
            self.raw("=\"")?;
            self.value(value, true)?;
            self.raw("\"")?;
        }
        if node.children.is_empty() {
            match text {
                Some(text) if !text.empty() => {
                    self.raw(">")?;
                    self.value(text, false)?;
                    self.raw("</")?;
                    self.name(name)?;
                    self.raw(">")?;
                }
                _ => self.raw("/>")?,
            }
        } else {
            if text.is_some_and(|t| !t.empty()) {
                return Err(BuildError::Invalid("mixed content"));
            }
            self.raw(">")?;
            for child in node.children.iter() {
                self.write(child, depth + 1, &scope, None)?;
            }
            self.line(depth)?;
            self.raw("</")?;
            self.name(name)?;
            self.raw(">")?;
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum Prepared<'a> {
    QName(QName<'a>),
    Other(Value<'a>),
}
impl Prepared<'_> {
    fn empty(self) -> bool {
        matches!(self, Self::Other(Value::Text("") | Value::Embedded([])))
    }
}
pub(super) fn schema_file(
    inner: &[Node<'_>],
    extra: &[Node<'_>],
    cx: Context<'_>,
) -> Result<Owned<Vec<u8>>> {
    let mut writer = Internal::new(cx)?;
    writer.raw(HEAD)?;
    writer.line(1)?;
    writer.raw("<SchemaFile xmlns=\"\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">")?;
    let mut root = Scope::new(cx.allocation, None).map_err(scoped_error)?;
    for (p, u) in [(None, ""), (Some("xs"), XS), (Some("xsi"), XSI)] {
        root.declare(p.map(Prefix::Borrowed), u)
            .map_err(scoped_error)?;
    }
    let mut schema = Scope::new(cx.allocation, Some(&root)).map_err(scoped_error)?;
    schema.declare(None, SCHEMA).map_err(scoped_error)?;
    writer.line(2)?;
    if inner.is_empty() {
        writer.raw(
            "<dataCompositionSchema xmlns=\"http://v8.1c.ru/8.1/data-composition-system/schema\"/>",
        )?;
    } else {
        writer.raw(
            "<dataCompositionSchema xmlns=\"http://v8.1c.ru/8.1/data-composition-system/schema\">",
        )?;
        for node in inner {
            writer.write(node, 3, &schema, None)?;
        }
        writer.line(2)?;
        writer.raw("</dataCompositionSchema>")?;
    }
    for node in extra {
        writer.write(node, 2, &root, None)?;
    }
    writer.line(1)?;
    writer.raw("</SchemaFile>")?;
    Ok(writer.bytes.finish())
}
pub(super) fn settings_file(node: &Node<'_>, cx: Context<'_>) -> Result<Owned<Vec<u8>>> {
    let mut writer = Internal::new(cx)?;
    writer.raw(HEAD)?;
    let root = Scope::new(cx.allocation, None).map_err(scoped_error)?;
    writer.write(node, 1, &root, Some(&SETTINGS_ROOT))?;
    Ok(writer.bytes.finish())
}
pub(super) fn appearance_key(node: &Node<'_>, cx: Context<'_>) -> Result<Owned<Vec<u8>>> {
    let mut writer = Internal::new(cx)?;
    let root = Scope::new(cx.allocation, None).map_err(scoped_error)?;
    writer.write(node, 1, &root, None)?;
    Ok(writer.bytes.finish())
}

pub(super) fn embedded_file(
    node: &Node<'_>,
    namespaces: EmbeddedNamespaces,
    cx: Context<'_>,
) -> Result<Owned<Vec<u8>>> {
    let mut writer = Internal::new(cx)?;
    writer.embedded = true;
    writer.raw(HEAD)?;
    let scope = Scope::new(cx.allocation, None).map_err(scoped_error)?;
    let mut declarations = cx.allocation.vector(11)?;
    declarations.push((None, node.ns))?;
    match namespaces {
        EmbeddedNamespaces::Plain => {
            declarations.push((Some("xs"), XS))?;
            declarations.push((Some("xsi"), XSI))?;
        }
        EmbeddedNamespaces::Settings => {
            for &(prefix, uri) in &SETTINGS_ROOT {
                if prefix.is_some() {
                    declarations.push((prefix, uri))?;
                }
            }
        }
    }
    writer.write(node, 1, &scope, Some(&declarations))?;
    Ok(writer.bytes.finish())
}
