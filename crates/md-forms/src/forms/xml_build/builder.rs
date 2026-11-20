use super::{BuiltForm, FormBuildError, FormBuildInput, FormBuildResolution, Result, invalid, xml};
use crate::forms::{
    decode::{self, Item},
    fe::E,
};
use brace::Node;
use mdschema::forms::{FormTables, Value};
use memory_budget::{Category, MemoryBudget};
use std::collections::{BTreeMap, HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

pub(super) fn parse(s: &str) -> Result<Node> {
    brace::parse(s).map_err(|e| FormBuildError::Resource(e.to_string()))
}
pub(super) fn list(v: Vec<Node>) -> Node {
    Node::List(v)
}
pub(super) fn count(n: usize) -> Node {
    Node::atom(n.to_string())
}
pub(super) fn set(body: &mut [Node], path: &[usize], value: Node) -> Result<()> {
    let (first, rest) = path
        .split_first()
        .ok_or_else(|| invalid("property", "empty path"))?;
    let target = body
        .get_mut(*first)
        .ok_or_else(|| invalid("property", "path outside body"))?;
    if rest.is_empty() {
        *target = value;
        return Ok(());
    }
    set(
        target
            .as_list_mut()
            .ok_or_else(|| invalid("property", "path through scalar"))?,
        rest,
        value,
    )
}
pub(super) fn numeric_path(path: &[Value]) -> Result<Vec<usize>> {
    path.iter()
        .map(|v| {
            v.as_u64()
                .and_then(|i| usize::try_from(i).ok())
                .ok_or_else(|| invalid("property", "non-numeric path"))
        })
        .collect()
}

pub(super) struct Builder<'a> {
    pub root: &'a E,
    pub source: mdconf_core::xml::view::NodeRef<'a, 'a>,
    pub memory: &'a MemoryBudget,
    pub sources: HashMap<usize, mdconf_core::xml::view::NodeRef<'a, 'a>>,
    pub input: &'a FormBuildInput<'a>,
    pub resolution: &'a dyn FormBuildResolution,
    pub cancelled: &'a dyn Fn() -> bool,
    pub tables: std::sync::Arc<FormTables>,
    pub path_shared: crate::forms::shared::Shared,
    pub path_model: Option<decode::DecodedForm>,
    pub path_stage: usize,
    pub types: Value,
    pub data: Value,
    pub templates: Value,
    pub item_ids: HashMap<String, String>,
    pub item_names: HashSet<String>,
    pub command_ids: HashMap<String, String>,
    pub attrs: Vec<&'a E>,
    pub dynamic_fields: HashMap<String, Vec<String>>,
    pub asset_uses: std::cell::RefCell<HashSet<usize>>,
    pub asset_index: HashMap<String, usize>,
    pub native_values: std::cell::RefCell<HashMap<usize, memory_budget::Charged<Node>>>,
    pub asset_charges: std::cell::RefCell<Vec<memory_budget::Reservation>>,
    pub items: HashMap<String, &'a E>,
    pub next_id: i64,
}

fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && !p.contains(['\\', ':', '\0'])
        && p.split('/').all(|s| !matches!(s, "" | "." | ".."))
}
pub(super) fn build(
    input: FormBuildInput<'_>,
    resolution: &dyn FormBuildResolution,
    memory: &MemoryBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<BuiltForm> {
    if cancelled() {
        return Err(FormBuildError::Cancelled);
    }
    if input.xml.len() > 8 << 20 {
        return Err(FormBuildError::Limit("XML bytes"));
    }
    if input.module.len() > 8 << 20 {
        return Err(FormBuildError::Limit("module bytes"));
    }
    if input.files.len() > 4096 {
        return Err(FormBuildError::Limit("asset count"));
    }
    let mut path_bytes = 0u64;
    let mut asset_bytes = 0u64;
    for (path, bytes) in input.files {
        path_bytes = path_bytes
            .checked_add(path.len() as u64)
            .ok_or(FormBuildError::Limit("asset path bytes"))?;
        asset_bytes = asset_bytes
            .checked_add(bytes.len() as u64)
            .ok_or(FormBuildError::Limit("asset bytes"))?;
    }
    if path_bytes > 8 << 20 {
        return Err(FormBuildError::Limit("asset path bytes"));
    }
    if asset_bytes > 64 << 20 {
        return Err(FormBuildError::Limit("asset bytes"));
    }
    let nodes = input.xml.iter().filter(|&&b| b == b'<').count() as u64;
    if nodes > 65536 {
        return Err(FormBuildError::Limit("XML tokens"));
    }
    let admission = (64 << 20)
        + nodes * (256 << 10)
        + input.xml.len() as u64 * 32
        + input.module.len() as u64 * 8
        + asset_bytes * 8
        + path_bytes * 16
        + input.files.len() as u64 * 512;
    let charge = memory.try_reserve(Category::Execution, admission)?;
    let mut paths = HashSet::new();
    let mut asset_index = HashMap::new();
    asset_index
        .try_reserve(input.files.len())
        .map_err(|_| memory_budget::BudgetError::AllocationFailed)?;
    for (index, (path, _)) in input.files.iter().enumerate() {
        if cancelled() {
            return Err(FormBuildError::Cancelled);
        }
        if !safe_path(path) {
            return Err(invalid("assets", "unsafe path"));
        }
        let key: String = path.nfc().flat_map(char::to_lowercase).collect();
        if !paths.insert(key) {
            return Err(invalid("assets", "case/NFC path collision"));
        }
        asset_index.insert(path.nfc().collect(), index);
    }
    let arena = xml::parse(input.xml, memory, cancelled)?;
    let source = arena
        .root()
        .ok_or_else(|| invalid("Form", "missing XML root"))?;
    let root = xml::convert(source, cancelled)?;
    xml::unique(&root)?;
    if !xml::text(&root).trim().is_empty() {
        return Err(invalid("Form", "unexpected root text"));
    }
    for child in &root.kids {
        if matches!(
            child.tag.as_str(),
            "Attributes" | "Commands" | "Parameters" | "ChildItems" | "CommandInterface"
        ) {
            xml::container(child)?;
        }
    }
    if root.attrs.iter().any(|(k, _)| k != "version") {
        return Err(invalid("Form", "unknown attribute"));
    }
    if let Some(version) = xml::attr(&root, "version")
        && !matches!(
            version,
            "2.10"
                | "2.11"
                | "2.12"
                | "2.13"
                | "2.14"
                | "2.15"
                | "2.16"
                | "2.17"
                | "2.18"
                | "2.19"
                | "2.20"
                | "2.21"
        )
    {
        return Err(invalid("Form", "unsupported XML version"));
    }
    let resource =
        mdschema::embedded_resource().map_err(|e| FormBuildError::Resource(e.to_string()))?;
    let read = |name| -> Result<Value> {
        serde_json::from_str(
            resource
                .json(name)
                .map_err(|e| FormBuildError::Resource(e.to_string()))?,
        )
        .map_err(|e| FormBuildError::Resource(e.to_string()))
    };
    let tables = std::sync::Arc::new(
        mdschema::forms::owned_tables(
            resource,
            input.owner.is_some_and(|o| {
                o.starts_with("ExternalDataProcessor.") || o.starts_with("ExternalReport.")
            }),
            mdschema::forms::FormCompat::Base,
        )
        .map_err(|e| FormBuildError::Resource(e.to_string()))?,
    );
    let standard = read("stdattrs.json")?;
    let std = standard
        .as_object()
        .into_iter()
        .flat_map(|m| m.iter())
        .filter_map(|(kind, rows)| {
            Some((
                kind.clone(),
                rows.as_object()?
                    .iter()
                    .filter_map(|(id, name)| Some((id.clone(), name.as_str()?.to_owned())))
                    .collect(),
            ))
        })
        .collect();
    let mut names = Vec::new();
    let mut metadata_charges = Vec::new();
    let mut metadata_error = None;
    resolution.metadata_entries(&mut |id, name| {
        if metadata_error.is_some() {
            return;
        }
        if cancelled() {
            metadata_error = Some(FormBuildError::Cancelled);
            return;
        }
        let admission = (name.len() as u64)
            .checked_mul(32)
            .and_then(|n| n.checked_add(512));
        let Some(admission) = admission else {
            metadata_error = Some(FormBuildError::Limit("metadata names"));
            return;
        };
        match memory.try_reserve(Category::Execution, admission) {
            Ok(charge) => {
                metadata_charges.push(charge);
                names.push(id);
            }
            Err(e) => metadata_error = Some(e.into()),
        }
    });
    if let Some(e) = metadata_error {
        return Err(e);
    }
    names.sort_unstable();
    names.dedup();
    let path_shared = crate::forms::shared::Shared::from_owned_resolution(
        tables.clone(),
        std,
        names,
        HashMap::new(),
        resolution,
    );
    let mut builder = Builder {
        root: &root,
        source,
        memory,
        sources: xml::source_map(&root, source, cancelled)?,
        input: &input,
        resolution,
        cancelled,
        tables,
        path_shared,
        path_model: None,
        path_stage: 0,
        types: read("types.json")?,
        data: read("forms/build.json")?,
        templates: read("forms/templates.json")?,
        item_ids: HashMap::new(),
        item_names: HashSet::new(),
        command_ids: HashMap::new(),
        attrs: Vec::new(),
        dynamic_fields: HashMap::new(),
        asset_uses: std::cell::RefCell::new(HashSet::new()),
        asset_index,
        native_values: std::cell::RefCell::new(HashMap::new()),
        asset_charges: std::cell::RefCell::new(metadata_charges),
        items: HashMap::new(),
        next_id: 1,
    };
    builder.index()?;
    builder.dynamic_fields = builder.dynamic_mentions()?;
    let tree = builder.tree()?;
    builder.check()?;
    if builder.asset_uses.borrow().len() != input.files.len() {
        return Err(invalid("assets", "unreferenced asset"));
    }
    if decode::decode_form(&tree).is_none() {
        return Err(invalid(
            "Form",
            "generated form failed structural validation",
        ));
    }
    let mut bytes = b"\xef\xbb\xbf".to_vec();
    bytes.extend_from_slice(brace::to_text(&tree).as_bytes());
    builder.check()?;
    Ok(BuiltForm {
        bytes,
        _native_values: builder.native_values.into_inner(),
        _asset_charges: builder.asset_charges.into_inner(),
        _charge: charge,
    })
}

impl<'a> Builder<'a> {
    fn set_path_model(&mut self, attrs: Node) -> Result<()> {
        let form = Item {
            id_text: "0".into(),
            class: None,
            version: "59".into(),
            body: vec![],
            children: vec![],
            nested: vec![],
            children_at: None,
            opt: vec![],
            bag: vec![],
            bag_at: None,
        };
        let records = decode::attr_records(&attrs)
            .ok_or_else(|| invalid("Attributes", "invalid path model"))?;
        self.path_stage += 1;
        self.path_model = Some(decode::DecodedForm {
            root: list(vec![Node::int(4), Node::int(0), Node::str(""), attrs]),
            form,
            module_text: String::new(),
            attrs: records,
            params: vec![],
            commands: vec![],
            layout: &crate::forms::variant::V4_59,
        });
        Ok(())
    }
    fn generated_name(&mut self, base: &str, id: i64) -> Result<String> {
        let mut name = base.to_owned();
        let mut suffix = 1usize;
        while !self
            .item_names
            .insert(name.nfc().flat_map(char::to_lowercase).collect())
        {
            self.check()?;
            name = format!("{base}{suffix}");
            suffix = suffix
                .checked_add(1)
                .ok_or(FormBuildError::Limit("generated item names"))?;
        }
        self.item_ids.insert(name.clone(), id.to_string());
        Ok(name)
    }
    pub fn retain_field_prefix(&self, bytes: usize) -> Result<()> {
        self.check()?;
        let bytes = (bytes as u64)
            .checked_mul(128)
            .ok_or(FormBuildError::Limit("dynamic field expansion"))?;
        self.asset_charges
            .borrow_mut()
            .push(self.memory.try_reserve(Category::Execution, bytes)?);
        Ok(())
    }
    pub fn retain_asset_allocation(&self, bytes: usize) -> Result<()> {
        self.check()?;
        let admission = (bytes as u64)
            .checked_mul(32)
            .ok_or(FormBuildError::Limit("asset expansion"))?;
        let charge = self.memory.try_reserve(Category::Execution, admission)?;
        self.asset_charges.borrow_mut().push(charge);
        Ok(())
    }
    pub fn check(&self) -> Result<()> {
        if (self.cancelled)() {
            Err(FormBuildError::Cancelled)
        } else {
            self.memory.check_admission(Category::Execution)?;
            Ok(())
        }
    }
    fn index(&mut self) -> Result<()> {
        fn items<'a>(
            e: &'a E,
            b: &mut Builder<'a>,
            names: &mut HashSet<String>,
            ids: &mut HashSet<String>,
        ) -> Result<()> {
            b.check()?;
            if let Some(id) = xml::attr(e, "id") {
                let n: i64 = id.parse().map_err(|_| invalid(&e.tag, "invalid item id"))?;
                let root_bar =
                    xml::child(b.root, "AutoCommandBar").is_some_and(|bar| std::ptr::eq(bar, e));
                if n == 0 || (n < 0 && !(n == -1 && root_bar)) || n == i64::MAX {
                    return Err(invalid(&e.tag, "item id out of range"));
                }
                if !ids.insert(n.to_string()) {
                    return Err(invalid(&e.tag, "duplicate item id"));
                }
                let name = xml::attr(e, "name")
                    .filter(|n| !n.is_empty())
                    .ok_or_else(|| invalid(&e.tag, "missing item name"))?;
                if name.len() > 4096 {
                    return Err(invalid(&e.tag, "item name byte limit"));
                }
                if !names.insert(name.nfc().flat_map(char::to_lowercase).collect()) {
                    return Err(invalid(&e.tag, "duplicate item name"));
                }
                b.item_ids.insert(name.to_owned(), n.to_string());
                b.items.insert(name.to_owned(), e);
                b.next_id = b.next_id.max(n + 1);
            }
            for c in &e.kids {
                items(c, b, names, ids)?;
            }
            Ok(())
        }
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        if let Some(e) = xml::child(self.root, "AutoCommandBar") {
            items(e, self, &mut names, &mut ids)?;
        }
        if let Some(e) = xml::child(self.root, "ChildItems") {
            items(e, self, &mut names, &mut ids)?;
        }
        self.item_names = names;
        if let Some(e) = xml::child(self.root, "Attributes") {
            self.attrs = e.kids.iter().filter(|e| e.tag == "Attribute").collect();
            if e.kids
                .iter()
                .any(|c| !matches!(c.tag.as_str(), "Attribute" | "ConditionalAppearance"))
            {
                return Err(invalid("Attributes", "unsupported attribute section"));
            }
        }
        if let Some(e) = xml::child(self.root, "Commands") {
            for c in &e.kids {
                if c.tag != "Command" {
                    return Err(invalid("Commands", "unknown record"));
                }
                let name =
                    xml::attr(c, "name").ok_or_else(|| invalid("Command", "missing name"))?;
                let id = super::records::id(c)?;
                if self
                    .command_ids
                    .insert(name.to_owned(), id.to_string())
                    .is_some()
                {
                    return Err(invalid("Command", "duplicate name"));
                }
            }
        }
        Ok(())
    }
    pub fn category(&self) -> String {
        self.attrs
            .iter()
            .find(|a| xml::ctext(a, "MainAttribute", "false") == "true")
            .and_then(|a| xml::child(a, "Type"))
            .and_then(|t| t.kids.first())
            .map(|t| {
                let q = xml::text(t).split(':').next_back().unwrap_or_default();
                let head = q.split('.').next().unwrap_or_default();
                if head == "ExternalDataProcessorObject" {
                    "DataProcessorObject".to_owned()
                } else if head == "ExternalReportObject" {
                    "ReportObject".to_owned()
                } else {
                    head.to_owned()
                }
            })
            .unwrap_or_else(|| "none".to_owned())
    }
    pub fn array(&self, key: &str) -> Result<Vec<Node>> {
        self.data
            .get(key)
            .and_then(|v| v.get("base"))
            .and_then(Value::as_array)
            .ok_or_else(|| FormBuildError::Resource(format!("missing {key}")))?
            .iter()
            .map(|s| {
                parse(
                    s.as_str()
                        .ok_or_else(|| FormBuildError::Resource(key.to_owned()))?,
                )
            })
            .collect()
    }
    fn tree(&mut self) -> Result<Node> {
        let preliminary = self.attributes()?;
        self.set_path_model(preliminary)?;
        let with_columns = self.attributes()?;
        self.set_path_model(with_columns)?;
        let attrs = self.attributes()?;
        self.set_path_model(attrs.clone())?;
        let params = self.parameters()?;
        let commands = self.commands()?;
        let acb = if let Some(e) = xml::child(self.root, "AutoCommandBar") {
            Some(self.item(e, None, true)?)
        } else {
            let name = self.generated_name("FormCommandBar", -1)?;
            let generated = E::new(
                "AutoCommandBar",
                None,
                vec![("name".into(), name), ("id".into(), "-1".into())],
                vec![],
            );
            Some(self.item(&generated, None, true)?)
        };
        let mut children = Vec::new();
        if let Some(e) = xml::child(self.root, "ChildItems") {
            for c in &e.kids {
                children.push(self.item(c, None, false)?);
            }
        }
        let mut head = self.array("head")?;
        let mut tail = self.array("tail")?;
        let mut bag = BTreeMap::new();
        if let Some(ext) = self
            .data
            .get("ext")
            .and_then(|v| v.get(self.category()))
            .and_then(Value::as_array)
        {
            for pair in ext {
                let key = pair
                    .get(0)
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("Form", "invalid extension key"))?;
                let value = pair
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("Form", "invalid extension value"))?;
                let value = if value.contains("<CFG>") {
                    let owner = self
                        .input
                        .owner
                        .and_then(|name| self.resolution.metadata_id(name))
                        .ok_or_else(|| invalid("Form", "report owner identity required"))?;
                    value.replace("<CFG>", &owner.to_string())
                } else {
                    value.to_owned()
                };
                bag.insert(key.to_owned(), parse(&value)?);
            }
        }
        self.form_props(&mut head, &mut tail, &mut bag)?;
        let events = self.form_events()?;
        let cmdset =
            self.command_set(xml::child(self.root, "CommandSet"), &self.category(), &head)?;
        let mut form = vec![Node::int(59)];
        form.extend(head);
        form.push(count(bag.len()));
        for (key, value) in bag {
            form.extend([Node::atom(key), value]);
        }
        form.extend([
            events,
            cmdset,
            Node::int(1),
            acb.map_or_else(|| list(vec![Node::int(0)]), |i| i.to_raw()),
            count(children.len()),
        ]);
        for item in children {
            form.push(Node::atom(class_guid(
                item.class
                    .ok_or_else(|| invalid("Form", "missing item class"))?,
            )?));
            form.push(item.to_raw());
        }
        form.extend(tail);
        let (nav, bar) = self.command_interface()?;
        Ok(list(vec![
            Node::int(4),
            list(form),
            Node::str(super::values::crlf(self.input.module)),
            attrs,
            params,
            commands,
            nav,
            bar,
            Node::int(0),
            Node::int(0),
        ]))
    }
    pub fn item(&mut self, e: &E, parent: Option<&str>, nested: bool) -> Result<Item> {
        self.check()?;
        if !xml::text(e).trim().is_empty() {
            return Err(invalid(&e.tag, "unexpected item text"));
        }
        let class = if nested {
            None
        } else {
            Some(
                class(&e.tag)
                    .ok_or_else(|| invalid(&e.tag, "unsupported item kind"))?
                    .0,
            )
        };
        let context = if nested
            && parent.is_some()
            && matches!(
                e.tag.as_str(),
                "ContextMenu" | "ExtendedTooltip" | "AutoCommandBar"
            ) {
            format!("{}/{}", parent.unwrap_or_default(), e.tag)
        } else {
            e.tag.clone()
        };
        let pctx = if e.tag == "Button" {
            format!(
                "{context}@{}:{}",
                parent.unwrap_or_default(),
                xml::ctext(e, "Type", "")
            )
        } else {
            format!("{context}@{}", parent.unwrap_or_default())
        };
        let candidates = [&pctx, &context, &e.tag];
        let tpl = candidates
            .iter()
            .filter_map(|k| self.templates.get(*k))
            .find(|t| t.get("cls").and_then(Value::as_str) == Some(class.unwrap_or("nested")))
            .ok_or_else(|| invalid(&context, "missing construction template"))?
            .clone();
        let name = xml::attr(e, "name").ok_or_else(|| invalid(&context, "missing name"))?;
        let id = xml::attr(e, "id").ok_or_else(|| invalid(&context, "missing id"))?;
        if e.attrs
            .iter()
            .any(|(k, _)| !matches!(k.as_str(), "name" | "id" | "DisplayImportance"))
        {
            return Err(invalid(name, "unknown item attribute"));
        }
        let version = tpl
            .get("ver")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(&context, "missing template version"))?
            .to_owned();
        let mut body = Vec::new();
        for s in tpl
            .get("body")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(&context, "missing template body"))?
        {
            let text = s
                .as_str()
                .ok_or_else(|| invalid(&context, "invalid template body"))?;
            body.push(if text == decode::NESTED {
                Node::atom(decode::NESTED)
            } else {
                parse(&text.replace("⟨N⟩", &name.replace('"', "\"\"")))?
            });
        }
        set(
            &mut body,
            &[1],
            list(vec![Node::atom(id), Node::atom(decode::FORMID)]),
        )?;
        let name_slot = body
            .iter()
            .enumerate()
            .skip(2)
            .take(10)
            .find(|(_, n)| n.as_str().is_some())
            .map(|(i, _)| i)
            .ok_or_else(|| invalid(&context, "missing item name slot"))?;
        set(&mut body, &[name_slot], Node::str(name))?;
        if let Some((_, Some(kind))) = class.and_then(|_| class_kind(&e.tag)) {
            set(&mut body, &[5], Node::atom(kind))?;
        }
        let children_at = match class {
            Some("Group") => Some(21),
            Some("Table") => Some(61),
            None if version == "22" => Some(21),
            _ => None,
        };
        let bag_at = (class == Some("Table") && version == "73").then_some(54);
        let mut item = Item {
            id_text: id.to_owned(),
            class,
            version,
            body,
            children: vec![],
            nested: vec![],
            children_at,
            opt: vec![],
            bag: vec![],
            bag_at,
        };
        if class == Some("Table") {
            if item.version != "73" || item.body.len() != 127 {
                return Err(invalid(&context, "unsupported table constructor layout"));
            }
            if let Some(bag) = self
                .data
                .get("table_bag")
                .and_then(|v| v.get("base"))
                .and_then(|v| v.get("other"))
                .and_then(Value::as_array)
            {
                for p in bag {
                    item.bag.push((
                        p.get(0)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        parse(p.get(1).and_then(Value::as_str).unwrap_or_default())?,
                    ));
                }
            }
        }
        self.hidden(e, &context, &mut item)?;
        self.item_props(e, &context, &mut item)?;
        if class == Some("Table") && xml::child(e, "RowFilter").is_none() {
            item.bag.retain(|(key, _)| key != "13");
        }
        if item.class == Some("Table") && xml::child(e, "RowPictureDataPath").is_none() {
            set(&mut item.body, &[43], list(vec![Node::int(0)]))?;
        }
        let importance = xml::attr(e, "DisplayImportance");
        let importance_at = match class {
            Some(class) => crate::forms::write::di_at(class),
            None => crate::forms::write::di_at_nested(&item.version),
        };
        if let Some(at) = importance_at.filter(|&at| at < item.body.len()) {
            let code = match importance {
                Some(value) => (1..=5)
                    .find(|code| {
                        crate::forms::write::display_importance(&code.to_string()) == Some(value)
                    })
                    .ok_or_else(|| invalid(name, "unknown DisplayImportance"))?,
                None => 0,
            };
            set(&mut item.body, &[at], Node::int(code))?;
        } else if importance.is_some() {
            return Err(invalid(name, "unsupported DisplayImportance layout"));
        }
        let uv = if class == Some("Button") { 3 } else { 4 };
        set(
            &mut item.body,
            &[uv],
            Node::int(i64::from(xml::child(e, "UserVisible").is_some())),
        )?;
        if let Some(v) = xml::child(e, "UserVisible") {
            item.opt.push(("UserVisible", self.roles(Some(v))?));
        }
        let nested_positions: Vec<_> = item
            .body
            .iter()
            .enumerate()
            .filter(|(_, n)| n.as_atom() == Some(decode::NESTED))
            .map(|(i, _)| i)
            .collect();
        for pos in nested_positions {
            let tag = if class.is_none() && item.version == "6" {
                match pos {
                    15 => Some("ContextMenu"),
                    17 => Some("ExtendedTooltip"),
                    _ => None,
                }
            } else {
                decode::nested_at(
                    if class.is_none() {
                        "Addition"
                    } else {
                        class.unwrap_or_default()
                    },
                    pos,
                )
            }
            .ok_or_else(|| invalid(&context, "unknown nested item position"))?;
            let ni = if let Some(n) = xml::child(e, tag) {
                self.item(n, Some(&e.tag), true)?
            } else {
                let id = self.next_id;
                self.next_id = self
                    .next_id
                    .checked_add(1)
                    .ok_or(FormBuildError::Limit("generated item ids"))?;
                let generated = self.generated_name(&format!("{name}{tag}"), id)?;
                let n = E::new(
                    tag,
                    None,
                    vec![
                        ("name".to_owned(), generated),
                        ("id".to_owned(), id.to_string()),
                    ],
                    vec![],
                );
                self.item(&n, Some(&e.tag), true)?
            };
            item.nested.push((pos, ni));
        }
        if let Some(children) = xml::child(e, "ChildItems") {
            if children_at.is_none() {
                return Err(invalid(name, "item cannot contain children"));
            }
            for c in &children.kids {
                item.children.push(self.item(c, Some(&e.tag), false)?);
            }
        }
        if let Some(at) = children_at {
            set(&mut item.body, &[at], count(item.children.len()))?;
        }
        if let Some(at) = bag_at {
            set(&mut item.body, &[at], count(item.bag.len()))?;
        }
        item.bag
            .sort_by_key(|(k, _)| k.parse::<u32>().unwrap_or(u32::MAX));
        Ok(item)
    }
}

pub(super) fn class(tag: &str) -> Option<(&'static str, Option<&'static str>)> {
    class_kind(tag)
}
fn class_kind(tag: &str) -> Option<(&'static str, Option<&'static str>)> {
    if tag == "Button" {
        return Some(("Button", None));
    }
    if tag == "Table" {
        return Some(("Table", None));
    }
    for cls in ["Group", "Field", "Decoration", "Addition"] {
        for n in 0..=20 {
            let code = n.to_string();
            if decode::kind(cls, &code) == Some(tag) {
                let codes = [
                    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14",
                    "15", "16", "17", "18", "19", "20",
                ];
                return codes.get(n).copied().map(|code| (cls, Some(code)));
            }
        }
    }
    None
}
fn class_guid(class: &str) -> Result<&'static str> {
    match class {
        "Group" => Ok("cd5394d0-7dda-4b56-8927-93ccbe967a01"),
        "Field" => Ok("77ffcc29-7f2d-4223-b22f-19666e7250ba"),
        "Decoration" => Ok("3d3cb80c-508b-41fa-8a18-680cdf5f1712"),
        "Button" => Ok("a9f3b1ac-f51b-431e-b102-55a69acdecad"),
        "Table" => Ok("143c00f7-a42d-4cd7-9189-88e4467dc768"),
        "Addition" => Ok("c5259a1d-518a-4afd-b98d-0176027e4feb"),
        _ => Err(invalid("item", "unknown class")),
    }
}
