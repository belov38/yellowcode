use super::builder::{Builder, count, list};
use super::embedded::SETTINGS;
use super::values::{base64, boolean, crlf};
use super::{Result, invalid, xml};
use crate::forms::{ctx::DL_SPECIAL, fe::E};
use brace::Node;
use md_graphics::templates::dcs_build::EmbeddedNamespaces;
use std::collections::HashMap;

const SETTING_NAMES: [&str; 10] = [
    "AutoFillAvailableFields",
    "ManualQuery",
    "DynamicDataRead",
    "QueryText",
    "KeyType",
    "KeyField",
    "MainTable",
    "GetInvisibleFieldPresentations",
    "AutoSaveUserSettings",
    "ListSettings",
];

pub(super) fn consumes_setting(namespace: Option<&str>, local: &str) -> bool {
    namespace == Some(xml::NS) && SETTING_NAMES.contains(&local)
}

pub(super) fn is_dynamic(e: &E) -> bool {
    xml::child(e, "Settings").is_some_and(|s| {
        xml::attr(s, "xsi:type").is_some_and(|s| s == "DynamicList")
            || xml::child(s, "ManualQuery").is_some()
    })
}
fn typed(kind: &str, value: Node) -> Node {
    list(vec![Node::str(kind), value])
}
fn boolean_value(e: &E, prop: &str, default: &str) -> Result<Node> {
    Ok(typed("B", boolean(xml::ctext(e, prop, default))?))
}
fn secondary(name: &str) -> Option<String> {
    let value = name
        .split('.')
        .map(|p| {
            crate::forms::query::STD_RU
                .iter()
                .find(|(_, en)| *en == p)
                .map(|(ru, _)| *ru)
                .unwrap_or(match p {
                    "DataVersion" => "ВерсияДанных",
                    "MessageNo" => "НомерСообщения",
                    "Type" => "Тип",
                    _ => p,
                })
        })
        .collect::<Vec<_>>()
        .join(".");
    (value != name).then_some(value)
}
fn server_state(bytes: &[u8]) -> Result<Node> {
    let mut raw = b"A\xc1".to_vec();
    let (first, rest) = bytes.split_at(bytes.len().min(2));
    for chunk in std::iter::once(first).chain(rest.chunks(32768)) {
        let n = u16::try_from(chunk.len()).map_err(|_| invalid("ServerState", "chunk too long"))?;
        if n < 256 {
            raw.extend([0x9a, n as u8]);
        } else {
            raw.push(0x9b);
            raw.extend(n.to_le_bytes());
        }
        raw.extend_from_slice(chunk);
    }
    raw.extend(b"  ");
    let encoded = base64(&raw);
    let text = encoded
        .as_atom()
        .and_then(|s| s.strip_prefix("#base64:"))
        .ok_or_else(|| invalid("ServerState", "base64 encoding failed"))?;
    Ok(typed("S", Node::str(text.trim_start_matches(['\r', '\n']))))
}
fn field_prefixes(
    rest: &[&str],
    fields: &mut Vec<String>,
    admit: &mut dyn FnMut(usize) -> Result<()>,
) -> Result<()> {
    let mut bytes = 0usize;
    for (i, part) in rest.iter().enumerate() {
        bytes = bytes
            .checked_add(usize::from(i != 0))
            .and_then(|n| n.checked_add(part.len()))
            .ok_or(super::FormBuildError::Limit("dynamic field expansion"))?;
        admit(
            bytes
                .checked_add(128)
                .ok_or(super::FormBuildError::Limit("dynamic field expansion"))?,
        )?;
        let n = i + 1;
        let field = rest.get(..n).unwrap_or_default().join(".");
        let field = field
            .rsplit_once('[')
            .filter(|(_, s)| s.ends_with(']'))
            .map_or(field.as_str(), |(s, _)| s)
            .to_owned();
        if DL_SPECIAL.iter().any(|(_, name)| *name == field)
            || matches!(field.as_str(), "Order" | "SettingsComposer")
        {
            break;
        }
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    Ok(())
}

fn list_settings<'a>(builder: &Builder<'_>, st: &'a E) -> Result<Option<&'a E>> {
    if !xml::text(st).trim().is_empty()
        || st
            .attrs
            .iter()
            .any(|(k, v)| k != "xsi:type" || v != "DynamicList")
    {
        return Err(invalid(
            "Settings",
            "unexpected DynamicList attribute or text",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for c in &st.kids {
        builder.check()?;
        let source = builder.source_of(c)?;
        let consumed = consumes_setting(source.ns(), source.local());
        if SETTING_NAMES.contains(&source.local()) && !consumed {
            return Err(invalid(&c.tag, "unexpected setting namespace"));
        }
        if consumed && c.tag != "KeyField" && !seen.insert(c.tag.as_str()) {
            return Err(invalid(&c.tag, "duplicate DynamicList setting"));
        }
        if consumed && c.tag != "ListSettings" && (!c.attrs.is_empty() || !c.kids.is_empty()) {
            return Err(invalid(&c.tag, "unexpected scalar structure"));
        }
    }
    let Some(settings) = xml::child(st, "ListSettings") else {
        return Ok(None);
    };
    xml::container(settings)?;
    xml::unique(settings)?;
    for c in &settings.kids {
        match c.tag.as_str() {
            "dcsset:filter"
            | "dcsset:order"
            | "dcsset:conditionalAppearance"
            | "dcsset:dataParameters" => xml::container(c)?,
            "dcsset:itemsViewMode" | "dcsset:itemsUserSettingID" => {
                if !c.attrs.is_empty() || !c.kids.is_empty() {
                    return Err(invalid(&c.tag, "unexpected scalar structure"));
                }
            }
            "dcsset:item" => {
                if c.attrs.len() != 1
                    || xml::attr(c, "xsi:type") != Some("dcsset:StructureItemGroup")
                    || !xml::text(c).trim().is_empty()
                {
                    return Err(invalid(&c.tag, "unsupported structure item"));
                }
                xml::unique(c)?;
                for group in &c.kids {
                    if group.tag != "dcsset:groupItems" {
                        return Err(invalid(&group.tag, "unconsumed structure item child"));
                    }
                    xml::container(group)?;
                }
            }
            _ => return Err(invalid(&c.tag, "unconsumed ListSettings child")),
        }
    }
    Ok(Some(settings))
}
impl Builder<'_> {
    pub fn dynamic_mentions(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut out: HashMap<String, Vec<String>> = self
            .attrs
            .iter()
            .filter(|a| is_dynamic(a))
            .map(|a| {
                (
                    xml::attr(a, "name").unwrap_or_default().to_owned(),
                    Vec::new(),
                )
            })
            .collect();
        let tables: HashMap<_, _> = self
            .items
            .iter()
            .filter(|(_, e)| e.tag == "Table")
            .map(|(name, e)| {
                (
                    name.as_str(),
                    xml::ctext(e, "DataPath", "").trim_start_matches('~'),
                )
            })
            .collect();
        fn walk(
            b: &Builder<'_>,
            e: &E,
            parent: &str,
            tables: &HashMap<&str, &str>,
            out: &mut HashMap<String, Vec<String>>,
        ) -> Result<()> {
            b.check()?;
            if e.kids.is_empty()
                && matches!(
                    e.tag.as_str(),
                    "DataPath"
                        | "TitleDataPath"
                        | "FooterDataPath"
                        | "RowPictureDataPath"
                        | "MultipleValueDataPath"
                        | "ReportResult"
                        | "DetailsData"
                        | "VariantAppearance"
                        | "Attribute"
                        | "Field"
                )
                && (e.tag != "Field" || matches!(parent, "UseAlways" | "Save"))
            {
                let text = xml::text(e).trim_start_matches('~');
                let parts: Vec<_> = text.split('.').collect();
                let (attr, rest) =
                    if parts.first() == Some(&"Items") && parts.get(2) == Some(&"CurrentData") {
                        (
                            parts
                                .get(1)
                                .and_then(|t| tables.get(t))
                                .copied()
                                .unwrap_or_default(),
                            parts.get(3..).unwrap_or_default(),
                        )
                    } else {
                        (
                            parts.first().copied().unwrap_or_default(),
                            parts.get(1..).unwrap_or_default(),
                        )
                    };
                if let Some(fields) = out.get_mut(attr) {
                    field_prefixes(rest, fields, &mut |bytes| b.retain_field_prefix(bytes))?;
                }
            }
            for c in &e.kids {
                walk(b, c, &e.tag, tables, out)?;
            }
            Ok(())
        }
        walk(self, self.root, "", &tables, &mut out)?;
        Ok(out)
    }
    pub fn dynamic_settings(&self, attr: &E, st: &E) -> Result<Node> {
        self.check()?;
        let settings = list_settings(self, st)?;
        let mut rows = Vec::new();
        let mut push = |key: &str, value: Node| {
            rows.extend([Node::str(key), value]);
        };
        push(
            "QueryText",
            typed("S", Node::str(crlf(xml::ctext(st, "QueryText", "")))),
        );
        let main = xml::ctext(st, "MainTable", "");
        let (id, category) = if main.is_empty() {
            (brace::Guid::from_bytes_le([0; 16]), 0)
        } else if let Some(id) = self.resolution.metadata_id(main) {
            (id, 1)
        } else {
            let (head, tail) = main
                .rsplit_once('.')
                .ok_or_else(|| invalid("MainTable", "unresolved metadata table"))?;
            let kind = head.split('.').next().unwrap_or_default();
            let category = match (kind, tail) {
                ("AccumulationRegister", "Balance") | ("InformationRegister", "SliceLast") => 3,
                ("Task", "TasksByExecutive") => 2,
                _ => return Err(invalid("MainTable", "unknown virtual table")),
            };
            (
                self.resolution
                    .metadata_id(head)
                    .ok_or_else(|| invalid("MainTable", "unresolved metadata table"))?,
                category,
            )
        };
        push(
            "MainTable",
            list(vec![
                Node::str("#"),
                Node::atom("fc01b5df-97fe-449b-83d4-218a090e681e"),
                Node::guid(id),
            ]),
        );
        push("MainTableCategory", typed("N", Node::int(category)));
        let key = match xml::ctext(st, "KeyType", "") {
            "" => 0,
            "FieldValue" => 1,
            "RowKey" => 2,
            _ => return Err(invalid("KeyType", "unknown key type")),
        };
        push("KeyType", typed("N", Node::int(key)));
        let keys: Vec<_> = st
            .kids
            .iter()
            .filter(|c| c.tag == "KeyField")
            .map(|c| typed("S", Node::str(xml::text(c))))
            .collect();
        let mut key_fields = vec![count(keys.len())];
        key_fields.extend(keys);
        push(
            "KeyFields",
            list(vec![
                Node::str("#"),
                Node::atom("51e7a0d2-530b-11d4-b98a-008048da3034"),
                list(key_fields),
            ]),
        );
        let dynamic = boolean(xml::ctext(st, "DynamicDataRead", "true"))?
            .as_i64()
            .unwrap_or_default();
        push("DynamicalDataSelection", typed("B", Node::int(1 - dynamic)));
        push(
            "AutoFillAvailableFields",
            boolean_value(st, "AutoFillAvailableFields", "true")?,
        );
        push("ManualQuery", boolean_value(st, "ManualQuery", "false")?);
        let name = xml::attr(attr, "name").unwrap_or_default();
        let fields = self
            .dynamic_fields
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for (i, field) in fields.iter().enumerate() {
            push(&format!("FieldsMapItemId{i}"), typed("N", count(i + 1)));
            push(
                &format!("FieldsMapItemName{i}"),
                typed("S", Node::str(field)),
            );
            if let Some(s) = secondary(field) {
                push(
                    &format!("FieldsMapItemSecondaryName{i}"),
                    typed("S", Node::str(s)),
                );
            }
            push(&format!("FiledsMapItemId{i}"), typed("N", count(i + 1)));
            push(
                &format!("FiledsMapItemName{i}"),
                typed("S", Node::str(field)),
            );
        }
        push(
            "FieldsMapSecondaryNamesLoaded",
            typed("B", Node::int(i64::from(!fields.is_empty()))),
        );
        let mut required = Vec::new();
        if let Some(always) = xml::child(attr, "UseAlways") {
            for field in &always.kids {
                let text = xml::text(field).trim_start_matches('~');
                if let Some(field) = text.strip_prefix(name).and_then(|s| s.strip_prefix('.')) {
                    if let Some(i) = fields.iter().position(|s| s == field) {
                        required.push(i as i64 + 1);
                    } else if let Some((id, _)) = DL_SPECIAL.iter().find(|(_, n)| *n == field) {
                        required.push(
                            id.parse()
                                .map_err(|_| invalid("DynamicList", "invalid special field id"))?,
                        );
                    }
                }
            }
        }
        required.sort_unstable();
        for (i, id) in required.into_iter().enumerate() {
            push(&format!("ReqMapFieldId{i}"), typed("N", Node::int(id)));
        }
        push(
            "AutoSaveUserSettings",
            boolean_value(st, "AutoSaveUserSettings", "true")?,
        );
        for (tag, local, key, guid, default) in [
            (
                "filter",
                "Filter",
                "Filter",
                "f6841c6b-6c71-4c82-ae9e-d08b49db326c",
                "dfcece9d-5077-440b-b6b3-45a5cb4538eb",
            ),
            (
                "order",
                "Order",
                "Order",
                "11743ff3-2db3-4cfc-9404-90ed8209437f",
                "88619765-ccb3-46c6-ac52-38e9c992ebd4",
            ),
        ] {
            let e = settings.and_then(|e| xml::child(e, &format!("dcsset:{tag}")));
            if !default_document(e, default) {
                push(key, self.dynamic_document(e, local, guid)?);
            }
        }
        let group = settings.and_then(|e| xml::child(e, "dcsset:item"));
        let view = settings.map_or("", |e| xml::ctext(e, "dcsset:itemsViewMode", ""));
        let group_id = settings.map_or("", |e| xml::ctext(e, "dcsset:itemsUserSettingID", ""));
        if group.is_some() || view != "Normal" || group_id != "911b6018-f537-43e8-a417-da56b22f9aec"
        {
            push("GroupSelectedSettingId", typed("S", Node::str(group_id)));
            let mode = match view {
                "Normal" => 0,
                "" | "Auto" => 1,
                "QuickAccess" => 2,
                "Inaccessible" => 3,
                _ => return Err(invalid("itemsViewMode", "unknown view mode")),
            };
            push("GroupSelectedSettingViewMode", typed("N", Node::int(mode)));
            push(
                "Group",
                self.dynamic_document(
                    group.and_then(|e| xml::child(e, "dcsset:groupItems")),
                    "GroupItems",
                    "e2e2f2e9-e309-4212-9c70-3ab32dd93b4d",
                )?,
            );
            push(
                "GroupSelectedSettingPresentation",
                list(vec![
                    Node::str("#"),
                    Node::atom("87024738-fc2a-4436-ada1-df79d395c424"),
                    list(vec![Node::int(0)]),
                ]),
            );
        }
        let appearance = settings.and_then(|e| xml::child(e, "dcsset:conditionalAppearance"));
        if !default_document(appearance, "b75fecce-942b-4aed-abc9-e6a02e460fb3") {
            push(
                "Appearance",
                self.dynamic_document(
                    appearance,
                    "ConditionalAppearance",
                    "93de27ad-a2d8-4b10-a82b-483c9b0648fe",
                )?,
            );
        }
        push(
            "GetInvisibleFieldPresentations",
            boolean_value(st, "GetInvisibleFieldPresentations", "true")?,
        );
        push("ServerState", server_state(&self.server_state_xml(st)?)?);
        push(
            "DataParameters",
            self.dynamic_document(
                settings.and_then(|e| xml::child(e, "dcsset:dataParameters")),
                "DataParameterValues",
                "6217eee1-6289-49d2-9315-d87888cd2d62",
            )?,
        );
        let mut out = vec![Node::int(0), count(rows.len() / 2)];
        out.extend(rows);
        Ok(list(out))
    }
    fn dynamic_document(&self, e: Option<&E>, local: &str, guid: &str) -> Result<Node> {
        let bytes = self.embedded(e, local, SETTINGS, EmbeddedNamespaces::Plain, None)?;
        Ok(list(vec![
            Node::str("#"),
            Node::atom(guid),
            list(vec![base64(&bytes)]),
        ]))
    }
}
fn default_document(e: Option<&E>, id: &str) -> bool {
    e.is_some_and(|e| {
        e.attrs.is_empty()
            && xml::text(e).trim().is_empty()
            && e.kids
                .iter()
                .all(|c| c.attrs.is_empty() && c.kids.is_empty())
            && e.kids.len() == 2
            && e.kids
                .first()
                .is_some_and(|c| c.tag == "dcsset:viewMode" && xml::text(c) == "Normal")
            && e.kids
                .get(1)
                .is_some_and(|c| c.tag == "dcsset:userSettingID" && xml::text(c) == id)
    })
}
