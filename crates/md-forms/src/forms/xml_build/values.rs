use super::builder::{Builder, count, list, parse};
use super::{Result, invalid, xml};
use crate::forms::fe::E;
use brace::Node;
use mdschema::forms::Value;

pub(super) fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}
pub(super) fn xml_text(s: &str) -> String {
    s.replace('\n', "\r\n")
}
pub(super) fn boolean(s: &str) -> Result<Node> {
    match s {
        "true" => Ok(Node::int(1)),
        "false" => Ok(Node::int(0)),
        _ => Err(invalid("boolean", "expected true or false")),
    }
}
pub(super) fn lstr(e: Option<&E>) -> Result<Node> {
    let mut out = vec![Node::int(1), count(e.map_or(0, |e| e.kids.len()))];
    if let Some(e) = e {
        for item in &e.kids {
            if item.tag != "v8:item" {
                return Err(invalid(&e.tag, "expected v8:item"));
            }
            xml::unique(item)?;
            if item
                .kids
                .iter()
                .any(|e| !matches!(e.tag.as_str(), "v8:lang" | "v8:content"))
            {
                return Err(invalid(&e.tag, "unknown local string field"));
            }
            out.push(list(vec![
                Node::str(xml::ctext(item, "v8:lang", "")),
                Node::str(xml_text(xml::ctext(item, "v8:content", ""))),
            ]));
        }
    }
    Ok(list(out))
}
pub(super) fn base64(bytes: &[u8]) -> Node {
    brace::base64_atom(bytes, brace::Newline::CrLf, false)
}
pub(super) fn same(a: &E, b: &E) -> bool {
    a.tag == b.tag
        && xml::text(a) == xml::text(b)
        && a.attrs.len() == b.attrs.len()
        && a.attrs.iter().all(|v| b.attrs.contains(v))
        && a.kids.len() == b.kids.len()
        && a.kids.iter().zip(&b.kids).all(|(a, b)| same(a, b))
}

impl Builder<'_> {
    pub fn metadata(&self, name: &str) -> Result<Node> {
        self.resolution
            .metadata_id(name)
            .map(Node::guid)
            .ok_or_else(|| invalid("metadata", format!("unresolved identity {name}")))
    }
    pub fn roles(&self, el: Option<&E>) -> Result<Node> {
        let mut common = Node::int(1);
        let mut values = Vec::new();
        if let Some(e) = el {
            for c in &e.kids {
                match c.tag.as_str() {
                    "xr:Common" => common = boolean(xml::text(c))?,
                    "xr:Value" => {
                        values.push(
                            self.metadata(
                                xml::attr(c, "name")
                                    .ok_or_else(|| invalid(&e.tag, "role name missing"))?,
                            )?,
                        );
                        values.push(list(vec![Node::str("B"), boolean(xml::text(c))?]));
                    }
                    _ => return Err(invalid(&e.tag, "unknown role value")),
                }
            }
        }
        let mut body = vec![
            Node::int(0),
            list(vec![Node::str("B"), common]),
            count(values.len() / 2),
        ];
        body.extend(values);
        Ok(list(vec![Node::int(0), list(body)]))
    }
    pub fn functional_options(&self, el: Option<&E>) -> Result<Node> {
        let mut out = vec![Node::int(0), count(el.map_or(0, |e| e.kids.len()))];
        if let Some(e) = el {
            for c in &e.kids {
                out.push(self.metadata(xml::text(c))?);
            }
        }
        Ok(list(out))
    }
    pub fn pattern(&self, el: Option<&E>) -> Result<Node> {
        let mut out = vec![Node::str("Pattern")];
        let Some(e) = el else {
            return Ok(list(out));
        };
        for c in &e.kids {
            if !matches!(c.tag.as_str(), "v8:Type" | "v8:TypeSet") {
                if matches!(
                    c.tag.as_str(),
                    "v8:NumberQualifiers"
                        | "v8:StringQualifiers"
                        | "v8:DateQualifiers"
                        | "v8:BinaryDataQualifiers"
                ) {
                    continue;
                }
                return Err(invalid("Type", format!("unknown type field {}", c.tag)));
            }
            let name = xml::text(c).trim();
            let simple = match name {
                "xs:boolean" => Some("B"),
                "xs:string" => Some("S"),
                "xs:decimal" => Some("N"),
                "xs:dateTime" => Some("D"),
                "xs:base64Binary" => Some("R"),
                "v8:Null" => Some("L"),
                _ => None,
            };
            if simple.is_some() && c.tag == "v8:TypeSet" {
                return Err(invalid("Type", "primitive type cannot be a TypeSet"));
            }
            let mut t = if let Some(code) = simple {
                vec![Node::str(code)]
            } else if let Some(id) = name
                .strip_prefix("cfg:")
                .and_then(|name| self.resolution.type_id(name))
            {
                vec![Node::str("#"), Node::guid(id)]
            } else {
                let platform = self
                    .types
                    .as_object()
                    .and_then(|types| {
                        types
                            .iter()
                            .find(|(_, v)| v.get(1).and_then(Value::as_str) == Some(name))
                            .map(|(id, _)| id.to_owned())
                    })
                    .or_else(|| {
                        self.tables
                            .types
                            .as_object()?
                            .iter()
                            .find(|(_, v)| v.get(1).and_then(Value::as_str) == Some(name))
                            .map(|(id, _)| id.clone())
                    })
                    .ok_or_else(|| invalid("Type", format!("unknown platform type {name}")))?;
                vec![Node::str("#"), Node::atom(platform)]
            };
            match simple {
                Some("N") => {
                    let q = xml::child(e, "v8:NumberQualifiers");
                    for (tag, default) in [("v8:Digits", "0"), ("v8:FractionDigits", "0")] {
                        let value = q.map_or(default, |q| xml::ctext(q, tag, default));
                        let n: u32 = value
                            .parse()
                            .map_err(|_| invalid("Type", "invalid number qualifier"))?;
                        t.push(Node::int(i64::from(n)));
                    }
                    t.push(Node::int(i64::from(q.is_some_and(|q| {
                        xml::ctext(q, "v8:AllowedSign", "Any") == "Nonnegative"
                    }))));
                }
                Some("S" | "R") => {
                    if let Some(q) = xml::child(
                        e,
                        if simple == Some("S") {
                            "v8:StringQualifiers"
                        } else {
                            "v8:BinaryDataQualifiers"
                        },
                    ) {
                        let len: u32 = xml::ctext(q, "v8:Length", "0")
                            .parse()
                            .map_err(|_| invalid("Type", "invalid length"))?;
                        let variable = match xml::ctext(q, "v8:AllowedLength", "Variable") {
                            "Fixed" => false,
                            "Variable" => true,
                            _ => return Err(invalid("Type", "unknown allowed length")),
                        };
                        if len != 0 || !variable {
                            t.extend([Node::int(i64::from(len)), Node::int(i64::from(variable))]);
                        }
                    }
                }
                Some("D") => {
                    if let Some(q) = xml::child(e, "v8:DateQualifiers") {
                        match xml::ctext(q, "v8:DateFractions", "DateTime") {
                            "DateTime" => {}
                            "Date" => t.push(Node::str("D")),
                            "Time" => t.push(Node::str("T")),
                            _ => return Err(invalid("Type", "unknown date fractions")),
                        }
                    }
                }
                _ => {}
            }
            out.push(list(t));
        }
        Ok(list(out))
    }
    pub fn picture(&self, el: Option<&E>, item: Option<&str>) -> Result<Node> {
        let Some(e) = el else {
            return parse("{4,0,{0},\"\",-1,-1,1,0,\"\"}");
        };
        xml::unique(e)?;
        if e.kids.iter().any(|c| {
            !matches!(
                c.tag.as_str(),
                "xr:Ref" | "xr:Abs" | "xr:LoadTransparent" | "xr:TransparentPixel"
            )
        }) {
            return Err(invalid("Picture", "unknown picture field"));
        }
        let lt = boolean(xml::ctext(e, "xr:LoadTransparent", "true"))?;
        let pixel = xml::child(e, "xr:TransparentPixel");
        let coord = |key| -> Result<Node> {
            let n: i32 = pixel
                .and_then(|e| xml::attr(e, key))
                .unwrap_or("-1")
                .parse()
                .map_err(|_| invalid("Picture", "invalid pixel"))?;
            Ok(Node::int(i64::from(n)))
        };
        let mut result = vec![
            Node::int(4),
            Node::int(0),
            list(vec![Node::int(0)]),
            Node::str(""),
            coord("x")?,
            coord("y")?,
            lt,
            Node::int(0),
            Node::str(""),
        ];
        match (xml::child(e, "xr:Ref"), xml::child(e, "xr:Abs")) {
            (Some(r), None) => {
                let reference = xml::text(r);
                let id = self
                    .resolution
                    .metadata_id(reference)
                    .map(|id| list(vec![Node::int(0), Node::guid(id)]))
                    .or_else(|| {
                        let name = reference.strip_prefix("StdPicture.")?;
                        let key =
                            super::ui::resource_key("std_pictures.json", "std_pictures", name)
                                .or_else(|| {
                                    super::ui::resource_key("ui.json", "std_pictures", name)
                                })
                                .or_else(|| {
                                    self.tables
                                        .misc
                                        .get("pict")?
                                        .as_object()?
                                        .iter()
                                        .find(|(_, v)| v.as_str() == Some(name))
                                        .map(|(k, _)| k.as_str())
                                })?;
                        Some(if key.parse::<i64>().is_ok() {
                            list(vec![Node::atom(key)])
                        } else {
                            list(vec![Node::int(0), Node::atom(key)])
                        })
                    })
                    .ok_or_else(|| invalid("Picture", "unresolved picture reference"))?;
                super::builder::set(&mut result, &[1], Node::int(1))?;
                super::builder::set(&mut result, &[2], id)?;
            }
            (None, Some(a)) => {
                let file = xml::text(a);
                if file.is_empty() || file.contains(['/', '\\', ':']) || matches!(file, "." | "..")
                {
                    return Err(invalid("Picture", "unsafe picture filename"));
                }
                let path =
                    item.map_or_else(|| file.to_owned(), |item| format!("Items/{item}/{file}"));
                let asset_index = self
                    .asset_index
                    .get(&path.nfc().collect::<String>())
                    .copied()
                    .ok_or_else(|| invalid("Picture", format!("missing asset {path}")))?;
                let (_, bytes) = self
                    .input
                    .files
                    .get(asset_index)
                    .ok_or_else(|| invalid("Picture", "asset index"))?;
                self.retain_asset_allocation(bytes.len())?;
                self.asset_uses.borrow_mut().insert(asset_index);
                super::builder::set(&mut result, &[1], Node::int(3))?;
                super::builder::set(&mut result, &[7], list(vec![list(vec![base64(bytes)])]))?;
            }
            _ => return Err(invalid("Picture", "expected exactly one Ref or Abs")),
        }
        Ok(list(result))
    }
    pub fn data_path(&self, value: &str) -> Result<Node> {
        self.data_path_at(value, 0)
    }
    fn data_path_at(&self, value: &str, depth: usize) -> Result<Node> {
        self.check()?;
        if let Some(path) = self.with_form_info(|info| info.unresolve(value))?.flatten() {
            return Ok(path);
        }
        if depth > 64 {
            return Err(invalid("DataPath", "cyclic table path"));
        }
        let value = value.strip_prefix('~').unwrap_or(value);
        if value.is_empty() {
            return Ok(list(vec![Node::int(0)]));
        }
        let parts: Vec<_> = value.split('.').collect();
        if parts.first() == Some(&"Items") {
            let item = parts
                .get(1)
                .and_then(|name| self.items.get(*name))
                .ok_or_else(|| invalid("DataPath", "unknown item"))?;
            if parts.get(2) != Some(&"CurrentData") {
                return Err(invalid("DataPath", "unknown item property"));
            }
            let base = xml::ctext(item, "DataPath", "");
            let base_path = self.data_path_at(base, depth + 1)?;
            let base_len = base_path
                .as_list()
                .ok_or_else(|| invalid("DataPath", "invalid table base"))?
                .len();
            let mut full = base.to_owned();
            for part in parts.iter().skip(3) {
                full.push('.');
                full.push_str(part);
            }
            let mut path = self.data_path_at(&full, depth + 1)?;
            let segments = path
                .as_list_mut()
                .ok_or_else(|| invalid("DataPath", "bad table path"))?;
            if segments.len() < base_len || base_len < 2 {
                return Err(invalid("DataPath", "empty table path"));
            }
            let tail = segments.split_off(base_len);
            let mut out = vec![
                count(tail.len() + 1),
                list(vec![
                    Node::atom(xml::attr(item, "id").unwrap_or_default()),
                    Node::atom(crate::forms::decode::FORMID),
                ]),
            ];
            out.extend(tail);
            return Ok(list(out));
        }
        let attr = self
            .attrs
            .iter()
            .find(|a| xml::attr(a, "name") == parts.first().copied())
            .ok_or_else(|| invalid("DataPath", format!("unknown attribute {value}")))?;
        let mut out = vec![
            Node::int(0),
            list(vec![Node::atom(xml::attr(attr, "id").unwrap_or_default())]),
        ];
        if let Some(fields) = xml::attr(attr, "name").and_then(|name| self.dynamic_fields.get(name))
        {
            if parts.len() > 1 {
                let name = parts.get(1..).unwrap_or_default().join(".");
                let id = if let Some(i) = fields.iter().position(|f| f == &name) {
                    (i + 1).to_string()
                } else if let Some((id, _)) = crate::forms::ctx::DL_SPECIAL
                    .iter()
                    .find(|(_, n)| *n == name)
                {
                    (*id).to_owned()
                } else {
                    self.tables
                        .paths
                        .get("std")
                        .and_then(Value::as_object)
                        .and_then(|m| {
                            m.iter().find(|(k, v)| {
                                k.starts_with("cfg:DynamicList|") && v.as_str() == Some(&name)
                            })
                        })
                        .and_then(|(k, _)| k.split_once('|').map(|(_, id)| id.to_owned()))
                        .ok_or_else(|| invalid("DataPath", "unresolved dynamic list property"))?
                };
                out.push(list(vec![Node::atom(id)]));
            }
            let len = out.len() - 1;
            result_count(&mut out, len)?;
            return Ok(list(out));
        }
        let mut current = *attr;
        let mut metadata = xml::child(attr, "Type")
            .and_then(|e| e.kids.iter().find(|e| e.tag == "v8:Type"))
            .and_then(|e| xml::text(e).strip_prefix("cfg:"))
            .map(|t| {
                let (kind, name) = t.split_once('.').unwrap_or((t, ""));
                let kind = kind
                    .strip_suffix("Object")
                    .or_else(|| kind.strip_suffix("Ref"))
                    .unwrap_or(kind);
                format!("{kind}.{name}")
            });
        for part in parts.iter().skip(1) {
            if let Some(col) = xml::child(current, "Columns").and_then(|cols| {
                cols.kids
                    .iter()
                    .find(|c| xml::attr(c, "name") == Some(*part))
            }) {
                out.push(list(vec![Node::atom(
                    xml::attr(col, "id").unwrap_or_default(),
                )]));
                current = col;
                metadata = None;
                continue;
            }
            if let Some(owner) = metadata.as_ref() {
                let mut found = None;
                for kind in ["Attribute", "TabularSection", "Dimension", "Resource"] {
                    let path = format!("{owner}.{kind}.{part}");
                    if let Some(id) = self.resolution.metadata_id(&path) {
                        found = Some((path, id));
                        break;
                    }
                }
                if let Some((path, id)) = found {
                    out.push(list(vec![Node::int(0), Node::guid(id)]));
                    metadata = Some(path);
                    continue;
                }
            }
            return Err(invalid("DataPath", format!("unresolved path {value}")));
        }
        let len = out.len() - 1;
        result_count(&mut out, len)?;
        Ok(list(out))
    }
}
fn result_count(out: &mut [Node], n: usize) -> Result<()> {
    *out.first_mut()
        .ok_or_else(|| invalid("DataPath", "missing count"))? = count(n);
    Ok(())
}
use unicode_normalization::UnicodeNormalization;
