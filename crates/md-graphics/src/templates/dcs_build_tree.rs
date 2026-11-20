use super::super::xdto::overlay::View;
use super::*;
use crate::xml::view::NodeRef;

#[derive(Clone, Copy)]
pub(super) enum Value<'a> {
    Text(&'a str),
    QName(&'a str, &'a str),
    Guid(Guid),
    Style(Guid),
    Index(usize),
    Embedded(&'a [u8]),
}
pub(super) struct Attribute<'a> {
    pub ns: &'a str,
    pub local: &'a str,
    pub value: Value<'a>,
}
pub(super) struct Node<'a> {
    pub ns: &'a str,
    pub local: &'a str,
    pub attrs: Vector<Attribute<'a>>,
    pub text: Option<Value<'a>>,
    pub children: Vector<Node<'a>>,
}
impl<'a> Node<'a> {
    pub fn new(ns: &'a str, local: &'a str, cx: Context<'_>) -> Result<Self> {
        Ok(Self {
            ns,
            local,
            attrs: cx.allocation.vector(0)?,
            text: None,
            children: cx.allocation.vector(0)?,
        })
    }
    fn has_embedded(&self) -> bool {
        matches!(self.text, Some(Value::Embedded(_)))
            || self.children.iter().any(Self::has_embedded)
    }
}
pub(super) fn remove<T>(values: &mut Vector<T>, at: usize) -> Result<T> {
    values
        .get_mut(at..)
        .ok_or(BudgetError::Overflow)?
        .rotate_left(1);
    values
        .pop()
        .ok_or(BuildError::Memory(BudgetError::Overflow))
}

pub(super) struct Converter<'a, 'files> {
    pub cx: Context<'a>,
    pub lookup: &'a dyn InverseLookup,
    pub assets: &'files [Asset<'files>],
    pub embedded: bool,
}
fn resolve<'a>(node: NodeRef<'a, '_>, text: &'a str) -> Result<(&'a str, &'a str)> {
    let text = text.trim();
    let (ns, local) = View::Source(node)
        .resolve(text)
        .ok_or(BuildError::Invalid("QName"))?;
    if !mdschema::valid_type_namespace(ns)
        || !mdschema::is_ncname(local)
        || text
            .split_once(':')
            .is_some_and(|(p, _)| !mdschema::is_ncname(p))
    {
        return Err(BuildError::Invalid("QName"));
    }
    Ok((ns, local))
}
fn attr<'a>(node: NodeRef<'a, '_>, ns: &str, local: &str) -> Option<&'a str> {
    node.attrs()
        .find(|a| a.ns().unwrap_or("") == ns && a.local() == local)
        .map(|a| a.value())
}
impl<'files> Converter<'_, 'files> {
    fn appearance_parameter<'a>(
        &self,
        source: NodeRef<'a, '_>,
        text: &'a str,
    ) -> Result<Option<&'static str>> {
        if !self.embedded
            || source.ns() != Some(CORE)
            || source.local() != "parameter"
            || source.attrs().next().is_some()
            || source.children().next().is_some()
        {
            return Ok(None);
        }
        let Some(parent) = source.parent() else {
            return Ok(None);
        };
        let Some(appearance) = parent.parent() else {
            return Ok(None);
        };
        if parent.ns() != Some(CORE)
            || parent.local() != "item"
            || appearance.ns() != Some(SETTINGS)
            || appearance.local() != "appearance"
            || attr(parent, XSI, "type")
                .map(|text| resolve(parent, text))
                .transpose()?
                != Some((SETTINGS, "SettingsParameterValue"))
        {
            return Ok(None);
        }
        let aliases = mdschema::static_schema()
            .table("forms/misc.json")
            .and_then(|table| table.get("external_dcs_names"))
            .and_then(|table| table.as_object())
            .ok_or(BuildError::Invalid("external appearance aliases"))?;
        for (native, alias) in aliases {
            self.cx.check()?;
            let alias = alias
                .as_str()
                .ok_or(BuildError::Invalid("external appearance alias"))?;
            if self.cx.equal(text, alias)? {
                return Ok(Some(native));
            }
        }
        Ok(None)
    }
    pub(super) fn identity(&self, source: NodeRef<'_, '_>) -> Result<()> {
        const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
        if source.ns() == Some(XMLNS) {
            return Err(BuildError::Invalid("XMLNS namespace"));
        }
        self.cx.xml_name(source.local())?;
        for node in std::iter::successors(Some(source), |node| node.parent()) {
            self.cx.check()?;
            for (prefix, uri) in node.ns_decls() {
                if let Some(prefix) = prefix {
                    self.cx.xml_name(prefix)?;
                }
                if uri == XMLNS
                    || (uri == XML_NS && prefix != Some("xml"))
                    || (prefix == Some("xml") && uri != XML_NS)
                {
                    return Err(BuildError::Invalid("XML namespace"));
                }
            }
        }
        for (index, attribute) in source.attrs().enumerate() {
            if attribute.ns() == Some(XMLNS) {
                return Err(BuildError::Invalid("XMLNS namespace"));
            }
            self.cx.xml_name(attribute.local())?;
            for previous in source.attrs().take(index) {
                if self
                    .cx
                    .equal(attribute.ns().unwrap_or(""), previous.ns().unwrap_or(""))?
                    && self.cx.equal(attribute.local(), previous.local())?
                {
                    return Err(BuildError::Invalid("duplicate expanded attribute"));
                }
            }
        }
        Ok(())
    }
    fn special<'a>(
        &self,
        node: NodeRef<'a, '_>,
        text: &'a str,
        standard_table: Option<&str>,
    ) -> Result<Value<'a>> {
        let embedded_style = self.embedded && standard_table.is_some();
        let color = standard_table == Some("style_colors");
        if embedded_style
            && standard_table == Some("style_fonts")
            && text == "0"
            && node.ns() == Some(CORE)
            && node.local() == "value"
            && node.text().is_none()
            && node.children().next().is_none()
            && node.attrs().take(8).count() == 7
            && [
                ("ref", "0"),
                ("bold", "true"),
                ("italic", "false"),
                ("underline", "false"),
                ("strikeout", "false"),
                ("kind", "StyleItem"),
            ]
            .iter()
            .all(|(name, value)| attr(node, "", name) == Some(*value))
        {
            return Ok(Value::Text(text));
        }
        if (text.starts_with('#') && (!embedded_style || color))
            || (!embedded_style && !text.contains(':'))
            || (embedded_style && color && text == "auto")
        {
            return Ok(Value::Text(text));
        }
        let (ns, local) = resolve(node, text)?;
        if embedded_style {
            let supported = match standard_table {
                Some("style_fonts") => {
                    matches!(ns, STYLE | "http://v8.1c.ru/8.1/data/ui/fonts/system")
                }
                Some("style_colors") => matches!(
                    ns,
                    STYLE
                        | "http://v8.1c.ru/8.1/data/ui/colors/palette"
                        | "http://v8.1c.ru/8.1/data/ui/colors/web"
                        | "http://v8.1c.ru/8.1/data/ui/colors/windows"
                ),
                _ => false,
            };
            if !supported {
                return Err(BuildError::Invalid("style reference namespace"));
            }
        }
        if ns == STYLE {
            let name = self.cx.allocation.text(&["StyleItem.", local])?;
            if let Some(id) = self.lookup.object_id(&name) {
                return Ok(Value::Style(id));
            }
            if embedded_style
                && standard_table
                    .and_then(|table| {
                        mdschema::static_schema()
                            .table("ui_tables.json")?
                            .get(table)?
                            .as_object()
                    })
                    .is_some_and(|values| {
                        values
                            .iter()
                            .any(|(_, value)| value.as_str() == Some(local))
                    })
            {
                return Ok(Value::QName(ns, local));
            }
            return Err(BuildError::Invalid("style reference"));
        }
        Ok(Value::QName(ns, local))
    }
    fn config_type(&self, local: &str, set: bool) -> Result<Guid> {
        if let Some(id) = self.lookup.type_id(local) {
            return Ok(id);
        }
        let table = mdschema::static_schema()
            .table("types.json")
            .and_then(|t| t.as_object())
            .ok_or(BuildError::Invalid("platform types"))?;
        let mut found = None;
        for (id, row) in table.iter() {
            self.cx.check()?;
            if let Some(ty) = mdschema::static_platform_type_row(id, row)
                && ty.name.namespace_uri == CFG
                && ty.name.local == local
                && ty.set == set
            {
                if found.is_some() {
                    return Err(BuildError::Invalid("ambiguous config type"));
                }
                found = Guid::parse(id);
            }
        }
        found.ok_or(BuildError::Invalid("config type"))
    }
    fn picture<'a>(&self, source: NodeRef<'a, '_>, mut node: Node<'a>) -> Result<Node<'a>>
    where
        'files: 'a,
    {
        if source
            .attrs()
            .any(|a| a.ns() != Some(XSI) || a.local() != "type")
        {
            return Err(BuildError::Invalid("picture attributes"));
        }
        if source.text().is_some() {
            return Err(BuildError::Invalid("picture content"));
        }
        let mut reference = None;
        let mut embedded = None;
        let mut transparent = None;
        for child in source.children() {
            self.identity(child)?;
            if child.ns() != Some(XR)
                || child.children().next().is_some()
                || child.attrs().next().is_some()
            {
                return Err(BuildError::Invalid("picture content"));
            }
            let text = child.text().unwrap_or("").trim();
            let duplicate = match child.local() {
                "Ref" => reference.replace(text).is_some(),
                "Abs" => embedded.replace(text).is_some(),
                "LoadTransparent" => transparent.replace(text).is_some(),
                _ => return Err(BuildError::Invalid("picture content")),
            };
            if duplicate {
                return Err(BuildError::Invalid("picture content"));
            }
        }
        if transparent.is_some_and(|value| value != "true") {
            return Err(BuildError::Invalid("picture transparency"));
        }
        node.attrs.push(Attribute {
            ns: XSI,
            local: "type",
            value: Value::QName(V8UI, "Picture"),
        })?;
        match (reference, embedded) {
            (Some(reference), None) => {
                let (kind, name) = reference
                    .split_once('.')
                    .filter(|(_, n)| !n.is_empty() && mdschema::is_ncname(n))
                    .ok_or(BuildError::Invalid("picture reference"))?;
                match kind {
                    "CommonPicture" if self.lookup.object_id(reference).is_some() => (),
                    "StdPicture" => {
                        let common = self.cx.allocation.text(&["CommonPicture.", name])?;
                        if self.lookup.object_id(&common).is_some() {
                            return Err(BuildError::Invalid("ambiguous picture reference"));
                        }
                        let known = ["ui.json", "std_pictures.json"].into_iter().any(|file| {
                            mdschema::static_schema()
                                .table(file)
                                .and_then(|t| t.get("std_pictures"))
                                .and_then(|t| t.as_object())
                                .is_some_and(|values| {
                                    values.iter().any(|(_, v)| v.as_str() == Some(name))
                                })
                        });
                        if !known {
                            return Err(BuildError::Invalid("picture reference"));
                        }
                    }
                    _ => return Err(BuildError::Invalid("picture reference")),
                }
                node.attrs.push(Attribute {
                    ns: "",
                    local: "ref",
                    value: Value::QName(V8UI, name),
                })?;
            }
            (None, Some(path)) => {
                if path.is_empty() || path.contains(['/', '\\', ':']) || matches!(path, "." | "..")
                {
                    return Err(BuildError::Invalid("picture asset path"));
                }
                let mut matching = self
                    .assets
                    .iter()
                    .filter(|a| a.path.strip_prefix("Template/") == Some(path));
                let asset = matching
                    .next()
                    .ok_or(BuildError::Invalid("missing picture asset"))?;
                if matching.next().is_some() {
                    return Err(BuildError::Invalid("ambiguous picture asset"));
                }
                node.text = Some(Value::Embedded(asset.bytes));
            }
            _ => return Err(BuildError::Invalid("picture content")),
        }
        Ok(node)
    }
    pub fn convert<'a>(&self, source: NodeRef<'a, '_>, depth: usize) -> Result<Node<'a>>
    where
        'files: 'a,
    {
        self.cx.check()?;
        self.identity(source)?;
        if depth > MAX_DEPTH {
            return Err(BuildError::Invalid("element nesting too deep"));
        }
        let mut node = Node::new(source.ns().unwrap_or(""), source.local(), self.cx)?;
        let ty = attr(source, XSI, "type")
            .map(|text| resolve(source, text))
            .transpose()?;
        if node.ns == V8
            && matches!(node.local, "Type" | "TypeSet" | "TypeId")
            && (source.text().is_none()
                || source.attrs().next().is_some()
                || source.children().next().is_some())
        {
            return Err(BuildError::Invalid("type content"));
        }
        if ty == Some((XR, "Picture")) {
            return self.picture(source, node);
        }
        for a in source.attrs() {
            let ns = a.ns().unwrap_or("");
            let value = if (ns, a.local()) == (XSI, "type") {
                let (ns, local) = ty.ok_or(BuildError::Invalid("QName"))?;
                Value::QName(ns, local)
            } else if ns.is_empty()
                && a.local() == "ref"
                && matches!(ty, Some((V8UI, "Font" | "Picture")))
            {
                self.special(
                    source,
                    a.value(),
                    (ty == Some((V8UI, "Font"))).then_some("style_fonts"),
                )?
            } else {
                Value::Text(a.value())
            };
            node.attrs.push(Attribute {
                ns,
                local: a.local(),
                value,
            })?;
        }
        node.text = source
            .text()
            .map(|text| -> Result<_> {
                Ok(Value::Text(
                    self.appearance_parameter(source, text)?.unwrap_or(text),
                ))
            })
            .transpose()?;
        if let Some(text) = source.text().filter(|text| !text.is_empty()) {
            if node.ns == V8 && matches!(node.local, "Type" | "TypeSet") {
                let (ns, local) = resolve(source, text)?;
                node.text = Some(if ns == CFG {
                    let id = self.config_type(local, node.local == "TypeSet")?;
                    node.local = "TypeId";
                    Value::Guid(id)
                } else {
                    Value::QName(ns, local)
                });
            } else if node.ns == V8 && node.local == "TypeId" {
                node.text = Some(Value::Guid(
                    Guid::parse(text.trim()).ok_or(BuildError::Invalid("type id"))?,
                ));
            } else if matches!(ty, Some((XS, "QName") | (V8, "Type"))) {
                let (ns, local) = resolve(source, text)?;
                node.text = Some(Value::QName(ns, local));
            } else if ty == Some((V8UI, "Color")) {
                node.text = Some(self.special(source, text, Some("style_colors"))?);
            }
        }
        for child in source.children() {
            node.children.push(self.convert(child, depth + 1)?)?;
        }
        sort_types(&mut node.children, self.cx)?;
        Ok(node)
    }
}
fn type_rank(node: &Node<'_>) -> Option<u8> {
    if node.ns != V8 {
        return None;
    }
    Some(match node.local {
        "Type" => 0,
        "TypeSet" => 1,
        "TypeId" => 2,
        _ => return None,
    })
}
fn sort_types(children: &mut Vector<Node<'_>>, cx: Context<'_>) -> Result<()> {
    if !children.iter().any(|n| n.ns == V8 && n.local == "TypeId") {
        return Ok(());
    }
    let mut ordered = cx.allocation.vector(children.len())?;
    let first = children
        .iter()
        .position(|n| type_rank(n).is_some())
        .unwrap_or(0);
    for (ordinal, child) in children.iter().enumerate() {
        let rank = type_rank(child);
        let key = if ordinal < first {
            (0, 0, Guid::NIL)
        } else if let Some(rank) = rank {
            let id = match child.text {
                Some(Value::Guid(id)) => id,
                _ => Guid::NIL,
            };
            (1, rank, id)
        } else {
            (2, 0, Guid::NIL)
        };
        ordered.push((key, ordinal))?;
    }
    ordered.sort_unstable();
    let mut result = cx.allocation.vector(children.len())?;
    let mut slots = cx.allocation.vector(children.len())?;
    while let Some(child) = children.pop() {
        slots.push(Some(child))?;
    }
    for (_, ordinal) in ordered.iter() {
        cx.check()?;
        let at = slots
            .len()
            .checked_sub(ordinal + 1)
            .ok_or(BudgetError::Overflow)?;
        result.push(
            slots
                .get_mut(at)
                .and_then(Option::take)
                .ok_or(BudgetError::Overflow)?,
        )?;
    }
    std::mem::swap(children, &mut result);
    Ok(())
}

pub(super) fn extract_appearances<'a>(
    node: &mut Node<'a>,
    appearances: &mut Vector<Node<'a>>,
    keys: &mut Vector<Option<Owned<Vec<u8>>>>,
    cx: Context<'_>,
) -> Result<()> {
    cx.check()?;
    for child in node.children.iter_mut() {
        if (node.ns, node.local, child.ns, child.local) == (AREA, "tableCell", AREA, "appearance") {
            if child.text.is_some()
                || child.attrs.iter().any(|a| {
                    (a.ns, a.local) != (XSI, "type")
                        || !matches!(a.value, Value::QName(AREA, "TableCellAppearance"))
                })
            {
                return Err(BuildError::Invalid("cell appearance content"));
            }
            let key = write::appearance_key(child, cx)?;
            let embedded = child.has_embedded();
            let index = if !embedded {
                keys.iter()
                    .position(|k| k.as_ref().is_some_and(|k| k.as_slice() == key.as_slice()))
            } else {
                None
            };
            let index = if let Some(index) = index {
                index
            } else {
                let index = appearances.len();
                let mut app = Node::new(AREA, "appearance", cx)?;
                app.attrs.push(Attribute {
                    ns: XSI,
                    local: "type",
                    value: Value::QName(AREA, "TableCellAppearance"),
                })?;
                std::mem::swap(&mut app.children, &mut child.children);
                appearances.push(app)?;
                keys.push(if embedded { None } else { Some(key) })?;
                index
            };
            let mut replacement = Node::new(AREA, "appIndex", cx)?;
            replacement.text = Some(Value::Index(index));
            *child = replacement;
        } else {
            extract_appearances(child, appearances, keys, cx)?;
        }
    }
    Ok(())
}
