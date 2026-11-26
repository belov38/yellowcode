mod tabular;

use roxmltree::{Document, Node};

use crate::value::{
    Array, Date, Decimal, EnumRef, Map, NS_CONFIG, NS_CORE, NS_XS, Ref, Structure, TypeName, Uuid,
    Value, ValueError, config_ref_class, fits_depth,
};

const NS_APP: &str = "http://v8.1c.ru/8.2/managed-application/core";
const NS_MODULES: &str = "http://v8.1c.ru/8.2/managed-application/modules";
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

#[derive(Debug, thiserror::Error)]
pub enum XmlError {
    #[error("XML parse error: {0}")]
    Parse(String),
    #[error("missing field {0} in login response")]
    MissingLoginField(&'static str),
    #[error("unsupported value type {0} in call result")]
    UnsupportedType(String),
    #[error("unsupported {0} in call result")]
    UnsupportedPart(String),
    #[error("bad collection in call result: {0}")]
    Shape(String),
    #[error("collections nested deeper than {MAX_DEPTH}")]
    TooDeep,
    #[error("bad value in call result: {0}")]
    Value(#[from] ValueError),
}

impl From<roxmltree::Error> for XmlError {
    fn from(e: roxmltree::Error) -> Self {
        XmlError::Parse(e.to_string())
    }
}

pub use crate::value::MAX_DEPTH;

#[derive(Debug, Clone, Default)]
pub struct LoginInfo {
    pub info_base_instance_id: Option<String>,
    pub info_base_alias: Option<String>,
    pub config: Option<String>,
    pub assembly_version: Option<String>,
    pub types: Option<String>,
    pub user: Option<String>,
    pub seance: Option<String>,
    pub seance_no: Option<String>,
    pub roles_id: Option<String>,
}

fn strip_bom(body: &[u8]) -> &[u8] {
    body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body)
}

fn text_by_local(doc: &Document, ns: &str, local: &str) -> Option<String> {
    doc.descendants()
        .find(|n| {
            n.is_element() && n.tag_name().name() == local && n.tag_name().namespace() == Some(ns)
        })
        .and_then(|n| n.text())
        .map(|s| s.to_string())
}

pub fn parse_login(body: &[u8]) -> Result<LoginInfo, XmlError> {
    let text = String::from_utf8_lossy(strip_bom(body));
    let doc = Document::parse(&text)?;
    let g = |local| text_by_local(&doc, NS_APP, local);
    let info = LoginInfo {
        info_base_instance_id: g("infoBaseInstanceID"),
        info_base_alias: g("infoBaseAlias"),
        config: g("config"),
        assembly_version: g("assemblyVersion"),
        types: g("types"),
        user: g("user"),
        seance: g("seance"),
        seance_no: g("seanceNo"),
        roles_id: g("rolesID"),
    };
    if info.config.is_none() {
        return Err(XmlError::MissingLoginField("config"));
    }
    Ok(info)
}

pub fn parse_server_modules(body: &[u8]) -> Result<Vec<(String, String)>, XmlError> {
    let text = String::from_utf8_lossy(strip_bom(body));
    let doc = Document::parse(&text)?;
    let mut out = Vec::new();
    for n in doc.descendants() {
        if n.is_element()
            && n.tag_name().name() == "server"
            && n.tag_name().namespace() == Some(NS_MODULES)
            && let (Some(code), Some(id)) = (n.attribute("code"), n.attribute("id"))
        {
            out.push((code.to_string(), id.to_string()));
        }
    }
    Ok(out)
}

pub fn parse_module_members(body: &[u8]) -> Result<Vec<String>, XmlError> {
    let text = String::from_utf8_lossy(strip_bom(body));
    let doc = Document::parse(&text)?;
    let mut out = Vec::new();
    for n in doc.descendants() {
        if n.is_element()
            && matches!(n.tag_name().name(), "func" | "proc")
            && n.tag_name().namespace() == Some(NS_MODULES)
            && let Some(name) = n.attribute("name")
        {
            out.push(name.to_string());
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallValues {
    pub ret: Option<Value>,
    pub params: Vec<Value>,
}

pub(crate) fn value_text(body: &[u8]) -> String {
    let raw = String::from_utf8_lossy(strip_bom(body));
    keep_carriage_returns(&raw).into_owned()
}

pub(crate) fn typed_value(n: Node, implied: Option<&TypeName>) -> Result<Value, XmlError> {
    value_in(n, 0, implied)
}

pub(crate) fn has_xsi_type(n: Node) -> bool {
    n.attribute((NS_XSI, "type")).is_some() || n.attribute((NS_XSI, "nil")).is_some()
}

pub fn parse_call_result(body: &[u8]) -> Result<CallValues, XmlError> {
    let text = value_text(body);
    let doc = Document::parse(&text)?;
    let mut out = CallValues::default();
    for n in doc.root_element().children() {
        if !n.is_element() || n.tag_name().namespace() != Some(NS_MODULES) {
            continue;
        }
        match n.tag_name().name() {
            "ret" if out.ret.is_some() => {
                return Err(XmlError::Parse(
                    "several m:ret in a call result".to_string(),
                ));
            }
            "ret" => out.ret = Some(value_of(n)?),
            "param" => out.params.push(value_of(n)?),
            _ => {}
        }
    }
    Ok(out)
}

fn keep_carriage_returns(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\r') {
        return std::borrow::Cow::Borrowed(text);
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len() + 16);
    let mut depth = 0usize;
    let mut i = 0;
    let copy_until = |out: &mut String, from: usize, pat: &str| -> usize {
        let end = text[from..]
            .find(pat)
            .map_or(text.len(), |k| from + k + pat.len());
        out.push_str(&text[from..end]);
        end
    };
    while i < b.len() {
        let rest = &text[i..];
        if rest.starts_with("<!--") {
            i = copy_until(&mut out, i, "-->");
        } else if rest.starts_with("<![CDATA[") {
            i = copy_until(&mut out, i, "]]>");
        } else if rest.starts_with("<?") {
            i = copy_until(&mut out, i, "?>");
        } else if rest.starts_with("<!") {
            i = copy_until(&mut out, i, ">");
        } else if b[i] == b'<' {
            let mut j = i + 1;
            let mut quote = None;
            while j < b.len() {
                match (quote, b[j]) {
                    (None, b'"' | b'\'') => quote = Some(b[j]),
                    (Some(q), c) if c == q => quote = None,
                    (None, b'>') => break,
                    _ => {}
                }
                j += 1;
            }
            let end = (j + 1).min(b.len());
            let tag = &text[i..end];
            if tag.starts_with("</") {
                depth = depth.saturating_sub(1);
            } else if !tag.ends_with("/>") {
                depth += 1;
            }
            out.push_str(tag);
            i = end;
        } else {
            let end = rest.find('<').map_or(text.len(), |k| i + k);
            let chunk = &text[i..end];
            if depth > 0 {
                out.push_str(&chunk.replace('\r', "&#13;"));
            } else {
                out.push_str(chunk);
            }
            i = end;
        }
    }
    std::borrow::Cow::Owned(out)
}

fn value_of(n: Node) -> Result<Value, XmlError> {
    value_at(n, 0)
}

fn value_at(n: Node, depth: usize) -> Result<Value, XmlError> {
    value_in(n, depth, None)
}

fn value_in(n: Node, depth: usize, implied: Option<&TypeName>) -> Result<Value, XmlError> {
    let local = n.tag_name().name();
    if let Some(a) = n
        .attributes()
        .find(|a| a.namespace() != Some(NS_XSI) || !matches!(a.name(), "type" | "nil"))
    {
        return Err(XmlError::Shape(format!(
            "attribute {} on <{local}>",
            a.name()
        )));
    }
    if is_nil(n) {
        let content = n
            .children()
            .any(|c| c.is_element() || (c.is_text() && !c.text().unwrap_or("").is_empty()));
        if content || n.attribute((NS_XSI, "type")).is_some() {
            return Err(XmlError::Shape(format!(
                "nil <{local}> with a type or content"
            )));
        }
        return Ok(Value::Undefined);
    }
    let ty = match n.attribute((NS_XSI, "type")) {
        Some(qname) => {
            resolve_qname(n, qname).ok_or_else(|| XmlError::UnsupportedType(qname.to_string()))?
        }
        None => implied
            .cloned()
            .ok_or_else(|| XmlError::UnsupportedType("(no xsi:type)".to_string()))?,
    };
    value_of_type(n, &ty, depth)
}

fn is_nil(n: Node) -> bool {
    matches!(n.attribute((NS_XSI, "nil")), Some("true" | "1"))
}

fn resolve_qname(n: Node, qname: &str) -> Option<TypeName> {
    let (prefix, local) = match qname.split_once(':') {
        Some((p, l)) => (Some(p), l),
        None => (None, qname),
    };
    if local.is_empty() || local.contains(':') {
        return None;
    }
    let ns = n.lookup_namespace_uri(prefix)?;
    Some(TypeName::new(ns, local))
}

fn leaf_text<'a>(n: Node<'a, '_>) -> Result<std::borrow::Cow<'a, str>, XmlError> {
    if n.children().any(|c| c.is_element()) {
        return Err(XmlError::Shape(format!(
            "an element inside <{}> of a simple type",
            n.tag_name().name()
        )));
    }
    let mut texts = n
        .children()
        .filter(|c| c.is_text())
        .map(|c| c.text().unwrap_or(""));
    Ok(match (texts.next(), texts.next()) {
        (None, _) => std::borrow::Cow::Borrowed(""),
        (Some(one), None) => std::borrow::Cow::Borrowed(one),
        (Some(a), Some(b)) => {
            let mut all = String::from(a);
            all.push_str(b);
            texts.for_each(|t| all.push_str(t));
            std::borrow::Cow::Owned(all)
        }
    })
}

fn value_of_type(n: Node, ty: &TypeName, depth: usize) -> Result<Value, XmlError> {
    let (ns, local) = (ty.ns(), ty.local());
    let unsupported = || XmlError::UnsupportedType(format!("{{{ns}}}{local}"));
    if ns == NS_CONFIG {
        let class = config_ref_class(ty).ok_or_else(unsupported)?;
        let text = leaf_text(n)?;
        return Ok(if class {
            Value::EnumRef(EnumRef::new(ty.clone(), &text)?)
        } else {
            Value::Ref(Ref::new(ty.clone(), Uuid::parse(&text)?)?)
        });
    }
    let collection = matches!(
        (ns, local),
        (
            NS_CORE,
            "Array"
                | "FixedArray"
                | "Structure"
                | "FixedStructure"
                | "Map"
                | "FixedMap"
                | "ValueTable"
                | "ValueTree"
                | "ValueListType"
        )
    );
    if collection {
        if depth >= MAX_DEPTH {
            return Err(XmlError::TooDeep);
        }
        return match local {
            "ValueTable" => tabular::table_of(n, depth + 1).map(Value::ValueTable),
            "ValueTree" => tabular::tree_of(n, depth + 1).map(Value::ValueTree),
            "ValueListType" => tabular::list_of(n, depth + 1).map(Value::ValueList),
            _ => collection_of(n, local, depth + 1),
        };
    }
    let primitive = matches!(
        (ns, local),
        (NS_XS, "string" | "decimal" | "boolean" | "dateTime") | (NS_CORE, "UUID" | "Null")
    );
    if !primitive {
        return Err(unsupported());
    }
    let text = leaf_text(n)?;
    let text = text.as_ref();
    Ok(match (ns, local) {
        (NS_XS, "string") => Value::String(text.to_string()),
        (NS_XS, "decimal") => Value::Number(Decimal::parse(text)?),
        (NS_XS, "boolean") => match text {
            "true" | "1" => Value::Bool(true),
            "false" | "0" => Value::Bool(false),
            _ => return Err(ValueError::Bool(text.to_string()).into()),
        },
        (NS_XS, "dateTime") => Value::Date(Date::parse_xsd(text)?),
        (NS_CORE, "UUID") => Value::Uuid(Uuid::parse(text)?),
        (NS_CORE, "Null") => Value::Null,
        _ => return Err(unsupported()),
    })
}

fn elements<'a, 'i>(n: Node<'a, 'i>) -> Result<Vec<Node<'a, 'i>>, XmlError> {
    let mut out = Vec::new();
    for c in n.children() {
        if c.is_element() {
            out.push(c);
        } else if c.is_text() && !c.text().unwrap_or("").trim().is_empty() {
            return Err(XmlError::Shape(format!(
                "text inside <{}>",
                n.tag_name().name()
            )));
        }
    }
    Ok(out)
}

fn expect_core(n: Node, local: &str) -> Result<(), XmlError> {
    if n.tag_name().namespace() == Some(NS_CORE) && n.tag_name().name() == local {
        return Ok(());
    }
    Err(XmlError::Shape(format!(
        "expected core <{local}>, found <{}>{}",
        n.tag_name().name(),
        n.tag_name()
            .namespace()
            .map(|ns| format!(" in {ns}"))
            .unwrap_or_default()
    )))
}

fn single_core<'a, 'i>(n: Node<'a, 'i>, local: &str) -> Result<Node<'a, 'i>, XmlError> {
    match elements(n)?.as_slice() {
        [c] => {
            expect_core(*c, local)?;
            Ok(*c)
        }
        other => Err(XmlError::Shape(format!(
            "<{}> holds {} elements, expected one <{local}>",
            n.tag_name().name(),
            other.len()
        ))),
    }
}

fn collection_of(n: Node, local: &str, depth: usize) -> Result<Value, XmlError> {
    let fixed = local.starts_with("Fixed");
    let children = elements(n)?;
    Ok(match local {
        "Array" | "FixedArray" => {
            let mut items = Vec::with_capacity(children.len());
            for c in children {
                expect_core(c, "Value")?;
                items.push(value_at(c, depth)?);
            }
            Value::Array(Array { fixed, items })
        }
        "Structure" | "FixedStructure" => {
            let mut st = if fixed {
                Structure::new_fixed()
            } else {
                Structure::new()
            };
            for c in children {
                expect_core(c, "Property")?;
                let Some(name) = c.attribute("name") else {
                    return Err(XmlError::Shape("<Property> without name".to_string()));
                };
                let v = value_at(single_core(c, "Value")?, depth)?;
                if st.insert(name, v)?.is_some() {
                    return Err(XmlError::Shape(format!("repeated structure key {name:?}")));
                }
            }
            Value::Structure(st)
        }
        _ => {
            let mut m = if fixed { Map::new_fixed() } else { Map::new() };
            for c in children {
                expect_core(c, "pair")?;
                let [k, v] = elements(c)?[..] else {
                    return Err(XmlError::Shape(
                        "<pair> must hold <Key> and <Value>".to_string(),
                    ));
                };
                expect_core(k, "Key")?;
                expect_core(v, "Value")?;
                let key = value_at(k, depth)?;
                let shown = format!("{key:?}");
                if m.insert(key, value_at(v, depth)?)?.is_some() {
                    return Err(XmlError::Shape(format!("repeated map key {shown}")));
                }
            }
            Value::Map(m)
        }
    })
}

pub fn parse_exception(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(strip_bom(body));
    let doc = match Document::parse(&text) {
        Ok(d) => d,
        Err(_) => return text.chars().take(200).collect(),
    };
    let reason = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "exception")
        .and_then(|n| n.attribute("reason"))
        .unwrap_or("");
    let descr = text_by_local(&doc, NS_CORE, "descr").unwrap_or_default();
    if reason.is_empty() {
        return descr.trim().to_string();
    }
    format!("{reason}: {descr}").trim().to_string()
}

pub fn build_call_request(name: &str, params: &[Value]) -> Result<Vec<u8>, XmlError> {
    if !params.iter().all(fits_depth) {
        return Err(XmlError::TooDeep);
    }
    let mut r = String::from(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<request xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\" \
xmlns:cfg=\"http://v8.1c.ru/8.1/data/enterprise/current-config\" \
xmlns:m=\"http://v8.1c.ru/8.2/managed-application/modules\" \
xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" \
xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" ",
    );
    r.push_str(&format!("name=\"{}\"", escape_attr(name)));
    if params.is_empty() {
        r.push_str("/>");
    } else {
        r.push('>');
        for p in params {
            push_param(&mut r, p);
        }
        r.push_str("</request>");
    }
    Ok(r.into_bytes())
}

pub(crate) fn params_xml(params: &[Value]) -> Result<String, XmlError> {
    if !params.iter().all(fits_depth) {
        return Err(XmlError::TooDeep);
    }
    let mut r = String::new();
    for p in params {
        push_param(&mut r, p);
    }
    Ok(r)
}

fn push_param(r: &mut String, v: &Value) {
    push_value(r, "m:param", v);
}

fn push_value(r: &mut String, elem: &str, v: &Value) {
    push_value_as(r, elem, v, None);
}

fn push_value_as(r: &mut String, elem: &str, v: &Value, implied: Option<&TypeName>) {
    let Some(ty) = v.type_name() else {
        r.push_str(&format!("<{elem} xsi:nil=\"true\"/>"));
        return;
    };
    let attr = if implied == Some(&ty) {
        String::new()
    } else {
        let (decl, qname) = qname_of(&ty, "d3p1");
        format!("{decl} xsi:type=\"{qname}\"")
    };
    let text = v.lexical().unwrap_or_default();
    let empty = match v {
        Value::Array(a) => a.items.is_empty(),
        Value::Structure(st) => st.is_empty(),
        Value::Map(m) => m.is_empty(),
        Value::ValueTable(t) => t.is_empty(),
        Value::ValueTree(t) => t.is_empty(),
        Value::ValueList(_) => false,
        _ => text.is_empty(),
    };
    if empty {
        r.push_str(&format!("<{elem}{attr}/>"));
        return;
    }
    r.push_str(&format!("<{elem}{attr}>"));
    match v {
        Value::Array(a) => {
            for item in &a.items {
                push_value(r, "v8:Value", item);
            }
        }
        Value::Structure(st) => {
            for (key, item) in st.props() {
                r.push_str(&format!("<v8:Property name=\"{}\">", escape_attr(key)));
                push_value(r, "v8:Value", item);
                r.push_str("</v8:Property>");
            }
        }
        Value::Map(m) => {
            for (key, item) in m.pairs() {
                r.push_str("<v8:pair>");
                push_value(r, "v8:Key", key);
                push_value(r, "v8:Value", item);
                r.push_str("</v8:pair>");
            }
        }
        Value::ValueTable(t) => tabular::push_table(r, t),
        Value::ValueTree(t) => tabular::push_tree(r, t),
        Value::ValueList(l) => tabular::push_list(r, l),
        _ => r.push_str(&escape_text(&text)),
    }
    r.push_str(&format!("</{elem}>"));
}

fn qname_of(ty: &TypeName, prefix: &str) -> (String, String) {
    match ty.known_prefix() {
        Some(p) => (String::new(), format!("{p}:{}", ty.local())),
        None => (
            format!(" xmlns:{prefix}=\"{}\"", escape_attr(ty.ns())),
            format!("{prefix}:{}", ty.local()),
        ),
    }
}

pub(crate) fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\r', "&#13;")
}

pub(crate) fn escape_attr(s: &str) -> String {
    escape_text(s)
        .replace('"', "&quot;")
        .replace('\n', "&#10;")
        .replace('\t', "&#9;")
}
