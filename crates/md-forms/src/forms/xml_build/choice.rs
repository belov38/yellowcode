use super::builder::{Builder, count, list, parse, set};
use super::properties::path_value;
use super::values::{boolean, crlf, lstr};
use super::{Result, invalid, xml};
use crate::forms::{decode::Item, fe::E};
use brace::Node;

fn child<'a>(e: &'a E, name: &str) -> Option<&'a E> {
    e.kids.iter().find(|c| {
        c.tag == name
            || c.tag.strip_prefix("xr:") == Some(name)
            || c.tag.strip_prefix("app:") == Some(name)
    })
}
fn ctext<'a>(e: &'a E, name: &str, default: &'a str) -> &'a str {
    child(e, name).map_or(default, xml::text)
}
const DESTIME: &str = "0e704aa2-07bd-48b9-8223-a0212c4d5fc2";
const FIXED_ARRAY: &str = "4500381b-db30-4a10-9db4-990038032acf";
const DTREF: &str = "5c14e26f-099b-4d37-84a6-b433d87400da";
const ZERO: &str = "00000000-0000-0000-0000-000000000000";
impl Builder<'_> {
    pub(super) fn numeric_bound(&self, e: &E) -> Result<Node> {
        if !e.kids.is_empty()
            || e.attrs.len() != 1
            || xml::attr(e, "xsi:type") != Some("xs:decimal")
        {
            return Err(invalid(&e.tag, "expected scalar XMLSchema decimal"));
        }
        self.typed_value(e)
    }
    fn design_ref(&self, value: &str) -> Result<Node> {
        if value.is_empty() {
            return Ok(list(vec![Node::int(0), Node::atom(ZERO), Node::atom(ZERO)]));
        }
        let mut parts = value.split('.');
        let kind = parts
            .next()
            .ok_or_else(|| invalid("Value", "missing reference kind"))?;
        let object = parts
            .next()
            .ok_or_else(|| invalid("Value", "missing reference object"))?;
        let ty = self
            .resolution
            .type_id(&format!("{kind}Ref.{object}"))
            .ok_or_else(|| invalid("Value", "unresolved reference type"))?;
        let value = if value.ends_with(".EmptyRef") {
            Node::atom(ZERO)
        } else {
            self.resolution
                .metadata_id(value)
                .or_else(|| {
                    value
                        .rsplit('.')
                        .next()
                        .and_then(|name| self.resolution.predefined_id(name))
                })
                .map(Node::guid)
                .ok_or_else(|| invalid("Value", "unresolved reference value"))?
        };
        Ok(list(vec![Node::int(0), Node::guid(ty), value]))
    }
    fn typed_value(&self, e: &E) -> Result<Node> {
        self.check()?;
        if xml::attr(e, "xsi:nil") == Some("true") {
            if !e.kids.is_empty() || !xml::text(e).is_empty() {
                return Err(invalid("Value", "nil value contains content"));
            }
            return Ok(list(vec![Node::str("U")]));
        }
        let ty = xml::attr(e, "xsi:type").ok_or_else(|| invalid("Value", "missing xsi:type"))?;
        let value = xml::text(e);
        match ty {
            "xs:string" => Ok(list(vec![Node::str("S"), Node::str(crlf(value))])),
            "xs:boolean" => Ok(list(vec![Node::str("B"), boolean(value)?])),
            "xs:decimal" => {
                let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
                let mut digit = false;
                let mut dot = false;
                for (index, byte) in digits.bytes().enumerate() {
                    if index % 4096 == 0 {
                        self.check()?;
                    }
                    if byte.is_ascii_digit() {
                        digit = true;
                    } else if byte == b'.' && !dot {
                        dot = true;
                    } else {
                        return Err(invalid("Value", "invalid decimal"));
                    }
                }
                if !digit {
                    return Err(invalid("Value", "invalid decimal"));
                }
                Ok(list(vec![Node::str("N"), Node::atom(value)]))
            }
            "xs:dateTime" => Ok(list(vec![Node::str("D"), Node::atom(date(value)?)])),
            _ if ty.ends_with("DesignTimeRef") => Ok(list(vec![
                Node::str("#"),
                Node::atom(DTREF),
                self.design_ref(value)?,
            ])),
            _ if ty.ends_with("FixedArray") => {
                let mut values = vec![count(e.kids.len())];
                for c in &e.kids {
                    values.push(self.typed_value(c)?);
                }
                Ok(list(vec![
                    Node::str("#"),
                    Node::atom(FIXED_ARRAY),
                    list(values),
                ]))
            }
            _ if ty.ends_with("FormChoiceListDesTimeValue") => Ok(list(vec![
                Node::str("#"),
                Node::atom(DESTIME),
                self.des_time(e)?,
            ])),
            _ => Err(invalid("Value", "unsupported typed value")),
        }
    }
    fn des_time(&self, e: &E) -> Result<Node> {
        xml::unique(e)?;
        let value = child(e, "Value").ok_or_else(|| invalid("ChoiceList", "missing Value"))?;
        let presentation = lstr(child(e, "Presentation"))?;
        let picture = self.picture(child(e, "Picture"), None)?;
        if xml::attr(value, "xsi:type").is_some_and(|t| t.ends_with("DesignTimeRef")) {
            let reference = self.design_ref(xml::text(value))?;
            return Ok(list(vec![
                Node::int(1),
                Node::int(0),
                list(vec![Node::str("U")]),
                reference
                    .at(1)
                    .cloned()
                    .ok_or_else(|| invalid("Value", "reference type"))?,
                reference
                    .at(2)
                    .cloned()
                    .ok_or_else(|| invalid("Value", "reference value"))?,
                presentation,
                picture,
            ]));
        }
        Ok(list(vec![
            Node::int(1),
            Node::int(1),
            self.typed_value(value)?,
            Node::atom(ZERO),
            Node::atom(ZERO),
            presentation,
            picture,
        ]))
    }
    pub fn choice_property(&self, item: &mut Item, e: &E, prop: &str) -> Result<bool> {
        let el = child(e, prop);
        match prop {
            "ChoiceList" => {
                let mut pairs = Vec::new();
                let mut states = Vec::new();
                if let Some(el) = el {
                    for entry in &el.kids {
                        let value = child(entry, "Value")
                            .ok_or_else(|| invalid(prop, "missing choice Value"))?;
                        pairs.extend([
                            Node::str(ctext(entry, "Presentation", "")),
                            list(vec![
                                Node::str("#"),
                                Node::atom(DESTIME),
                                self.des_time(value)?,
                            ]),
                        ]);
                        let state: i64 = ctext(entry, "CheckState", "0")
                            .parse()
                            .map_err(|_| invalid(prop, "invalid check state"))?;
                        states.push(list(vec![Node::int(state), self.picture(None, None)?]));
                    }
                }
                let mut out = vec![Node::int(3), count(states.len())];
                out.extend(pairs);
                out.extend(states);
                set(&mut item.body, &[39, 1], list(out))?;
            }
            "ChoiceParameters" => {
                let mut out = vec![Node::int(0), count(el.map_or(0, |e| e.kids.len()))];
                if let Some(el) = el {
                    for entry in &el.kids {
                        out.push(Node::str(
                            xml::attr(entry, "name")
                                .ok_or_else(|| invalid(prop, "missing parameter name"))?,
                        ));
                        out.push(
                            self.typed_value(
                                child(entry, "app:value")
                                    .or_else(|| child(entry, "value"))
                                    .ok_or_else(|| invalid(prop, "missing parameter value"))?,
                            )?,
                        );
                    }
                }
                set(&mut item.body, &[39, 27], list(out))?;
            }
            "ChoiceParameterLinks" => {
                let n = el.map_or(0, |e| e.kids.len());
                let mut old = vec![Node::int(5006), count(n)];
                let mut new = vec![Node::int(5007), count(n)];
                if let Some(el) = el {
                    for link in &el.kids {
                        let path = self.data_path(ctext(link, "DataPath", ""))?;
                        let mode = match ctext(link, "ValueChange", "Clear") {
                            "Clear" => 0,
                            "DontChange" => 1,
                            _ => return Err(invalid(prop, "unknown value change mode")),
                        };
                        let mut row = vec![Node::str(ctext(link, "Name", ""))];
                        row.extend(
                            path.as_list()
                                .ok_or_else(|| invalid(prop, "bad path"))?
                                .iter()
                                .cloned(),
                        );
                        row.push(Node::int(mode));
                        old.extend(row.iter().cloned());
                        new.extend(row);
                        new.extend([Node::str(""), Node::str("")]);
                    }
                }
                set(&mut item.body, &[39, 26], list(old))?;
                set(&mut item.body, &[39, 64], list(new))?;
            }
            "TypeLink" => {
                let mut out = vec![Node::int(3)];
                let path = self.data_path(el.map_or("", |e| ctext(e, "DataPath", "")))?;
                out.extend(
                    path.as_list()
                        .ok_or_else(|| invalid(prop, "bad type path"))?
                        .iter()
                        .cloned(),
                );
                let link: i64 = el
                    .map_or("0", |e| ctext(e, "LinkItem", "0"))
                    .parse()
                    .map_err(|_| invalid(prop, "invalid LinkItem"))?;
                out.push(Node::int(link));
                set(&mut item.body, &[39, 42], list(out))?;
            }
            "UserSettingsGroup" => {
                let id = el
                    .map(|e| {
                        self.item_ids
                            .get(xml::text(e))
                            .ok_or_else(|| invalid(prop, "unknown group item"))
                    })
                    .transpose()?;
                if el.is_some() || item.bag.iter().any(|(k, _)| k == "16") {
                    let value = list(vec![
                        Node::str("N"),
                        id.map_or_else(|| Node::int(0), Node::atom),
                    ]);
                    if let Some((_, v)) = item.bag.iter_mut().find(|(k, _)| k == "16") {
                        *v = value;
                    } else {
                        item.bag.push(("16".into(), value));
                    }
                }
            }
            "ShowCommandBar" if item.class == Some("Table") => {
                set(&mut item.body, &[110], Node::int(i64::from(el.is_some())))?;
                if let Some(el) = el {
                    let code = match xml::text(el) {
                        "false" => 0,
                        "true" => 1,
                        "auto" => 2,
                        _ => return Err(invalid(prop, "unknown command bar mode")),
                    };
                    set(&mut item.body, &[111], Node::int(code))?;
                }
            }
            "ShowCheckBoxesInDropList" | "MultipleValueDataPath" => {
                if el.is_none() {
                    return Ok(true);
                }
                let current = path_value(&item.body, &[39, 62]).and_then(Node::as_list);
                let mut value = if current
                    .is_some_and(|v| v.len() > 9 && v.first().and_then(Node::as_i64) == Some(1))
                {
                    list(current.unwrap_or_default().to_vec())
                } else {
                    parse(
                        self.data
                            .get("multi_values")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| invalid(prop, "missing multi-value constructor"))?,
                    )?
                };
                let body = value
                    .as_list_mut()
                    .ok_or_else(|| invalid(prop, "invalid multi-value constructor"))?;
                if prop == "ShowCheckBoxesInDropList" {
                    set(body, &[4], boolean(el.map_or("false", xml::text))?)?;
                } else {
                    let full = self.data_path(el.map_or("", xml::text))?;
                    let full = full.as_list().ok_or_else(|| invalid(prop, "bad path"))?;
                    let base = path_value(&item.body, &[11])
                        .and_then(Node::as_list)
                        .ok_or_else(|| invalid(prop, "missing base path"))?;
                    let tail = full
                        .get(1..)
                        .and_then(|full| base.get(1..).and_then(|base| full.strip_prefix(base)))
                        .ok_or_else(|| invalid(prop, "path outside base"))?;
                    let mut p = vec![count(tail.len())];
                    p.extend(tail.iter().cloned());
                    set(body, &[9], list(p))?;
                }
                set(&mut item.body, &[39, 62], value)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
fn date(value: &str) -> Result<String> {
    let value = value.strip_suffix('Z').unwrap_or(value);
    if value.len() != 19
        || value.get(4..5) != Some("-")
        || value.get(7..8) != Some("-")
        || value.get(10..11) != Some("T")
        || value.get(13..14) != Some(":")
        || value.get(16..17) != Some(":")
    {
        return Err(invalid("Value", "invalid dateTime"));
    }
    let part = |start, end| -> Result<u32> {
        value
            .get(start..end)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| invalid("Value", "invalid dateTime"))
    };
    let y = part(0, 4)?;
    let m = part(5, 7)?;
    let d = part(8, 10)?;
    let max = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    if y == 0 || d == 0 || d > max || part(11, 13)? > 23 || part(14, 16)? > 59 || part(17, 19)? > 59
    {
        return Err(invalid("Value", "dateTime out of range"));
    }
    Ok(value.chars().filter(|c| c.is_ascii_digit()).collect())
}
