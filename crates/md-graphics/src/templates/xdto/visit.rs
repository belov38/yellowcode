use super::{CFG, MAX_WRITE_DEPTH, STYLE, V8, V8UI, XS, XSI};
use super::{
    overlay::{Value, View},
    scope::{Ctx, Prefix, QName, Scope, Scoped},
};
use crate::{
    graph::read::{R, Read},
    templates::{
        allocation::{Allocation, Vector},
        context::Lookup,
    },
    xml::view::NodeRef,
    xml_out::emit::XmlEmit,
};
use std::cell::Cell;

pub(in crate::templates) const NS_AREA: &str =
    "http://v8.1c.ru/8.1/data-composition-system/area-template";
pub(in crate::templates) struct Policy<'a, 'src> {
    pub lookup: Option<&'a dyn Lookup>,
    pub appearances: &'a [NodeRef<'a, 'src>],
    pub failed: &'a Cell<bool>,
}
#[derive(Clone, Copy)]
struct TypeKey<'a> {
    group: u8,
    text: &'a str,
}
impl<'a, 'src> Policy<'a, 'src> {
    fn type_id(&self, tid: &str) -> Option<(&'static str, &'static str, &'a str)> {
        let lookup = self.lookup?;
        if let Some(name) = brace::Guid::parse(tid).and_then(|id| lookup.type_name(&id)) {
            let head = name.split('.').next().unwrap_or("");
            if head == "DefinedType" {
                return None;
            }
            return Some((
                if head == "Characteristic" {
                    "TypeSet"
                } else {
                    "Type"
                },
                CFG,
                name,
            ));
        }
        let value = mdschema::static_schema()
            .table("types.json")?
            .get(tid)?
            .as_array()?;
        let (prefix, local) = value.get(1)?.as_str()?.split_once(':')?;
        let ns = match prefix {
            "cfg" => CFG,
            "v8" => V8,
            "xs" => XS,
            "v8ui" => V8UI,
            "ent" => "http://v8.1c.ru/8.1/data/enterprise",
            _ => {
                self.failed.set(true);
                return None;
            }
        };
        Some((
            if value.first()?.as_str()? == "TypeSet" {
                "TypeSet"
            } else {
                "Type"
            },
            ns,
            local,
        ))
    }
    fn object(&self, id: &str) -> Option<(&'static str, &'a str)> {
        let name = self.lookup?.name(&brace::Guid::parse(id)?)?;
        let (kind, local) = name.split_once('.')?;
        match kind {
            "StyleItem" => Some((STYLE, local)),
            "CommonPicture" => Some((V8UI, local)),
            _ => None,
        }
    }
    fn type_key(&self, view: View<'a, 'src>) -> TypeKey<'a> {
        let text = view.text().and_then(Value::plain).unwrap_or("").trim();
        if view.local() == "TypeId" {
            let group = match self.type_id(text) {
                Some(("TypeSet", _, _)) => 1,
                Some(_) => 0,
                None => 2,
            };
            return TypeKey { group, text };
        }
        let group = u8::from(view.local() == "TypeSet");
        let text = match view.resolve(text) {
            Some((XS, local)) => match local {
                "boolean" => "5",
                "string" => "9f",
                "dateTime" => "a8",
                "decimal" => "b0",
                _ => "0",
            },
            Some((uri, local)) => {
                let prefix = match uri {
                    V8 => Some("v8:"),
                    CFG => Some("cfg:"),
                    _ => None,
                };
                prefix
                    .and_then(|prefix| {
                        mdschema::static_schema()
                            .table("types.json")?
                            .as_object()?
                            .iter()
                            .rev()
                            .find_map(|(id, value)| {
                                let name = value.at(1)?.as_str()?;
                                (name.strip_prefix(prefix) == Some(local)).then_some(*id)
                            })
                    })
                    .unwrap_or("0")
            }
            None => "0",
        };
        TypeKey { group, text }
    }
    fn substitute(&self, view: View<'a, 'src>) -> View<'a, 'src> {
        if self.lookup.is_none() {
            return view;
        }
        if view.ns() == NS_AREA && view.local() == "appIndex" {
            let index = view
                .text()
                .and_then(Value::plain)
                .and_then(|text| super::super::dcs::app_index(Some(text), self.appearances.len()));
            if let Some(node) = index.and_then(|index| self.appearances.get(index)).copied() {
                return View::Rename {
                    node,
                    ns: NS_AREA,
                    local: "appearance",
                    drop_attrs: true,
                };
            }
            self.failed.set(true);
            return view;
        }
        let Some(t) = view.attr(XSI, "type") else {
            return view;
        };
        let Some(reference) = view.attr("", "ref").filter(|value| !value.is_empty()) else {
            return view;
        };
        if view.resolve(t) != Some((V8UI, "Picture")) {
            return view;
        }
        let reference = match view.resolve(reference) {
            Some((_, local)) => [
                if self
                    .lookup
                    .is_some_and(|lookup| lookup.common_picture(local))
                {
                    "CommonPicture."
                } else {
                    "StdPicture."
                },
                local,
            ],
            None => ["", reference],
        };
        match view {
            View::Source(node) | View::Rename { node, .. } => View::Picture { node, reference },
            _ => view,
        }
    }
}
fn generated(prefix: &str) -> bool {
    prefix
        .strip_prefix('d')
        .and_then(|rest| rest.split_once('p'))
        .is_some_and(|(a, b)| {
            !a.is_empty()
                && !b.is_empty()
                && a.bytes().chain(b.bytes()).all(|byte| byte.is_ascii_digit())
        })
}
fn is_type(view: View<'_, '_>) -> bool {
    view.ns() == V8 && matches!(view.local(), "Type" | "TypeSet" | "TypeId")
}
struct Line<'a> {
    bytes: Vector<u8>,
    read: Read<'a>,
}
impl<'a> Line<'a> {
    fn new(read: Read<'a>, depth: usize) -> R<Self> {
        let mut out = Self {
            bytes: read.allocation.vector(depth)?,
            read,
        };
        for _ in 1..depth {
            out.bytes.push(b'\t')?
        }
        Ok(out)
    }
    fn raw(&mut self, text: &str) -> R<()> {
        self.bytes.extend(text.as_bytes())?;
        Ok(())
    }
    fn name(&mut self, name: QName<'_>) -> R<()> {
        if let Some(prefix) = name.prefix {
            prefix.with_text(|prefix| self.raw(prefix))?;
            self.raw(":")?
        }
        self.raw(name.local)
    }
    fn escape(&mut self, text: &str, attribute: bool) -> R<()> {
        let mut from = 0;
        for (at, byte) in text.bytes().enumerate() {
            let replacement = match byte {
                b'&' => "&amp;",
                b'<' => "&lt;",
                b'>' => "&gt;",
                b'\r' => "&#13;",
                b'"' if attribute => "&quot;",
                _ => continue,
            };
            self.bytes.extend(
                text.as_bytes()
                    .get(from..at)
                    .ok_or_else(|| self.read.invalid("escape range"))?,
            )?;
            self.raw(replacement)?;
            from = at + 1;
        }
        self.bytes.extend(
            text.as_bytes()
                .get(from..)
                .ok_or_else(|| self.read.invalid("escape range"))?,
        )?;
        Ok(())
    }
    fn value(&mut self, value: Value<'_>, attribute: bool) -> R<()> {
        match value {
            Value::Plain(value) => self.escape(value, attribute),
            Value::Fragments(parts) => {
                for part in parts {
                    self.escape(part, attribute)?
                }
                Ok(())
            }
            Value::QName(name) => {
                if let Some(prefix) = name.prefix {
                    prefix.with_text(|prefix| self.escape(prefix, attribute))?;
                    self.raw(":")?
                }
                self.escape(name.local, attribute)
            }
        }
    }
    fn emit(self, output: &mut XmlEmit<'_>) -> R<()> {
        output
            .raw(std::str::from_utf8(&self.bytes).map_err(|_| self.read.invalid("UTF8 output"))?)?;
        Ok(())
    }
}
pub(in crate::templates) struct Writer<'a, 'policy, 'src> {
    pub output: XmlEmit<'policy>,
    pub read: Read<'policy>,
    pub policy: Policy<'a, 'src>,
}
impl<'a, 'policy, 'src> Writer<'a, 'policy, 'src> {
    pub(in crate::templates) fn new(
        allocation: Allocation<'policy>,
        policy: Policy<'a, 'src>,
    ) -> R<Self> {
        Ok(Self {
            output: XmlEmit::new(allocation, 0)?,
            read: Read::new(allocation),
            policy,
        })
    }
    fn special(&self, view: View<'a, 'src>, value: &'a str, ctx: &mut Ctx<'a, '_>) -> R<Value<'a>> {
        if let Some(id) = value.strip_prefix("0:")
            && self.policy.lookup.is_some()
        {
            let (ns, local) = self
                .policy
                .object(id)
                .ok_or_else(|| self.read.invalid("style item"))?;
            return Ok(Value::QName(ctx.qname(ns, local, self.read)?));
        }
        if value.contains(':')
            && !value.starts_with('#')
            && let Some((ns, local)) = view.resolve(value)
        {
            return Ok(Value::QName(ctx.qname(ns, local, self.read)?));
        }
        Ok(Value::Plain(value))
    }
    pub(in crate::templates) fn write(
        &mut self,
        original: View<'a, 'src>,
        depth: usize,
        scope: &dyn Scoped<'a>,
        rename: Option<(&'a str, &'a str)>,
        append: Option<View<'a, 'src>>,
    ) -> R<()> {
        if depth > MAX_WRITE_DEPTH {
            return Err(self.read.invalid("element nesting too deep"));
        }
        let view = self.policy.substitute(original);
        let (ns, mut local) = match view {
            View::Rename { ns, local, .. } => (ns, local),
            View::Picture { .. } => (view.ns(), view.local()),
            _ => rename.unwrap_or((view.ns(), view.local())),
        };
        let mut ctx = Ctx::new(self.read.allocation, scope, depth)?;
        let parent = view.parent();
        for (prefix, uri) in view.decls() {
            let Some(prefix) = prefix else { continue };
            if generated(prefix)
                || parent.and_then(|parent| parent.lookup(Some(prefix))) == Some(uri)
            {
                continue;
            }
            if ctx.scope.prefix(uri).is_none() {
                ctx.scope.declare(Some(Prefix::Borrowed(prefix)), uri)?
            }
        }
        let mut text = view.text();
        if (ns, local) == (V8, "TypeId") && self.policy.lookup.is_some() {
            let id = text.and_then(Value::plain).unwrap_or("").trim();
            if let Some((new_local, uri, name)) = self.policy.type_id(id) {
                local = new_local;
                text = Some(Value::Fragments([uri, name]))
            }
        } else if ns == V8
            && matches!(local, "Type" | "TypeSet")
            && let Some(value) = text.and_then(Value::plain).filter(|text| !text.is_empty())
        {
            let (uri, name) = view
                .resolve(value.trim())
                .ok_or_else(|| self.read.invalid("QName"))?;
            text = Some(Value::Fragments([uri, name]));
        }
        let name = ctx.qname(ns, local, self.read)?;
        let mut attrs = self.read.allocation.vector(0)?;
        let mut xsi_type = None;
        for attr in view.attrs() {
            let mut value = Value::Plain(attr.value);
            if (attr.ns, attr.local) == (XSI, "type") {
                let (uri, local) = view
                    .resolve(attr.value)
                    .ok_or_else(|| self.read.invalid("xsi:type"))?;
                value = Value::QName(ctx.qname(uri, local, self.read)?);
                xsi_type = Some((uri, local));
            }
            let name = ctx.attribute(attr.ns, attr.local, self.read)?;
            attrs.push((name, value))?;
        }
        if xsi_type == Some((XS, "QName"))
            && let Some(value) = text
                .and_then(Value::plain)
                .filter(|value| !value.is_empty())
            && let Some((uri, local)) = view.resolve(value.trim())
        {
            text = Some(Value::Fragments([uri, local]));
        } else if xsi_type == Some((V8UI, "Color"))
            && let Some(value) = text
                .and_then(Value::plain)
                .filter(|value| !value.is_empty())
        {
            text = Some(self.special(view, value, &mut ctx)?);
        }
        if matches!(xsi_type, Some((V8UI, "Font" | "Picture"))) {
            for (name, value) in attrs.iter_mut() {
                if name.prefix.is_none()
                    && name.local == "ref"
                    && let Some(plain) = value.plain()
                {
                    *value = self.special(view, plain, &mut ctx)?
                }
            }
        }
        if let Some(Value::Fragments([uri, local])) = text
            && !matches!(view, View::PictureChild { .. })
        {
            text = Some(Value::QName(ctx.qname(uri, local, self.read)?));
        }
        let mut line = Line::new(self.read, depth)?;
        line.raw("<")?;
        line.name(name)?;
        for binding in ctx.scope.bindings.iter() {
            line.raw(" xmlns")?;
            if let Some(prefix) = binding.prefix {
                line.raw(":")?;
                prefix.with_text(|text| line.raw(text))?
            }
            line.raw("=\"")?;
            line.escape(binding.uri, true)?;
            line.raw("\"")?;
        }
        for (name, value) in attrs.iter() {
            line.raw(" ")?;
            line.name(*name)?;
            line.raw("=\"")?;
            line.value(*value, true)?;
            line.raw("\"")?
        }
        let mut children = self.read.allocation.vector(0)?;
        for (ordinal, child) in view.children().enumerate() {
            children.push((child, ordinal, None::<TypeKey<'a>>))?
        }
        if self.policy.lookup.is_some()
            && children.first().is_some_and(|(child, ..)| is_type(*child))
        {
            for (child, _, key) in children.iter_mut() {
                if is_type(*child) {
                    *key = Some(self.policy.type_key(*child))
                }
            }
            children.sort_unstable_by(|a, b| match (a.2, b.2) {
                (Some(a_key), Some(b_key)) => {
                    (a_key.group, a_key.text, a.1).cmp(&(b_key.group, b_key.text, b.1))
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.1.cmp(&b.1),
            });
        }
        if children.is_empty() && append.is_none() {
            if let Some(text) = text.filter(|text| !text.empty()) {
                line.raw(">")?;
                line.value(text, false)?;
                line.raw("</")?;
                line.name(name)?;
                line.raw(">")?
            } else {
                line.raw("/>")?
            }
            return line.emit(&mut self.output);
        }
        line.raw(">")?;
        line.emit(&mut self.output)?;
        drop(attrs);
        for (child, ..) in children.iter() {
            self.write(*child, depth + 1, &ctx.scope, None, None)?
        }
        if let Some(append) = append {
            self.write(
                append,
                depth + 1,
                &ctx.scope,
                Some((
                    "http://v8.1c.ru/8.1/data-composition-system/settings",
                    "settings",
                )),
                None,
            )?
        }
        let mut line = Line::new(self.read, depth)?;
        line.raw("</")?;
        line.name(name)?;
        line.raw(">")?;
        line.emit(&mut self.output)
    }
    pub(in crate::templates) fn root(
        &mut self,
        decls: &[(Option<&'a str>, &'a str)],
        tag: &str,
    ) -> R<Scope<'a, 'a>> {
        let mut scope = Scope::new(self.read.allocation, None)?;
        let mut line = Line::new(self.read, 1)?;
        line.raw("<")?;
        line.raw(tag)?;
        for (prefix, uri) in decls {
            scope.declare(prefix.map(Prefix::Borrowed), uri)?;
            line.raw(" xmlns")?;
            if let Some(prefix) = prefix {
                line.raw(":")?;
                line.raw(prefix)?
            }
            line.raw("=\"")?;
            line.escape(uri, true)?;
            line.raw("\"")?
        }
        line.raw(">")?;
        line.emit(&mut self.output)?;
        Ok(scope)
    }
}
