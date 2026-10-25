use crate::index::Index;
use crate::model::*;
use crate::xml::XmlNode;
use crate::xml_out::value_elem;
use brace::Guid;
use mdschema::{Codec, Schema};
use std::collections::HashMap;

#[derive(Debug, Default)]
pub(crate) struct Names {
    by_name: HashMap<String, Option<Guid>>,
    by_type: HashMap<String, Option<Guid>>,
}

impl Names {
    pub(crate) fn new(ix: &Index) -> Names {
        fn invert(m: &HashMap<Guid, String>) -> HashMap<String, Option<Guid>> {
            let mut out: HashMap<String, Option<Guid>> = HashMap::with_capacity(m.len());
            for (id, name) in m {
                out.entry(name.clone())
                    .and_modify(|g| *g = None)
                    .or_insert(Some(*id));
            }
            out
        }
        Names {
            by_name: invert(&ix.names),
            by_type: invert(&ix.types),
        }
    }
}

pub(crate) struct XCtx<'a> {
    pub schema: &'static Schema,
    pub names: &'a Names,
    pub diag: &'a mut Diagnostics,
    pub scope: String,
}

impl XCtx<'_> {
    fn miss(&mut self, what: &str, tag: &str) {
        *self
            .diag
            .misses
            .entry(format!("{what}:{}.{tag}", self.scope))
            .or_default() += 1;
    }
}

fn qname(n: &XmlNode) -> String {
    match &n.prefix {
        Some(p) => format!("{p}:{}", n.local),
        None => n.local.clone(),
    }
}

fn attr_qname(a: &crate::xml::XmlAttr) -> String {
    match &a.prefix {
        Some(p) => format!("{p}:{}", a.local),
        None => a.local.clone(),
    }
}

fn is_expanded_text(t: &str) -> bool {
    t.strip_prefix("\r\n")
        .is_some_and(|r| r.bytes().all(|b| b == b'\t'))
}

pub(crate) fn xml_elem(el: &XmlNode) -> XmlElem {
    let mut attrs = Vec::with_capacity(el.ns_decls.len() + el.attrs.len());
    for (p, u) in &el.ns_decls {
        let name = match p {
            Some(p) => format!("xmlns:{p}"),
            None => "xmlns".to_string(),
        };
        attrs.push((name, u.clone()));
    }
    for a in &el.attrs {
        attrs.push((attr_qname(a), a.value.clone()));
    }
    let children: Vec<XmlElem> = el.children.iter().map(xml_elem).collect();
    let (text, expanded) = match el.text.as_deref() {
        _ if !children.is_empty() => (None, false),
        None | Some("") => (None, false),
        Some(t) => (Some(t.to_string()), false),
    };
    XmlElem {
        name: qname(el),
        attrs,
        text,
        children,
        expanded,
    }
}

fn same(a: &XmlElem, b: &XmlElem, depth: Option<usize>) -> bool {
    let expanded_body = a.expanded
        && b.text.as_deref().is_some_and(|t| {
            is_expanded_text(t) && depth.is_none_or(|d| t.len().saturating_sub(2) == d)
        });
    let text = |e: &XmlElem| e.text.clone().filter(|t| !t.is_empty());
    a.name == b.name
        && a.attrs == b.attrs
        && (expanded_body || text(a) == text(b))
        && a.children.len() == b.children.len()
        && (!a.children.is_empty() || expanded_body || a.expanded == b.expanded)
        && a.children
            .iter()
            .zip(&b.children)
            .all(|(x, y)| same(x, y, depth.map(|d| d.saturating_add(1))))
}

fn canonical_names(el: &XmlNode) -> bool {
    fn namespace(prefix: &str) -> Option<&'static str> {
        match prefix {
            "v8" => Some("http://v8.1c.ru/8.1/data/core"),
            "xr" => Some("http://v8.1c.ru/8.3/xcf/readable"),
            "xsi" => Some("http://www.w3.org/2001/XMLSchema-instance"),
            _ => None,
        }
    }
    let matches = |prefix: Option<&str>, ns: Option<&str>| {
        prefix
            .and_then(namespace)
            .is_none_or(|expected| ns == Some(expected))
    };
    matches(el.prefix.as_deref(), el.ns.as_deref())
        && el
            .attrs
            .iter()
            .all(|a| matches(a.prefix.as_deref(), a.ns.as_deref()))
        && el.children.iter().all(canonical_names)
}

fn is_leaf(el: &XmlNode) -> bool {
    el.attrs.is_empty() && el.ns_decls.is_empty() && el.children.is_empty()
}

fn text_of(el: &XmlNode) -> String {
    el.text.clone().unwrap_or_default()
}

pub(crate) fn value(cx: &mut XCtx, codec: Codec, el: &XmlNode) -> Value {
    property(cx, codec, None, el, None)
}

pub(crate) fn property(
    cx: &mut XCtx,
    codec: Codec,
    enum_map: Option<&std::collections::BTreeMap<String, String>>,
    el: &XmlNode,
    depth: Option<usize>,
) -> Value {
    let tag = qname(el);
    if codec == Codec::Unknown
        || !canonical_names(el)
        || (codec == Codec::Enum
            && enum_map.is_some_and(|map| {
                !map.is_empty()
                    && !map
                        .values()
                        .any(|v| v == el.text.as_deref().unwrap_or_default())
            }))
    {
        return raw_value(cx, codec, el);
    }
    let mut diagnostics = Diagnostics::default();
    let mut attempt = XCtx {
        schema: cx.schema,
        names: cx.names,
        diag: &mut diagnostics,
        scope: cx.scope.clone(),
    };
    let typed = match codec {
        Codec::Bool => bool_value(el),
        Codec::Int => (is_leaf(el) && el.text.as_deref().is_some_and(|t| t.parse::<i64>().is_ok()))
            .then(|| Value::Text(text_of(el))),
        Codec::Str
        | Codec::Enum
        | Codec::Event
        | Codec::Method
        | Codec::CmdGroup
        | Codec::Shortcut
        | Codec::Color => is_leaf(el).then(|| Value::Text(text_of(el))),
        Codec::Ref => ref_value(&mut attempt, el),
        Codec::LStr => lstr(el).map(Value::LStr),
        Codec::Type => type_desc(&mut attempt, el).map(Value::Type),
        Codec::TValue => typed_value(el, None).map(Value::Typed),
        Codec::MdList => ref_list(&mut attempt, el, "xr:Item", true),
        Codec::FoContent => ref_list(&mut attempt, el, "xr:Object", false),
        Codec::FieldList => field_list(el),
        Codec::StdAttrs => std_attrs(&mut attempt, el, depth),
        _ => None,
    };
    match typed {
        Some(v)
            if matches!(v, Value::Text(_) | Value::Bool(_) | Value::Ref(_))
                || same(&expected(codec, &tag, &v), &xml_elem(el), depth) =>
        {
            for (key, count) in diagnostics.misses {
                *cx.diag.misses.entry(key).or_default() += count;
            }
            v
        }
        _ => raw_value(cx, codec, el),
    }
}

fn raw_value(cx: &mut XCtx, codec: Codec, el: &XmlNode) -> Value {
    cx.miss(
        if codec == Codec::Unknown {
            "unknown"
        } else {
            "raw"
        },
        &qname(el),
    );
    Value::Xml(xml_elem(el))
}

fn expected(codec: Codec, tag: &str, v: &Value) -> XmlElem {
    match v {
        Value::RefList(items) if codec == Codec::FoContent => {
            let mut e = XmlElem::new(tag);
            for r in items {
                e = e.child(XmlElem::leaf("xr:Object", &r.name));
            }
            e
        }
        Value::StandardAttributes(attributes) => {
            let mut element = XmlElem::new(tag).expanded();
            for attribute in attributes {
                let mut record = XmlElem::new("xr:StandardAttribute")
                    .with_attr("name", &attribute.name)
                    .expanded();
                for prop in &attribute.props {
                    record = record.child(expected(
                        prop.codec,
                        &format!("xr:{}", prop.name),
                        &prop.value,
                    ));
                }
                element = element.child(record);
            }
            element
        }
        _ => value_elem(tag, v),
    }
}

fn bool_value(el: &XmlNode) -> Option<Value> {
    if !is_leaf(el) {
        return None;
    }
    match el.text.as_deref() {
        Some("true") => Some(Value::Bool(true)),
        Some("false") => Some(Value::Bool(false)),
        _ => None,
    }
}

fn md_ref(cx: &mut XCtx, name: &str, tag: &str) -> MdRef {
    let id = cx.names.by_name.get(name).copied().flatten();
    if id.is_none() && !name.is_empty() {
        cx.miss("unresolved", tag);
    }
    MdRef {
        id,
        name: name.to_string(),
    }
}

fn ref_value(cx: &mut XCtx, el: &XmlNode) -> Option<Value> {
    if !is_leaf(el) {
        return None;
    }
    let tag = qname(el);
    Some(Value::Ref(md_ref(cx, &text_of(el), &tag)))
}

fn ref_list(cx: &mut XCtx, el: &XmlNode, item: &str, typed_item: bool) -> Option<Value> {
    if !el.attrs.is_empty() || !el.ns_decls.is_empty() || el.text.is_some() {
        return None;
    }
    let tag = qname(el);
    let mut refs = Vec::with_capacity(el.children.len());
    for c in &el.children {
        let attrs_ok = if typed_item {
            matches!(c.attrs.as_slice(), [a] if attr_qname(a) == "xsi:type" && a.value == "xr:MDObjectRef")
        } else {
            c.attrs.is_empty()
        };
        if qname(c) != item || !attrs_ok || !c.ns_decls.is_empty() || !c.children.is_empty() {
            return None;
        }
        refs.push(md_ref(cx, &text_of(c), &tag));
    }
    Some(Value::RefList(refs))
}

fn field_list(el: &XmlNode) -> Option<Value> {
    if !el.attrs.is_empty() || !el.ns_decls.is_empty() || el.text.is_some() {
        return None;
    }
    el.children
        .iter()
        .map(|c| (qname(c) == "xr:Field" && is_leaf(c)).then(|| text_of(c)))
        .collect::<Option<Vec<_>>>()
        .map(Value::FieldList)
}

fn lstr(el: &XmlNode) -> Option<LocalString> {
    if !el.attrs.is_empty() || !el.ns_decls.is_empty() || el.text.is_some() {
        return None;
    }
    let mut items = Vec::with_capacity(el.children.len());
    for it in &el.children {
        if qname(it) != "v8:item" || !it.attrs.is_empty() || it.text.is_some() {
            return None;
        }
        let [lang, content] = it.children.as_slice() else {
            return None;
        };
        if qname(lang) != "v8:lang" || qname(content) != "v8:content" {
            return None;
        }
        if !is_leaf(lang) || !is_leaf(content) {
            return None;
        }
        items.push((text_of(lang), text_of(content)));
    }
    Some(LocalString(items))
}

fn child_text<'a>(n: &'a XmlNode, name: &str) -> Option<&'a str> {
    let c = n.children.iter().find(|c| qname(c) == name)?;
    Some(c.text.as_deref().unwrap_or_default())
}

fn qualifier_length(n: &XmlNode) -> Option<(u32, bool)> {
    let length = child_text(n, "v8:Length")?.parse().ok()?;
    let fixed = match child_text(n, "v8:AllowedLength")? {
        "Fixed" => true,
        "Variable" => false,
        _ => return None,
    };
    Some((length, fixed))
}

pub(crate) fn type_desc(cx: &mut XCtx, el: &XmlNode) -> Option<TypeDescription> {
    let tag = qname(el);
    let mut d = TypeDescription::default();
    for c in &el.children {
        match qname(c).as_str() {
            q @ ("v8:Type" | "v8:TypeSet") => {
                let qn = text_of(c);
                let set = q == "v8:TypeSet";
                let platform = cx.schema.platform_types().find(|(_, ty)| ty.qname == qn);
                let generated = qn.strip_prefix("cfg:").filter(|n| n.contains('.'));
                let expected_set = generated
                    .map(|n| {
                        n.split('.').next().is_some_and(|category| {
                            crate::objects::types::SET_CATEGORIES.contains(&category)
                        })
                    })
                    .or_else(|| platform.as_ref().map(|(_, ty)| ty.set))
                    .or_else(|| {
                        matches!(
                            qn.as_str(),
                            "xs:boolean"
                                | "xs:string"
                                | "xs:decimal"
                                | "xs:dateTime"
                                | "xs:base64Binary"
                                | "v8:TypeDescription"
                        )
                        .then_some(false)
                    });
                if expected_set.is_some_and(|expected| expected != set) {
                    return None;
                }
                let type_id = match qn.strip_prefix("cfg:") {
                    Some(n) => {
                        let id = cx.names.by_type.get(n).copied().flatten();
                        if id.is_none() && !cx.schema.platform_types().any(|(_, ty)| ty.qname == qn)
                        {
                            cx.miss("unresolved-type", &tag);
                        }
                        id
                    }
                    None => None,
                };
                d.types.push(TypeRef {
                    qname: qn,
                    set,
                    type_id,
                });
            }
            "v8:NumberQualifiers" => {
                d.number = Some(NumberQualifiers {
                    digits: child_text(c, "v8:Digits")?.parse().ok()?,
                    fraction: child_text(c, "v8:FractionDigits")?.parse().ok()?,
                    nonnegative: match child_text(c, "v8:AllowedSign")? {
                        "Nonnegative" => true,
                        "Any" => false,
                        _ => return None,
                    },
                });
            }
            "v8:StringQualifiers" => {
                let (length, fixed) = qualifier_length(c)?;
                d.string = Some(StringQualifiers { length, fixed });
            }
            "v8:DateQualifiers" => {
                d.date = Some(match child_text(c, "v8:DateFractions")? {
                    "Date" => DateFractions::Date,
                    "Time" => DateFractions::Time,
                    "DateTime" => DateFractions::DateTime,
                    _ => return None,
                });
            }
            "v8:BinaryDataQualifiers" => {
                let (length, fixed) = qualifier_length(c)?;
                d.binary = Some(BinaryQualifiers { length, fixed });
            }
            _ => return None,
        }
    }
    Some(d)
}

fn xs_bool(s: &str) -> Option<bool> {
    match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn typed_value(el: &XmlNode, item: Option<&str>) -> Option<TypedValue> {
    if item.is_some_and(|i| qname(el) != i) || !el.ns_decls.is_empty() {
        return None;
    }
    let [a] = el.attrs.as_slice() else {
        return None;
    };
    let text = || text_of(el);
    match (attr_qname(a).as_str(), a.value.as_str()) {
        ("xsi:nil", "true") => {
            (el.children.is_empty() && el.text.is_none()).then_some(TypedValue::Nil)
        }
        ("xsi:type", "v8:FixedArray") => {
            if el.text.as_deref().is_some_and(|t| !is_expanded_text(t)) {
                return None;
            }
            let items = el
                .children
                .iter()
                .map(|c| typed_value(c, Some("v8:Value")))
                .collect::<Option<Vec<_>>>()?;
            Some(TypedValue::FixedArray(items))
        }
        (_, _) if !el.children.is_empty() => None,
        ("xsi:type", "v8:TypeDescription") => el
            .text
            .is_none()
            .then_some(TypedValue::EmptyTypeDescription),
        ("xsi:type", "xs:string") => Some(TypedValue::Str(text())),
        ("xsi:type", "xs:decimal") => Some(TypedValue::Number(text())),
        ("xsi:type", "xs:boolean") => xs_bool(&text()).map(TypedValue::Bool),
        ("xsi:type", "xs:dateTime") => Some(TypedValue::Date(text())),
        ("xsi:type", "xr:DesignTimeRef") => Some(TypedValue::DesignTimeRef(text())),
        _ => None,
    }
}

fn std_attrs(cx: &mut XCtx, el: &XmlNode, depth: Option<usize>) -> Option<Value> {
    if !el.attrs.is_empty()
        || !el.ns_decls.is_empty()
        || el.text.is_some() && el.children.is_empty()
    {
        if !(el.children.is_empty() && el.text.as_deref().is_some_and(is_expanded_text)) {
            return None;
        }
    }
    let mut out = Vec::with_capacity(el.children.len());
    for a in &el.children {
        if qname(a) != "xr:StandardAttribute"
            || !a.ns_decls.is_empty()
            || a.text
                .as_deref()
                .is_some_and(|text| !is_expanded_text(text))
        {
            return None;
        }
        let [n] = a.attrs.as_slice() else {
            return None;
        };
        if attr_qname(n) != "name" {
            return None;
        }
        let mut props = Vec::with_capacity(a.children.len());
        for p in &a.children {
            let q = qname(p);
            let name = q.strip_prefix("xr:")?;
            let spec = cx
                .schema
                .bag_properties()
                .find(|(_, p)| p.xml == name)
                .map(|(_, p)| p);
            let codec = spec.map_or(Codec::Unknown, |p| p.codec);
            props.push(Prop {
                name: name.to_string(),
                codec,
                value: property(
                    cx,
                    codec,
                    spec.map(|s| &s.map),
                    p,
                    depth.map(|d| d.saturating_add(2)),
                ),
            });
        }
        out.push(StandardAttribute {
            name: n.value.clone(),
            props,
        });
    }
    Some(Value::StandardAttributes(out))
}
