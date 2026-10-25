use std::fmt;
use std::sync::Arc;

use super::tables::JsonRef;

pub(crate) const PREFIX: &[(&str, &str)] = &[
    ("http://v8.1c.ru/8.3/xcf/logform", ""),
    ("http://v8.1c.ru/8.2/managed-application/core", "app"),
    ("http://v8.1c.ru/8.1/data/enterprise/current-config", "cfg"),
    ("http://v8.1c.ru/8.1/data-composition-system/core", "dcscor"),
    (
        "http://v8.1c.ru/8.1/data-composition-system/schema",
        "dcssch",
    ),
    (
        "http://v8.1c.ru/8.1/data-composition-system/settings",
        "dcsset",
    ),
    ("http://v8.1c.ru/8.1/data/enterprise", "ent"),
    ("http://v8.1c.ru/8.2/managed-application/logform", "lf"),
    ("http://v8.1c.ru/8.1/data/ui/colors/palette", "pal"),
    ("http://v8.1c.ru/8.1/data/ui/style", "style"),
    ("http://v8.1c.ru/8.1/data/ui/fonts/system", "sys"),
    ("http://v8.1c.ru/8.1/data/core", "v8"),
    ("http://v8.1c.ru/8.1/data/ui", "v8ui"),
    ("http://v8.1c.ru/8.1/data/ui/colors/web", "web"),
    ("http://v8.1c.ru/8.1/data/ui/colors/windows", "win"),
    ("http://v8.1c.ru/8.3/xcf/readable", "xr"),
    ("http://www.w3.org/2001/XMLSchema", "xs"),
    ("http://www.w3.org/2001/XMLSchema-instance", "xsi"),
];

pub(crate) fn prefix_of(ns: &str) -> Option<&'static str> {
    PREFIX.iter().find(|(u, _)| *u == ns).map(|(_, p)| *p)
}

#[derive(Clone)]
pub(crate) struct Rendered(pub Arc<dyn Fn(usize) -> Vec<String> + Send + Sync>);

impl Rendered {
    pub fn new(f: impl Fn(usize) -> Vec<String> + Send + Sync + 'static) -> Rendered {
        Rendered(Arc::new(f))
    }
}

impl fmt::Debug for Rendered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Rendered(..)")
    }
}

impl PartialEq for Rendered {
    fn eq(&self, other: &Rendered) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct E {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub text: Option<String>,
    pub kids: Vec<E>,
    pub render: Option<Rendered>,
}

impl E {
    pub fn new(
        tag: impl Into<String>,
        text: Option<String>,
        attrs: Vec<(String, String)>,
        kids: Vec<E>,
    ) -> E {
        E {
            tag: tag.into(),
            attrs,
            text,
            kids,
            render: None,
        }
    }

    pub fn leaf(tag: impl Into<String>, text: impl Into<String>) -> E {
        E::new(tag, Some(text.into()), Vec::new(), Vec::new())
    }

    pub fn rendered(r: Rendered) -> E {
        E {
            tag: String::new(),
            attrs: Vec::new(),
            text: None,
            kids: Vec::new(),
            render: Some(r),
        }
    }
}

pub(crate) fn esc_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace("\r\n", "\n")
}

pub(crate) fn esc_attr(s: &str) -> String {
    esc_text(s).replace('"', "&quot;")
}

fn local_prefix(
    attrs: &[(String, String)],
    text: Option<&str>,
    depth: usize,
) -> (Vec<(String, String)>, Option<String>) {
    let p = format!("d{}p1", depth + 1);
    let fix = |v: &str| match v.strip_prefix("@:") {
        Some(rest) => format!("{p}:{rest}"),
        None => v.to_string(),
    };
    let mut out: Vec<(String, String)> = Vec::with_capacity(attrs.len());
    for (k, v) in attrs {
        let k = if k == "xmlns:@" {
            format!("xmlns:{p}")
        } else {
            k.clone()
        };
        let v = fix(v);
        match out.iter_mut().find(|(ek, _)| *ek == k) {
            Some(slot) => slot.1 = v,
            None => out.push((k, v)),
        }
    }
    (out, text.map(fix))
}

pub(crate) fn ser(e: &E, depth: usize, lines: &mut Vec<String>) {
    if let Some(r) = &e.render {
        lines.extend((r.0)(depth));
        return;
    }
    let local;
    let (attrs, text): (&[(String, String)], Option<&str>) =
        if e.attrs.iter().any(|(k, _)| k == "xmlns:@") {
            let (a, t) = local_prefix(&e.attrs, e.text.as_deref(), depth);
            local = (a, t);
            (&local.0, local.1.as_deref())
        } else {
            (&e.attrs, e.text.as_deref())
        };
    let a: String = attrs
        .iter()
        .map(|(k, v)| format!(" {k}=\"{}\"", esc_attr(v)))
        .collect();
    let pad = "\t".repeat(depth);
    let tag = &e.tag;
    if !e.kids.is_empty() {
        lines.push(format!("{pad}<{tag}{a}>"));
        for k in &e.kids {
            ser(k, depth + 1, lines);
        }
        lines.push(format!("{pad}</{tag}>"));
    } else {
        match text {
            None | Some("") => lines.push(format!("{pad}<{tag}{a}/>")),
            Some(t) => lines.push(format!("{pad}<{tag}{a}>{}</{tag}>", esc_text(t))),
        }
    }
}

pub(crate) fn from_json(v: JsonRef<'_>) -> Option<E> {
    let fields = v.as_array()?;
    if fields.len() != 4 {
        return None;
    }
    let tag = fields.first()?;
    let attrs = fields.get(1)?;
    let text = fields.get(2)?;
    let kids = fields.get(3)?;
    let attrs = attrs
        .as_object()?
        .iter()
        .map(|(k, v)| Some((k.to_string(), v.as_str()?.to_string())))
        .collect::<Option<Vec<_>>>()?;
    let text = if text.is_null() {
        None
    } else {
        Some(text.as_str()?.to_string())
    };
    let kids = kids
        .as_array()?
        .iter()
        .map(from_json)
        .collect::<Option<Vec<_>>>()?;
    Some(E::new(tag.as_str()?, text, attrs, kids))
}
