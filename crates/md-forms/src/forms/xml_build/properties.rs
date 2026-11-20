use super::builder::{Builder, count, list, numeric_path, parse, set};
use super::values::{crlf, lstr, same, xml_text};
use super::{Result, invalid, xml};
use crate::forms::{
    decode::Item,
    fe::{E, from_json},
    tables::JsonRef,
};
use brace::Node;
use mdschema::forms::{PropEntry, Value};
use std::collections::{BTreeMap, HashSet};

pub(super) fn generic(entry: &PropEntry, el: Option<&E>) -> Result<Option<Node>> {
    let Some(e) = el else {
        let default = entry.default.first().and_then(Value::as_str).or_else(|| {
            entry
                .table
                .iter()
                .find(|(_, v)| v.raw.as_str() == Some("\0"))
                .map(|(k, _)| k.as_str())
        });
        return default.filter(|s| *s != "<none>").map(parse).transpose();
    };
    match entry.codec.as_str() {
        "int" => {
            if !e.kids.is_empty() || !e.attrs.is_empty() {
                return Err(invalid(&e.tag, "expected integer text"));
            }
            let n: i64 = xml::text(e)
                .parse()
                .map_err(|_| invalid(&e.tag, "invalid integer"))?;
            Ok(Some(Node::int(n)))
        }
        "str" => {
            if !e.kids.is_empty() || !e.attrs.is_empty() {
                return Err(invalid(&e.tag, "expected string text"));
            }
            Ok(Some(Node::str(xml_text(xml::text(e)))))
        }
        "flstr" => Ok(Some(lstr(Some(e))?)),
        "table" => {
            for (key, value) in &entry.table {
                if let Some(value) = value
                    .parsed
                    .as_ref()
                    .and_then(|v| from_json(JsonRef::Native(v)))
                    && same(e, &value)
                {
                    return Ok(Some(parse(key)?));
                }
            }
            if let Some(values) = entry.raw.get("external_values").and_then(Value::as_object) {
                for (key, value) in values {
                    if let Some(value) = from_json(JsonRef::Native(value))
                        && same(e, &value)
                    {
                        return Ok(Some(parse(key)?));
                    }
                }
            }
            if e.kids.is_empty() && e.attrs.is_empty() && matches!(xml::text(e), "true" | "false") {
                let wanted = if xml::text(e) == "true" { "1" } else { "0" };
                let other = if wanted == "1" { "0" } else { "1" };
                if entry
                    .table
                    .get(wanted)
                    .is_some_and(|v| v.raw.as_str() == Some("\0"))
                    && entry
                        .table
                        .get(other)
                        .and_then(|v| v.parsed.as_ref())
                        .and_then(|v| from_json(JsonRef::Native(v)))
                        .is_some_and(|v| {
                            v.tag == e.tag
                                && xml::text(&v) != xml::text(e)
                                && matches!(xml::text(&v), "true" | "false")
                        })
                {
                    return Ok(Some(Node::atom(wanted)));
                }
            }
            Err(invalid(
                &e.tag,
                "value has no inverse in the platform table",
            ))
        }
        _ => Ok(None),
    }
}
pub(super) fn events(values: Vec<(String, String)>) -> Node {
    let mut out = vec![count(values.len())];
    for (id, handler) in &values {
        out.extend([Node::atom(id), Node::str(crlf(handler))]);
    }
    out.extend([Node::int(1), Node::int(0)]);
    for (id, _) in &values {
        out.extend([Node::atom(id), Node::int(0), Node::int(1)]);
    }
    list(out)
}
fn event_rows(e: Option<&E>) -> Result<Vec<(&str, &str)>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if let Some(e) = e {
        xml::container(e)?;
        for c in &e.kids {
            if c.attrs.iter().any(|(k, _)| k != "name") {
                return Err(invalid("Event", "unknown event attribute"));
            }
            if c.tag != "Event" || !c.kids.is_empty() {
                return Err(invalid("Events", "expected Event"));
            }
            let name = xml::attr(c, "name").ok_or_else(|| invalid("Event", "missing name"))?;
            if !seen.insert(name) {
                return Err(invalid("Events", "duplicate event"));
            }
            out.push((name, xml::text(c)));
        }
    }
    Ok(out)
}
pub(super) fn path_value<'a>(body: &'a [Node], path: &[usize]) -> Option<&'a Node> {
    let (first, rest) = path.split_first()?;
    let n = body.get(*first)?;
    if rest.is_empty() {
        Some(n)
    } else {
        path_value(n.as_list()?, rest)
    }
}
fn item_get<'a>(item: &'a Item, path: &[Value]) -> Option<&'a Node> {
    match path.first()?.as_str() {
        Some("bag") => item
            .bag
            .iter()
            .find(|(k, _)| {
                path.get(1).is_some_and(|v| {
                    v.as_str() == Some(k) || v.as_u64().map(|n| n.to_string()).as_deref() == Some(k)
                })
            })
            .map(|(_, v)| v),
        _ => path_value(&item.body, &numeric_path(path).ok()?),
    }
}
pub(super) fn item_set(item: &mut Item, path: &[Value], value: Node) -> Result<()> {
    if path.first().and_then(Value::as_str) == Some("bag") {
        let key = path
            .get(1)
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .ok_or_else(|| invalid("property", "missing bag key"))?;
        if let Some((_, v)) = item.bag.iter_mut().find(|(k, _)| *k == key) {
            *v = value;
        } else {
            item.bag.push((key, value));
        }
        Ok(())
    } else {
        set(&mut item.body, &numeric_path(path)?, value)
    }
}
impl Builder<'_> {
    pub fn hidden(&self, e: &E, context: &str, item: &mut Item) -> Result<()> {
        let Some(hidden) = self
            .data
            .get("item_hidden")
            .and_then(|v| v.get("base"))
            .and_then(|v| v.get(context))
        else {
            return Ok(());
        };
        if hidden.get("ver").and_then(Value::as_str) != Some(&item.version)
            || hidden.get("len").and_then(Value::as_u64) != Some(item.body.len() as u64)
        {
            return Ok(());
        }
        if let Some(defaults) = hidden.get("d").and_then(Value::as_object) {
            for (key, default) in defaults {
                let path: Vec<usize> = serde_json::from_str(key)
                    .map_err(|_| invalid(context, "invalid hidden path"))?;
                let mut value = default;
                if let Some(rule) = hidden.get("r").and_then(|v| v.get(key)) {
                    let feature = rule.get(0).and_then(Value::as_str).unwrap_or_default();
                    let text = xml::child(e, feature).map_or("-", xml::text);
                    if let Some(v) = rule.get(1).and_then(|v| v.get(text)) {
                        value = v;
                    }
                }
                if path_value(&item.body, &path).is_some_and(|n| {
                    !n.is_list() && n.as_atom() != Some(crate::forms::decode::NESTED)
                }) {
                    let v = value
                        .as_str()
                        .ok_or_else(|| invalid(context, "invalid hidden value"))?;
                    set(&mut item.body, &path, Node::atom(v))?;
                }
            }
        }
        Ok(())
    }
    pub fn form_events(&self) -> Result<Node> {
        let mut out = Vec::new();
        let cat = self.category();
        for (name, handler) in event_rows(xml::child(self.root, "Events"))? {
            let id = self
                .data
                .get("form_events")
                .and_then(|v| v.get(format!("{cat}|{name}")).or_else(|| v.get(name)))
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Events", format!("unsupported form event {name}")))?;
            out.push((id.to_owned(), handler.to_owned()));
        }
        Ok(events(out))
    }
    fn item_events(&self, e: &E, ctx: &str, item: &mut Item) -> Result<()> {
        let mut paths: Vec<Vec<usize>> = Vec::new();
        for source in [
            self.templates.get("_event_paths"),
            self.data.get("event_paths"),
        ] {
            if let Some(values) = source.and_then(|v| v.get(ctx)).and_then(Value::as_array) {
                for v in values {
                    let p = numeric_path(
                        v.as_array()
                            .ok_or_else(|| invalid(ctx, "invalid event path"))?,
                    )?;
                    if !paths.contains(&p) {
                        paths.push(p);
                    }
                }
            }
        }
        let mut groups: BTreeMap<Vec<usize>, Vec<(String, String)>> = BTreeMap::new();
        for (name, handler) in event_rows(xml::child(e, "Events"))? {
            let key = format!("{ctx}|{name}");
            let row = self
                .data
                .get("item_events")
                .and_then(|v| v.get(&key))
                .or_else(|| self.templates.get("_events").and_then(|v| v.get(&key)))
                .ok_or_else(|| invalid(ctx, format!("unsupported event {name}")))?;
            let id = row
                .get(0)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(ctx, "invalid event id"))?;
            let path = numeric_path(
                row.get(1)
                    .and_then(Value::as_array)
                    .ok_or_else(|| invalid(ctx, "invalid event path"))?,
            )?;
            if !paths.contains(&path) {
                paths.push(path.clone());
            }
            groups
                .entry(path)
                .or_default()
                .push((id.to_owned(), handler.to_owned()));
        }
        for path in paths {
            if path_value(&item.body, &path).is_some() {
                set(
                    &mut item.body,
                    &path,
                    events(groups.remove(&path).unwrap_or_default()),
                )?;
            }
        }
        if !groups.is_empty() {
            return Err(invalid(ctx, "event path outside item"));
        }
        Ok(())
    }
    pub fn item_props(&self, e: &E, ctx: &str, item: &mut Item) -> Result<()> {
        xml::unique(e)?;
        let entries = self
            .tables
            .learned
            .get(ctx)
            .map(|c| c.props.clone())
            .unwrap_or_default();
        let mut consumed: HashSet<&str> = HashSet::new();
        let mut claimed: HashSet<Vec<Value>> = HashSet::new();
        let mut order: Vec<_> = entries
            .iter()
            .filter(|(p, _)| xml::child(e, p).is_some())
            .collect();
        order.extend(entries.iter().filter(|(p, _)| xml::child(e, p).is_none()));
        for (prop, entry) in order {
            self.check()?;
            if let Some(shape) = entry.raw.get("external_shape")
                && !crate::forms::write::external_shape_matches(JsonRef::Native(shape), item)
            {
                return Err(invalid(ctx, format!("unsupported shape for {prop}")));
            }
            let el = xml::child(e, prop);
            if el.is_some() {
                consumed.insert(prop);
            }
            if matches!(prop.as_str(), "Events" | "UserVisible" | "ChildItems") {
                continue;
            }
            if self.choice_property(item, e, prop)? {
                continue;
            }
            let path = entry.path.as_deref();
            if el.is_none() && path.is_some_and(|p| claimed.contains(p)) {
                continue;
            }
            let value = match prop.as_str() {
                "AutoMaxWidth"
                    if ctx == "SearchControlAddition"
                        && entry.raw.get("external_width_path").is_some() =>
                {
                    let path = entry
                        .raw
                        .get("external_width_path")
                        .and_then(Value::as_array)
                        .ok_or_else(|| invalid(prop, "missing external component path"))?;
                    let value = if let Some(e) = el {
                        if !e.attrs.is_empty() || !e.kids.is_empty() {
                            return Err(invalid(prop, "expected scalar boolean"));
                        }
                        super::values::boolean(xml::text(e))?
                    } else {
                        parse(
                            entry
                                .raw
                                .get("external_width_default")
                                .and_then(Value::as_str)
                                .ok_or_else(|| {
                                    invalid(prop, "missing external component default")
                                })?,
                        )?
                    };
                    item_set(item, path, value)?;
                    claimed.insert(path.to_vec());
                    continue;
                }
                "MinValue" | "MaxValue"
                    if ctx == "InputField"
                        && el.is_some_and(|e| xml::attr(e, "xsi:type") == Some("xs:decimal")) =>
                {
                    Some(self.numeric_bound(el.ok_or_else(|| invalid(prop, "missing bound"))?)?)
                }
                "DataPath" => {
                    let pos = match item.class {
                        Some("Field" | "Table") => 11,
                        Some("Button") => 9,
                        _ => continue,
                    };
                    set(
                        &mut item.body,
                        &[pos],
                        self.data_path(el.map_or("", xml::text))?,
                    )?;
                    continue;
                }
                "WarningOnEditRepresentation"
                    if item.class == Some("Field")
                        && el.is_some_and(|e| {
                            xml::text(e) == "DontShow" && e.kids.is_empty() && e.attrs.is_empty()
                        }) =>
                {
                    set(&mut item.body, &[17], Node::int(1))?;
                    continue;
                }
                "RowPictureDataPath" if item.class == Some("Table") => {
                    if let Some(el) = el {
                        let base = self.data_path(xml::ctext(e, "DataPath", ""))?;
                        let full = self.data_path(xml::text(el))?;
                        let base = base
                            .as_list()
                            .ok_or_else(|| invalid(ctx, "invalid table path"))?;
                        let full = full
                            .as_list()
                            .ok_or_else(|| invalid(ctx, "invalid picture path"))?;
                        let base = base
                            .get(1..)
                            .filter(|p| !p.is_empty())
                            .ok_or_else(|| invalid(ctx, "empty table path"))?;
                        let tail = full
                            .get(1..)
                            .and_then(|p| p.strip_prefix(base))
                            .ok_or_else(|| {
                                invalid(ctx, "row picture path belongs to another table")
                            })?;
                        let mut relative = vec![count(tail.len())];
                        relative.extend_from_slice(tail);
                        set(&mut item.body, &[43], list(relative))?;
                    }
                    continue;
                }
                "FooterDataPath" => {
                    set(
                        &mut item.body,
                        &[12],
                        self.data_path(el.map_or("", xml::text))?,
                    )?;
                    continue;
                }
                "CommandName" => {
                    set(
                        &mut item.body,
                        &[8],
                        self.command_ref(el.map_or("", xml::text))?,
                    )?;
                    continue;
                }
                "AdditionSource" => {
                    if let Some(el) = el {
                        let id = self
                            .item_ids
                            .get(xml::ctext(el, "Item", ""))
                            .ok_or_else(|| invalid(ctx, "unknown addition item"))?;
                        let kind = match xml::ctext(el, "Type", "") {
                            "SearchStringRepresentation" => 0,
                            "ViewStatusRepresentation" => 1,
                            "SearchControl" => 2,
                            _ => return Err(invalid(ctx, "unknown addition source")),
                        };
                        set(
                            &mut item.body,
                            &[19],
                            list(vec![Node::atom(id), Node::int(kind)]),
                        )?;
                    }
                    continue;
                }
                "CommandSource" => {
                    let mut v = list(vec![Node::int(0)]);
                    if let Some(el) = el {
                        let value = xml::text(el);
                        v =
                            if value == "Form" {
                                list(vec![Node::int(0), Node::atom(crate::forms::decode::FORMID)])
                            } else if let Some(name) = value.strip_prefix("Item.") {
                                list(vec![
                                    Node::atom(self.item_ids.get(name).ok_or_else(|| {
                                        invalid(ctx, "unknown command source item")
                                    })?),
                                    Node::atom(crate::forms::decode::FORMID),
                                ])
                            } else {
                                return Err(invalid(ctx, "unsupported command source"));
                            };
                    }
                    set(
                        &mut item.body,
                        &[20, if e.tag == "ButtonGroup" { 1 } else { 2 }],
                        v,
                    )?;
                    continue;
                }
                "CommandSet" => {
                    let Some(el) = el else {
                        continue;
                    };
                    let category = if item.class == Some("Table") {
                        self.table_command_category(e)?
                    } else {
                        format!("F:{}", e.tag)
                    };
                    let external_owner = crate::forms::write::fc_external_owner(self.input.owner);
                    let selection = if external_owner && item.class == Some("Table") {
                        let entry = entries
                            .iter()
                            .find(|(prop, _)| prop == "SelectionMode")
                            .map(|(_, entry)| entry)
                            .ok_or_else(|| {
                                invalid("CommandSet", "missing SelectionMode resource")
                            })?;
                        generic(entry, xml::child(e, "SelectionMode"))?
                            .and_then(|value| value.as_i64())
                    } else {
                        None
                    };
                    let policy = if external_owner && item.class == Some("Table") {
                        crate::forms::command_policy::for_table(item, &category, selection)
                            .map_err(|what| invalid("CommandSet", what))?
                    } else {
                        None
                    };
                    set(
                        &mut item.body,
                        &[if item.class == Some("Table") { 56 } else { 48 }],
                        self.command_set_available(Some(el), &category, policy.as_ref(), None)?,
                    )?;
                    continue;
                }
                "Title"
                    if ctx.ends_with("/ExtendedTooltip")
                        || matches!(ctx, "LabelDecoration" | "PictureDecoration") =>
                {
                    let title = lstr(el)?;
                    let formatted = el.and_then(|e| xml::attr(e, "formatted")) == Some("true");
                    set(
                        &mut item.body,
                        &[if ctx.ends_with("/ExtendedTooltip") {
                            22
                        } else {
                            23
                        }],
                        list(vec![
                            Node::int(1),
                            title.clone(),
                            Node::int(i64::from(formatted)),
                        ]),
                    )?;
                    let plain = if formatted {
                        plain_title(&title)
                    } else {
                        title
                    };
                    set(&mut item.body, &[7], plain)?;
                    continue;
                }
                "Picture"
                | "HeaderPicture"
                | "RowsPicture"
                | "ValuesPicture"
                | "FooterPicture"
                | "ChoiceButtonPicture" => {
                    if let Some(p) = path {
                        let numeric = numeric_path(p)?;
                        let mut target = numeric.clone();
                        if let Some(cur) = path_value(&item.body, &numeric)
                            && !is_picture(cur)
                        {
                            if let Some((i, _)) = cur
                                .as_list()
                                .and_then(|v| v.iter().enumerate().find(|(_, n)| is_picture(n)))
                            {
                                target.push(i);
                            } else {
                                if el.is_some() {
                                    return Err(invalid(ctx, "picture slot missing"));
                                }
                                continue;
                            }
                        }
                        set(
                            &mut item.body,
                            &target,
                            self.picture(el, xml::attr(e, "name"))?,
                        )?;
                        if el.is_some() {
                            claimed.insert(p.to_vec());
                        }
                        continue;
                    }
                    None
                }
                _ if prop.ends_with("Color") && el.is_some() => {
                    Some(self.color(el.map_or("", xml::text))?)
                }
                _ if prop.ends_with("Font") && el.is_some() => {
                    Some(self.font(el.ok_or_else(|| invalid(ctx, "missing font"))?)?)
                }
                "Shortcut" => Some(self.shortcut(el.map_or("", xml::text))?),
                "ChoiceForm" => el
                    .map(|el| self.metadata(xml::text(el)))
                    .transpose()?
                    .or_else(|| Some(Node::atom("00000000-0000-0000-0000-000000000000"))),
                "AvailableTypes" => el.map(|el| self.pattern(Some(el))).transpose()?,
                _ => generic(entry, el)?,
            };
            if let (Some(path), Some(value)) = (path, value) {
                if el.is_none() {
                    if let Some(cur) = item_get(item, path) {
                        let key = brace::to_compact(cur);
                        if entry.default.iter().any(|d| d.as_str() == Some(&key))
                            || entry
                                .table
                                .get(&key)
                                .is_some_and(|v| v.raw.as_str() == Some("\0"))
                        {
                            continue;
                        }
                    }
                    if claimed
                        .iter()
                        .any(|p| p.len() > path.len() && p.starts_with(path))
                    {
                        continue;
                    }
                }
                item_set(item, path, value)?;
                if el.is_some() {
                    claimed.insert(path.to_vec());
                }
            } else if el.is_some() {
                return Err(invalid(ctx, format!("unsupported property {prop}")));
            }
        }
        self.item_events(e, ctx, item)?;
        for c in &e.kids {
            if !consumed.contains(c.tag.as_str()) && self.choice_property(item, e, &c.tag)? {
                consumed.insert(&c.tag);
            }
        }
        for c in &e.kids {
            if !consumed.contains(c.tag.as_str())
                && !matches!(
                    c.tag.as_str(),
                    "Events"
                        | "UserVisible"
                        | "ChildItems"
                        | "ContextMenu"
                        | "ExtendedTooltip"
                        | "AutoCommandBar"
                        | "SearchStringAddition"
                        | "SearchControlAddition"
                        | "ViewStatusAddition"
                )
            {
                return Err(invalid(ctx, format!("unknown property {}", c.tag)));
            }
        }
        if let Some(hidden) = self
            .data
            .get("item_hidden")
            .and_then(|v| v.get("base"))
            .and_then(|v| v.get(ctx))
            .and_then(|v| v.get("m"))
            .and_then(Value::as_array)
        {
            for pair in hidden {
                let from = numeric_path(
                    pair.get(0)
                        .and_then(Value::as_array)
                        .ok_or_else(|| invalid(ctx, "invalid mirror source"))?,
                )?;
                let to = numeric_path(
                    pair.get(1)
                        .and_then(Value::as_array)
                        .ok_or_else(|| invalid(ctx, "invalid mirror target"))?,
                )?;
                if let Some(value) = path_value(&item.body, &from).cloned()
                    && path_value(&item.body, &to).is_some()
                {
                    set(&mut item.body, &to, value)?;
                }
            }
        }
        Ok(())
    }
    pub fn form_props(
        &self,
        head: &mut [Node],
        tail: &mut [Node],
        bag: &mut BTreeMap<String, Node>,
    ) -> Result<()> {
        let context = self
            .tables
            .learned
            .get("Form")
            .ok_or_else(|| invalid("Form", "missing property schema"))?;
        let structural = [
            "AutoCommandBar",
            "ChildItems",
            "Attributes",
            "Parameters",
            "Commands",
            "CommandInterface",
            "Events",
            "CommandSet",
        ];
        for c in &self.root.kids {
            if !structural.contains(&c.tag.as_str())
                && !context.props.iter().any(|(p, _)| p == &c.tag)
            {
                return Err(invalid("Form", format!("unknown property {}", c.tag)));
            }
        }
        if let Some(hidden) = self
            .data
            .get("form_hidden")
            .and_then(|v| v.get("base"))
            .and_then(Value::as_object)
        {
            for (key, rule) in hidden {
                let feature = rule.get(0).and_then(Value::as_str).unwrap_or_default();
                let value = xml::ctext(self.root, feature, "-");
                if let Some(v) = rule
                    .get(1)
                    .and_then(|v| v.get(value))
                    .and_then(Value::as_str)
                {
                    let (part, index) = key
                        .split_once('|')
                        .ok_or_else(|| invalid("Form", "invalid hidden field"))?;
                    let index: usize = index
                        .parse()
                        .map_err(|_| invalid("Form", "invalid hidden index"))?;
                    set(
                        if part == "h" { head } else { tail },
                        &[index],
                        Node::atom(v),
                    )?;
                }
            }
        }
        let mut claimed = HashSet::new();
        let mut order: Vec<_> = context
            .props
            .iter()
            .filter(|(p, _)| xml::child(self.root, p).is_some())
            .collect();
        order.extend(
            context
                .props
                .iter()
                .filter(|(p, _)| xml::child(self.root, p).is_none()),
        );
        for (prop, entry) in order {
            self.check()?;
            if structural.contains(&prop.as_str()) {
                continue;
            }
            let el = xml::child(self.root, prop);
            let Some(path) = entry.path.as_ref() else {
                if el.is_some() {
                    return Err(invalid("Form", format!("unsupported property {prop}")));
                }
                continue;
            };
            if el.is_none() && claimed.contains(path) {
                continue;
            }
            let value = generic(entry, el)?;
            if el.is_some() && value.is_none() {
                return Err(invalid("Form", format!("unsupported property {prop}")));
            }
            let Some(value) = value else {
                continue;
            };
            match path.first().and_then(Value::as_str) {
                Some("h" | "t") => {
                    let dst = if path.first().and_then(Value::as_str) == Some("h") {
                        &mut *head
                    } else {
                        &mut *tail
                    };
                    let p = numeric_path(path.get(1..).unwrap_or_default())?;
                    if el.is_none()
                        && let Some(cur) = path_value(dst, &p)
                    {
                        let key = brace::to_compact(cur);
                        if entry.default.iter().any(|d| d.as_str() == Some(&key))
                            || entry
                                .table
                                .get(&key)
                                .is_some_and(|v| v.raw.as_str() == Some("\0"))
                        {
                            continue;
                        }
                    }
                    set(dst, &p, value)?;
                }
                Some("bag") => {
                    let key = path
                        .get(1)
                        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                        .ok_or_else(|| invalid("Form", "missing bag key"))?;
                    if el.is_some() || bag.contains_key(&key) {
                        bag.insert(key, value);
                    }
                }
                _ => return Err(invalid("Form", "unsupported property path")),
            }
            if el.is_some() {
                claimed.insert(path.clone());
            }
        }
        Ok(())
    }
    fn table_command_category(&self, e: &E) -> Result<String> {
        let path = self.data_path(xml::ctext(e, "DataPath", ""))?;
        let category = self
            .with_form_info(|info| {
                info.resolve(&path).and_then(|(_, node)| node).map(|node| {
                    if node.kind == crate::forms::ctx::NodeKind::Ts {
                        "TabularSection".to_owned()
                    } else {
                        crate::forms::codecs_a::category(node.tname.as_deref())
                    }
                })
            })?
            .flatten()
            .filter(|category| category != "none")
            .ok_or_else(|| invalid("CommandSet", "unresolved table row type"))?;
        Ok(format!("T:{category}"))
    }
    pub fn command_set(&self, el: Option<&E>, category: &str, head: &[Node]) -> Result<Node> {
        let flag = if crate::forms::write::fc_external_owner(self.input.owner) {
            Some(
                head.get(13)
                    .and_then(Node::as_i64)
                    .ok_or_else(|| invalid("CommandSet", "invalid Customizable scalar"))?,
            )
        } else {
            None
        };
        self.command_set_available(el, category, None, flag)
    }
    fn command_set_available(
        &self,
        el: Option<&E>,
        category: &str,
        policy: Option<&crate::forms::command_policy::TablePolicy<'_>>,
        form_flag: Option<i64>,
    ) -> Result<Node> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        if let Some(e) = el {
            xml::container(e)?;
            for c in &e.kids {
                self.check()?;
                if c.tag != "ExcludedCommand" || !c.kids.is_empty() || !c.attrs.is_empty() {
                    return Err(invalid("CommandSet", "expected ExcludedCommand text"));
                }
                let id = self.standard_command(category, xml::text(c))?;
                if policy
                    .map(|policy| policy.unavailable(&id, xml::text(c)))
                    .transpose()
                    .map_err(|what| invalid("CommandSet", what))?
                    .unwrap_or(false)
                {
                    return Err(invalid(
                        "CommandSet",
                        "unavailable command in SingleRow table",
                    ));
                }
                if form_flag
                    .map(|flag| {
                        crate::forms::command_policy::form_unavailable(&id, xml::text(c), flag)
                    })
                    .transpose()
                    .map_err(|what| invalid("CommandSet", what))?
                    .unwrap_or(false)
                {
                    return Err(invalid(
                        "CommandSet",
                        "unavailable command in non-customizable form",
                    ));
                }
                if !seen.insert(id.clone()) {
                    return Err(invalid("CommandSet", "duplicate excluded command"));
                }
                ids.push(id);
            }
        }
        ids.sort();
        let mut out = vec![count(ids.len())];
        out.extend(ids.into_iter().map(Node::atom));
        Ok(list(out))
    }
    fn standard_command(&self, category: &str, name: &str) -> Result<String> {
        let prefix = format!("{category}|");
        let commands = self
            .tables
            .misc
            .get("cmd")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("CommandSet", "missing standard command resource"))?;
        let mut found = None;
        for (key, value) in commands {
            self.check()?;
            if value.as_str() != Some(name) || !key.starts_with(&prefix) {
                continue;
            }
            let (_, id) = key
                .rsplit_once('|')
                .ok_or_else(|| invalid("CommandSet", "invalid command resource identity"))?;
            if found.is_some_and(|first| first != id) {
                return Err(invalid(
                    "CommandSet",
                    format!("ambiguous standard command {category}.{name}"),
                ));
            }
            found = Some(id);
        }
        found.map(str::to_owned).ok_or_else(|| {
            invalid(
                "CommandSet",
                format!("unknown standard command {category}.{name}"),
            )
        })
    }
    pub fn command_ref(&self, text: &str) -> Result<Node> {
        if let Some(raw) = text.strip_prefix("0:") {
            let id = brace::Guid::parse(raw)
                .ok_or_else(|| invalid("CommandName", "invalid literal identity"))?;
            return Ok(list(vec![Node::int(0), Node::guid(id)]));
        }

        if text.is_empty() || text == "0" {
            return Ok(list(vec![Node::int(0)]));
        }
        if let Some(name) = text.strip_prefix("Form.Command.") {
            return Ok(list(vec![
                Node::atom(
                    self.command_ids
                        .get(name)
                        .ok_or_else(|| invalid("CommandName", "unknown command"))?,
                ),
                Node::atom(crate::forms::codecs_a::CMDID),
            ]));
        }
        if let Some(name) = text.strip_prefix("Form.StandardCommand.") {
            return Ok(list(vec![
                Node::int(0),
                Node::atom(self.standard_command(&self.category(), name)?),
            ]));
        }
        if let Some(rest) = text.strip_prefix("Form.Item.")
            && let Some((item, name)) = rest.rsplit_once(".StandardCommand.")
        {
            let id = self
                .item_ids
                .get(item)
                .ok_or_else(|| invalid("CommandName", "unknown item"))?;
            let e = self
                .items
                .get(item)
                .ok_or_else(|| invalid("CommandName", "missing item"))?;
            return Ok(list(vec![
                Node::atom(id),
                Node::atom(self.standard_command(
                    &if e.tag == "Table" {
                        self.table_command_category(e)?
                    } else {
                        format!("F:{}", e.tag)
                    },
                    name,
                )?),
            ]));
        }
        Ok(list(vec![Node::int(0), self.metadata(text)?]))
    }
}
fn is_picture(n: &Node) -> bool {
    n.as_list().is_some_and(|v| {
        matches!(v.len(), 9 | 10)
            && n.at(0).and_then(Node::as_atom) == Some("4")
            && n.at(2).is_some_and(Node::is_list)
    })
}

fn plain_title(title: &Node) -> Node {
    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while !rest.is_empty() {
            if rest.starts_with("<<") || rest.starts_with(">>") {
                if let Some(ch) = rest.chars().next() {
                    out.push(ch);
                }
                rest = rest.get(2..).unwrap_or_default();
            } else if let Some(after) = rest.strip_prefix('<') {
                if let Some(end) = after.find('>') {
                    if after
                        .get(..3)
                        .is_some_and(|s| s.eq_ignore_ascii_case("img"))
                    {
                        out.push(' ');
                    }
                    rest = after.get(end + 1..).unwrap_or_default();
                } else {
                    out.push_str(rest);
                    break;
                }
            } else if let Some(ch) = rest.chars().next() {
                out.push(ch);
                rest = rest.get(ch.len_utf8()..).unwrap_or_default();
            }
        }
        out
    }
    let mut out = title.clone();
    if let Some(items) = out.as_list_mut() {
        for item in items.iter_mut().skip(2) {
            if let Some(parts) = item.as_list_mut()
                && let Some(text) = parts.get_mut(1)
                && let Some(value) = text.as_str()
            {
                *text = Node::str(strip(value));
            }
        }
    }
    out
}
