use super::builder::{Builder, count, list, parse, set};
use super::properties::generic;
use super::values::{boolean, lstr};
use super::{Result, invalid, xml};
use crate::forms::fe::E;
use brace::Node;
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

pub(super) fn id(e: &E) -> Result<i64> {
    let value: i64 = xml::attr(e, "id")
        .ok_or_else(|| invalid(&e.tag, "missing id"))?
        .parse()
        .map_err(|_| invalid(&e.tag, "invalid id"))?;
    if value <= 0 {
        return Err(invalid(&e.tag, "expected positive id"));
    }
    Ok(value)
}
fn identity(e: &E, names: &mut HashSet<String>, ids: &mut HashSet<i64>) -> Result<(i64, String)> {
    if !xml::text(e).trim().is_empty() {
        return Err(invalid(&e.tag, "unexpected record text"));
    }
    if e.attrs
        .iter()
        .any(|(k, _)| !matches!(k.as_str(), "id" | "name"))
    {
        return Err(invalid(&e.tag, "unknown record attribute"));
    }
    let id = id(e)?;
    let name = xml::attr(e, "name")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid(&e.tag, "missing name"))?;
    if name.len() > 4096 {
        return Err(invalid(&e.tag, "record name byte limit"));
    }
    if !names.insert(name.nfc().flat_map(char::to_lowercase).collect()) || !ids.insert(id) {
        return Err(invalid(&e.tag, "duplicate record identity"));
    }
    Ok((id, name.to_owned()))
}
impl Builder<'_> {
    pub fn attributes(&self) -> Result<Node> {
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        let mut rows = Vec::new();
        for e in &self.attrs {
            self.check()?;
            xml::unique(e)?;
            let (id, name) = identity(e, &mut names, &mut ids)?;
            for c in &e.kids {
                if ![
                    "Title",
                    "Type",
                    "View",
                    "Edit",
                    "UseAlways",
                    "Save",
                    "MainAttribute",
                    "SavedData",
                    "FillCheck",
                    "Columns",
                    "Settings",
                    "FunctionalOptions",
                ]
                .contains(&c.tag.as_str())
                {
                    return Err(invalid(
                        &name,
                        format!("unknown attribute property {}", c.tag),
                    ));
                }
            }
            let mut body = vec![
                Node::int(9),
                list(vec![Node::int(id)]),
                Node::int(0),
                Node::str(name),
                lstr(xml::child(e, "Title"))?,
                self.pattern(xml::child(e, "Type"))?,
                self.roles(xml::child(e, "View"))?,
                self.roles(xml::child(e, "Edit"))?,
                self.relative_fields(e, "UseAlways")?,
                self.relative_fields(e, "Save")?,
                boolean(xml::ctext(e, "MainAttribute", "false"))?,
                boolean(xml::ctext(e, "SavedData", "false"))?,
                Node::int(i64::from(
                    xml::ctext(e, "FillCheck", "DontCheck") == "ShowError",
                )),
            ];
            let mut columns = Vec::new();
            if let Some(cols) = xml::child(e, "Columns") {
                xml::container(cols)?;
                let mut cn = HashSet::new();
                let mut ci = HashSet::new();
                for c in &cols.kids {
                    if c.tag == "AdditionalColumns" {
                        continue;
                    }
                    if c.tag != "Column" {
                        return Err(invalid("Columns", "unsupported column record"));
                    }
                    columns.push(self.column(c, &mut cn, &mut ci)?);
                }
            }
            body.push(count(columns.len()));
            body.extend(columns);
            let settings = if let Some(settings) = xml::child(e, "Settings") {
                if xml::attr(settings, "xsi:type") == Some("v8:TypeDescription") {
                    list(vec![
                        Node::int(0),
                        Node::int(1),
                        Node::str("ElementType"),
                        list(vec![
                            Node::str("#"),
                            Node::atom(crate::forms::section_codecs::T_TYPEDESC),
                            self.pattern(Some(settings))?,
                        ]),
                    ])
                } else if xml::attr(settings, "xsi:type") == Some("mxl:SpreadsheetDocument") {
                    list(vec![
                        Node::int(0),
                        Node::int(1),
                        Node::str("Moxel"),
                        self.spreadsheet(settings)?,
                    ])
                } else if super::dynamic::is_dynamic(e) {
                    self.dynamic_settings(e, settings)?
                } else {
                    return Err(invalid("Attribute.Settings", "unsupported settings type"));
                }
            } else {
                list(vec![Node::int(0), Node::int(0)])
            };
            body.extend([
                settings,
                self.functional_options(xml::child(e, "FunctionalOptions"))?,
            ]);
            rows.push(list(body));
        }
        let mut out = vec![Node::int(4), count(rows.len())];
        out.extend(rows);
        out.extend(self.appearance_tail()?);
        Ok(list(out))
    }
    fn relative_fields(&self, e: &E, tag: &str) -> Result<Node> {
        let mut rows = Vec::new();
        if self.path_stage < 2 {
            return Ok(list(vec![Node::int(0), Node::int(0)]));
        }
        if let Some(fields) = xml::child(e, tag) {
            xml::container(fields)?;
            for f in &fields.kids {
                if f.tag != "Field" {
                    return Err(invalid(tag, "unknown field record"));
                }
                if tag == "UseAlways" && super::dynamic::is_dynamic(e) {
                    let prefix = format!("{}.", xml::attr(e, "name").unwrap_or_default());
                    if xml::text(f).trim_start_matches('~').starts_with(&prefix) {
                        continue;
                    }
                }
                let path = self.data_path(xml::text(f))?;
                let mut segments = path
                    .as_list()
                    .ok_or_else(|| invalid(tag, "invalid path"))?
                    .to_vec();
                if segments.get(1).and_then(|n| n.at(0)).and_then(Node::text) != xml::attr(e, "id")
                {
                    return Err(invalid(tag, "path belongs to another attribute"));
                }
                let n = segments
                    .first()
                    .and_then(Node::as_i64)
                    .ok_or_else(|| invalid(tag, "missing path count"))?;
                segments.remove(1);
                set(&mut segments, &[0], Node::int(n - 1))?;
                rows.push(list(segments));
            }
        }
        rows.sort_by_key(brace::to_compact);
        let mut out = vec![Node::int(0), count(rows.len())];
        out.extend(rows);
        Ok(list(out))
    }
    pub fn column(
        &self,
        c: &E,
        names: &mut HashSet<String>,
        ids: &mut HashSet<i64>,
    ) -> Result<Node> {
        let (id, name) = identity(c, names, ids)?;
        xml::unique(c)?;
        for prop in &c.kids {
            if ![
                "Title",
                "Type",
                "View",
                "Edit",
                "FunctionalOptions",
                "FillCheck",
            ]
            .contains(&prop.tag.as_str())
            {
                return Err(invalid(&name, "unknown column property"));
            }
        }
        Ok(list(vec![
            Node::int(5),
            Node::int(id),
            Node::int(0),
            Node::str(name),
            lstr(xml::child(c, "Title"))?,
            self.pattern(xml::child(c, "Type"))?,
            self.roles(xml::child(c, "View"))?,
            self.roles(xml::child(c, "Edit"))?,
            self.functional_options(xml::child(c, "FunctionalOptions"))?,
            Node::int(i64::from(
                xml::ctext(c, "FillCheck", "DontCheck") == "ShowError",
            )),
        ]))
    }
    pub fn additional_columns(&self) -> Result<Vec<Node>> {
        let mut groups = Vec::new();
        let mut paths = HashSet::new();
        for attr in &self.attrs {
            let Some(columns) = xml::child(attr, "Columns") else {
                continue;
            };
            for group in columns.kids.iter().filter(|c| c.tag == "AdditionalColumns") {
                self.check()?;
                if group.attrs.len() != 1 || !xml::text(group).trim().is_empty() {
                    return Err(invalid("AdditionalColumns", "unexpected content"));
                }
                let table = xml::attr(group, "table")
                    .ok_or_else(|| invalid("AdditionalColumns", "missing table path"))?;
                let path = self.data_path(table)?;
                if !paths.insert(brace::to_compact(&path)) {
                    return Err(invalid("AdditionalColumns", "duplicate table path"));
                }
                let mut names = HashSet::new();
                let mut ids = HashSet::new();
                let mut cols = Vec::new();
                for c in &group.kids {
                    if c.tag != "Column" {
                        return Err(invalid("AdditionalColumns", "unknown record"));
                    }
                    cols.push(self.column(c, &mut names, &mut ids)?);
                }
                let mut row = vec![Node::int(0), path, count(cols.len())];
                row.extend(cols);
                groups.push(list(row));
            }
        }
        Ok(groups)
    }
    pub fn parameters(&self) -> Result<Node> {
        let mut rows = Vec::new();
        let mut names = HashSet::new();
        if let Some(section) = xml::child(self.root, "Parameters") {
            for e in &section.kids {
                self.check()?;
                if e.tag != "Parameter" {
                    return Err(invalid("Parameters", "unknown record"));
                }
                xml::unique(e)?;
                let name =
                    xml::attr(e, "name").ok_or_else(|| invalid("Parameter", "missing name"))?;
                if !names.insert(name.nfc().flat_map(char::to_lowercase).collect::<String>()) {
                    return Err(invalid("Parameter", "duplicate name"));
                }
                if e.attrs.iter().any(|(k, _)| k != "name")
                    || e.kids
                        .iter()
                        .any(|c| !matches!(c.tag.as_str(), "Type" | "KeyParameter"))
                {
                    return Err(invalid("Parameter", "unknown property"));
                }
                rows.push(list(vec![
                    Node::int(0),
                    Node::str(name),
                    self.pattern(xml::child(e, "Type"))?,
                    boolean(xml::ctext(e, "KeyParameter", "false"))?,
                ]));
            }
        }
        let mut out = vec![Node::int(0), count(rows.len())];
        out.extend(rows);
        Ok(list(out))
    }
    pub fn commands(&self) -> Result<Node> {
        let mut rows = Vec::new();
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        if let Some(section) = xml::child(self.root, "Commands") {
            for e in &section.kids {
                self.check()?;
                xml::unique(e)?;
                let (id, name) = identity(e, &mut names, &mut ids)?;
                let mut body=parse("{11,{0,409b9a53-7f7e-4178-86c1-33176c7c7a7a},\"\",{1,0},{1,0},{0,{0,{\"B\",1},0}},{0,0,0},{4,0,{0},\"\",-1,-1,1,0,\"\"},\"\",3,0,0,{0,0},1,0,1,0,0,2,0,0}")?.as_list().ok_or_else(||invalid("Command","invalid template"))?.to_vec();
                set(
                    &mut body,
                    &[1],
                    list(vec![
                        Node::int(id),
                        Node::atom(crate::forms::codecs_a::CMDID),
                    ]),
                )?;
                set(&mut body, &[2], Node::str(name))?;
                let entries = self
                    .tables
                    .learned
                    .get("Command")
                    .map(|c| c.props.as_slice())
                    .unwrap_or_default();
                for c in &e.kids {
                    if !entries.iter().any(|(p, _)| p == &c.tag) {
                        return Err(invalid("Command", format!("unknown property {}", c.tag)));
                    }
                }
                for (prop, entry) in entries {
                    let el = xml::child(e, prop);
                    let value = match prop.as_str() {
                        "Use" => Some(self.roles(el)?),
                        "FunctionalOptions" => Some(self.functional_options(el)?),
                        "Picture" => Some(self.picture(el, xml::attr(e, "name"))?),
                        "Shortcut" => Some(self.shortcut(el.map_or("", xml::text))?),
                        "AssociatedTableElementId" => {
                            let id = el
                                .map(|e| {
                                    self.item_ids.get(xml::text(e)).ok_or_else(|| {
                                        invalid("Command", "unknown associated table")
                                    })
                                })
                                .transpose()?;
                            set(
                                &mut body,
                                &[14],
                                id.map_or_else(|| Node::int(0), Node::atom),
                            )?;
                            continue;
                        }
                        _ => generic(entry, el)?,
                    };
                    if let Some(value) = value {
                        let fallback = match prop.as_str() {
                            "Use" => Some(5),
                            "FunctionalOptions" => Some(12),
                            "Picture" => Some(7),
                            _ => None,
                        };
                        if let Some(path) = entry.path.as_ref() {
                            set(&mut body, &super::builder::numeric_path(path)?, value)?;
                        } else if let Some(at) = fallback {
                            set(&mut body, &[at], value)?;
                        } else if el.is_some() {
                            return Err(invalid(
                                "Command",
                                format!("missing property path {prop}"),
                            ));
                        }
                    } else if el.is_some() {
                        return Err(invalid("Command", format!("unsupported property {prop}")));
                    }
                }
                let flag = i64::from(body.get(18).and_then(Node::as_atom) != Some("0"));
                set(&mut body, &[13], Node::int(flag))?;
                rows.push(list(body));
            }
        }
        let mut out = vec![Node::int(0), count(rows.len())];
        out.extend(rows);
        Ok(list(out))
    }
    pub fn command_interface(&self) -> Result<(Node, Node)> {
        let mut sections = Vec::new();
        let ci = xml::child(self.root, "CommandInterface");
        if let Some(ci) = ci {
            xml::unique(ci)?;
            if ci
                .kids
                .iter()
                .any(|c| !matches!(c.tag.as_str(), "NavigationPanel" | "CommandBar"))
            {
                return Err(invalid("CommandInterface", "unknown panel"));
            }
        }
        for tag in ["NavigationPanel", "CommandBar"] {
            let mut rows = Vec::new();
            if let Some(panel) = ci.and_then(|e| xml::child(e, tag)) {
                for (i, e) in panel.kids.iter().enumerate() {
                    if e.tag != "Item" {
                        return Err(invalid("CommandInterface", "unknown item"));
                    }
                    xml::unique(e)?;
                    let group = xml::ctext(e, "CommandGroup", "");
                    let group = if group.is_empty() {
                        list(vec![Node::int(0)])
                    } else {
                        let id = super::ui::resource_key("ui.json", "command_groups", group)
                            .map(Node::atom)
                            .map(Ok)
                            .unwrap_or_else(|| self.metadata(group))?;
                        list(vec![Node::int(0), id])
                    };
                    let index: i64 = xml::ctext(e, "Index", "0")
                        .parse()
                        .map_err(|_| invalid("CommandInterface", "invalid index"))?;
                    rows.push(list(vec![
                        Node::int(3),
                        count(i),
                        self.command_ref(xml::ctext(e, "Command", ""))?,
                        self.data_path(xml::ctext(e, "DataPath", ""))?,
                        Node::int(i64::from(xml::ctext(e, "Type", "") == "Added")),
                        group,
                        Node::int(index),
                        boolean(xml::ctext(e, "DefaultVisible", "true"))?,
                        self.roles(xml::child(e, "Visible"))?,
                    ]));
                }
            }
            let mut out = vec![Node::int(0), count(rows.len())];
            out.extend(rows);
            sections.push(list(out));
        }
        let mut it = sections.into_iter();
        Ok((
            it.next()
                .ok_or_else(|| invalid("CommandInterface", "missing navigation"))?,
            it.next()
                .ok_or_else(|| invalid("CommandInterface", "missing command bar"))?,
        ))
    }
}
