use crate::XmlWriter;
use md_model::model::*;
use mdschema::Codec;

#[cfg_attr(not(test), allow(dead_code))]
pub fn write_prop(w: &mut XmlWriter, tag: &str, codec: Codec, v: &Value) {
    match v {
        Value::Text(t) => w.leaf(tag, Some(t), &[]),
        Value::Bool(b) => w.leaf(tag, Some(if *b { "true" } else { "false" }), &[]),
        Value::LStr(ls) => write_lstr(w, tag, ls),
        Value::Ref(r) => w.leaf(tag, Some(&r.name), &[]),
        Value::Type(t) => write_type(w, tag, t),
        Value::Typed(t) => write_typed(w, tag, t),
        Value::RefList(items) => {
            let (item, attrs): (&str, &[(&str, &str)]) = if codec == Codec::FoContent {
                ("xr:Object", &[])
            } else {
                ("xr:Item", &[("xsi:type", "xr:MDObjectRef")])
            };
            write_list(w, tag, items.iter().map(|r| r.name.as_str()), item, attrs);
        }
        Value::FieldList(f) => write_list(w, tag, f.iter().map(String::as_str), "xr:Field", &[]),
        Value::StandardAttributes(sa) => {
            w.open(tag, &[]);
            for a in sa {
                w.open("xr:StandardAttribute", &[("name", &a.name)]);
                for p in &a.props {
                    write_prop(w, &format!("xr:{}", p.name), p.codec, &p.value);
                }
                w.close("xr:StandardAttribute");
            }
            w.close(tag);
        }
        Value::Xml(e) => w.elem_as(tag, e),
    }
}

fn write_lstr(w: &mut XmlWriter, tag: &str, ls: &LocalString) {
    if ls.0.is_empty() {
        w.leaf(tag, None, &[]);
        return;
    }
    w.open(tag, &[]);
    for (lang, text) in &ls.0 {
        w.open("v8:item", &[]);
        w.leaf("v8:lang", Some(lang), &[]);
        w.leaf("v8:content", Some(text), &[]);
        w.close("v8:item");
    }
    w.close(tag);
}

fn write_list<'a>(
    w: &mut XmlWriter,
    tag: &str,
    items: impl ExactSizeIterator<Item = &'a str>,
    item: &str,
    attrs: &[(&str, &str)],
) {
    if items.len() == 0 {
        w.leaf(tag, None, &[]);
        return;
    }
    w.open(tag, &[]);
    for i in items {
        w.leaf(item, Some(i), attrs);
    }
    w.close(tag);
}

fn allowed(fixed: bool) -> &'static str {
    if fixed { "Fixed" } else { "Variable" }
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn write_type(w: &mut XmlWriter, tag: &str, t: &TypeDescription) {
    w.elem(&type_elem(tag, t));
}

fn type_elem(tag: &str, t: &TypeDescription) -> XmlElem {
    let mut e = XmlElem::new(tag);
    let leaf = |name: &str, text: &str| XmlElem::leaf(name, text);
    for r in t.types.iter().filter(|r| !r.set) {
        e = e.child(leaf("v8:Type", &r.qname));
    }
    for r in t.types.iter().filter(|r| r.set) {
        e = e.child(leaf("v8:TypeSet", &r.qname));
    }
    if t.types.is_empty() {
        return e;
    }
    if let Some(n) = &t.number {
        e = e.child(
            XmlElem::new("v8:NumberQualifiers")
                .child(leaf("v8:Digits", &n.digits.to_string()))
                .child(leaf("v8:FractionDigits", &n.fraction.to_string()))
                .child(leaf(
                    "v8:AllowedSign",
                    if n.nonnegative { "Nonnegative" } else { "Any" },
                )),
        );
    }
    if let Some(s) = &t.string {
        e = e.child(
            XmlElem::new("v8:StringQualifiers")
                .child(leaf("v8:Length", &s.length.to_string()))
                .child(leaf("v8:AllowedLength", allowed(s.fixed))),
        );
    }
    if let Some(d) = &t.date {
        let f = match d {
            DateFractions::Date => "Date",
            DateFractions::Time => "Time",
            DateFractions::DateTime => "DateTime",
        };
        e = e.child(XmlElem::new("v8:DateQualifiers").child(leaf("v8:DateFractions", f)));
    }
    if let Some(b) = &t.binary {
        e = e.child(
            XmlElem::new("v8:BinaryDataQualifiers")
                .child(leaf("v8:Length", &b.length.to_string()))
                .child(leaf("v8:AllowedLength", allowed(b.fixed))),
        );
    }
    e
}

pub fn value_elem(tag: &str, v: &Value) -> XmlElem {
    let list = |items: Vec<&str>, item: &str, attrs: &[(&str, &str)]| {
        let mut e = XmlElem::new(tag);
        for i in items {
            let mut c = XmlElem::leaf(item, i);
            for (k, val) in attrs {
                c = c.with_attr(k, val);
            }
            e = e.child(c);
        }
        e
    };
    match v {
        Value::Text(t) => XmlElem::leaf(tag, t),
        Value::Bool(b) => XmlElem::leaf(tag, if *b { "true" } else { "false" }),
        Value::LStr(ls) => {
            let mut e = XmlElem::new(tag);
            for (lang, text) in &ls.0 {
                e = e.child(
                    XmlElem::new("v8:item")
                        .child(XmlElem::leaf("v8:lang", lang))
                        .child(XmlElem::leaf("v8:content", text)),
                );
            }
            e
        }
        Value::Ref(r) => XmlElem::leaf(tag, &r.name),
        Value::Type(t) => type_elem(tag, t),
        Value::Typed(t) => typed_elem(tag, t),
        Value::RefList(items) => list(
            items.iter().map(|r| r.name.as_str()).collect(),
            "xr:Item",
            &[("xsi:type", "xr:MDObjectRef")],
        ),
        Value::FieldList(f) => list(f.iter().map(String::as_str).collect(), "xr:Field", &[]),
        Value::StandardAttributes(sa) => {
            let mut e = XmlElem::new(tag).expanded();
            for a in sa {
                let mut c = XmlElem::new("xr:StandardAttribute").with_attr("name", &a.name);
                for p in &a.props {
                    c = c.child(value_elem(&format!("xr:{}", p.name), &p.value));
                }
                e = e.child(c);
            }
            e
        }
        Value::Xml(e) => {
            let mut e = e.clone();
            e.name = tag.to_string();
            e
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_typed(w: &mut XmlWriter, tag: &str, v: &TypedValue) {
    w.elem(&typed_elem(tag, v));
}

pub fn typed_elem(tag: &str, v: &TypedValue) -> XmlElem {
    let (ty, text): (&str, &str) = match v {
        TypedValue::Nil => return XmlElem::new(tag).with_attr("xsi:nil", "true"),
        TypedValue::EmptyTypeDescription => {
            return XmlElem::new(tag).with_attr("xsi:type", "v8:TypeDescription");
        }
        TypedValue::FixedArray(items) => {
            let mut e = XmlElem::new(tag)
                .with_attr("xsi:type", "v8:FixedArray")
                .expanded();
            for i in items {
                e = e.child(typed_elem("v8:Value", i));
            }
            return e;
        }
        TypedValue::Str(s) => ("xs:string", s),
        TypedValue::Number(s) => ("xs:decimal", s),
        TypedValue::Bool(b) => ("xs:boolean", if *b { "true" } else { "false" }),
        TypedValue::Date(s) => ("xs:dateTime", s),
        TypedValue::DesignTimeRef(s) => ("xr:DesignTimeRef", s),
    };
    XmlElem::leaf(tag, text).with_attr("xsi:type", ty)
}
