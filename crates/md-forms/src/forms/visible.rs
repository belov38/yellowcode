use std::sync::Arc;

use brace::{Doc, Node};

use super::decode::{DecodedForm, Item, decode_form_checked, item_tag};
use super::fe::{E, from_json};
use super::tables::TableView;
use super::write::ABSENT;
use super::{Form, FormSource};
use crate::{Configuration, EntryData, Error, MdObject, Result, SubKind};
use mdschema::forms::FormCompat;

const PATH: &str = "form/InputField.Visible";

fn fail(object: &str, what: impl Into<String>) -> Error {
    Error::Build {
        object: object.into(),
        path: PATH.into(),
        what: what.into(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mapping {
    pub slot: usize,
    pub hidden: Node,
    pub shown: Node,
}

impl Mapping {
    fn value(&self, visible: bool) -> &Node {
        if visible { &self.shown } else { &self.hidden }
    }

    fn visible(&self, value: Option<&Node>) -> Option<bool> {
        match value {
            Some(v) if *v == self.shown => Some(true),
            Some(v) if *v == self.hidden => Some(false),
            _ => None,
        }
    }

    fn atom_diff(&self) -> usize {
        let (a, b) = (
            self.hidden.as_atom().unwrap_or_default(),
            self.shown.as_atom().unwrap_or_default(),
        );
        a.bytes().zip(b.bytes()).filter(|(x, y)| x != y).count()
    }
}

fn key_atom(key: &str) -> Option<Node> {
    (!key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .then(|| Node::atom(key))
}

pub(crate) fn mapping(tables: TableView<'_>) -> std::result::Result<Mapping, String> {
    let entry = tables
        .learned
        .get("InputField")
        .and_then(|c| c.props.iter().find(|(name, _)| *name == "Visible"))
        .map(|(_, e)| e)
        .ok_or("learned InputField.Visible entry absent")?;
    if entry.codec != "table" {
        return Err(format!("Visible codec {} is unsupported", entry.codec));
    }
    let path = entry.path.ok_or("Visible path absent")?;
    let slot = match (path.len(), path.first().and_then(|v| v.as_i64())) {
        (1, Some(slot)) => usize::try_from(slot).map_err(|_| "negative Visible slot")?,
        _ => return Err("Visible path is not a single body slot".into()),
    };
    let false_key = E::leaf("Visible", "false");
    let hidden: Vec<&str> = entry
        .table
        .iter()
        .filter(|(_, v)| v.parsed.and_then(from_json).as_ref() == Some(&false_key))
        .map(|(k, _)| k)
        .collect();
    let [hidden] = hidden.as_slice() else {
        return Err("Visible false inverse missing or ambiguous".into());
    };
    let mut defaults: Vec<&str> = entry
        .table
        .iter()
        .filter(|(_, v)| v.raw.as_str() == Some(ABSENT))
        .map(|(k, _)| k)
        .collect();
    for v in entry.default.iter() {
        defaults.push(v.as_str().ok_or("non-string Visible default")?);
    }
    defaults.sort_unstable();
    defaults.dedup();
    let [shown] = defaults.as_slice() else {
        return Err("Visible default inverse missing or ambiguous".into());
    };
    let (Some(hidden), Some(shown)) = (key_atom(hidden), key_atom(shown)) else {
        return Err("Visible inverse values are not atoms".into());
    };
    if hidden == shown || hidden.as_atom().map(str::len) != shown.as_atom().map(str::len) {
        return Err("Visible inverse values collide or differ in length".into());
    }
    Ok(Mapping {
        slot,
        hidden,
        shown,
    })
}

pub(crate) const COMPAT_MODES: [FormCompat; 3] =
    [FormCompat::Base, FormCompat::V8_3_12, FormCompat::V8_5_1];

fn embedded_mappings() -> Vec<std::result::Result<Mapping, String>> {
    [false, true]
        .into_iter()
        .flat_map(|v85| {
            COMPAT_MODES
                .into_iter()
                .map(move |compat| mapping(TableView::embedded_mode(v85, compat)))
        })
        .collect()
}

fn embedded_mapping(object: &str) -> Result<Mapping> {
    let mut all = embedded_mappings().into_iter();
    let base = all
        .next()
        .ok_or_else(|| fail(object, "no embedded table variants"))?
        .map_err(|w| fail(object, w))?;
    if all.any(|m| m.as_ref().ok() != Some(&base)) {
        return Err(fail(
            object,
            "Visible mapping differs between table variants",
        ));
    }
    Ok(base)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Nested(usize),
    Child(usize),
}

type Parent = Option<(&'static str, usize)>;

pub(crate) fn index(form: &Item) -> Vec<(Vec<Step>, String, &'static str)> {
    let mut out = Vec::new();
    let mut pending: Vec<(&Item, Vec<Step>, Parent)> = vec![(form, Vec::new(), None)];
    while let Some((item, path, parent)) = pending.pop() {
        if !path.is_empty() {
            let tag = item_tag(item, parent.map(|p| p.0), parent.map(|p| p.1));
            out.push((path.clone(), item.id_text.clone(), tag));
        }
        for (n, child) in item.children.iter().enumerate().rev() {
            let mut next = path.clone();
            next.push(Step::Child(n));
            pending.push((child, next, None));
        }
        for (n, (at, child)) in item.nested.iter().enumerate().rev() {
            let mut next = path.clone();
            next.push(Step::Nested(n));
            pending.push((child, next, Some((item.class.unwrap_or("nested"), *at))));
        }
    }
    out
}

pub(crate) fn item_at<'a>(mut item: &'a Item, path: &[Step]) -> Option<&'a Item> {
    for step in path {
        item = match step {
            Step::Nested(n) => &item.nested.get(*n)?.1,
            Step::Child(n) => item.children.get(*n)?,
        };
    }
    Some(item)
}

pub(crate) fn item_at_mut<'a>(mut item: &'a mut Item, path: &[Step]) -> Option<&'a mut Item> {
    for step in path {
        item = match step {
            Step::Nested(n) => &mut item.nested.get_mut(*n)?.1,
            Step::Child(n) => item.children.get_mut(*n)?,
        };
    }
    Some(item)
}

pub(crate) fn strict(doc: &Doc) -> bool {
    doc.bom
        && doc.lead.is_empty()
        && doc.root.at(0) == Some(&Node::atom("4"))
        && doc.root.get(&[1, 0]) == Some(&Node::atom("59"))
}

fn decode_exact(object: &str, doc: &Doc, which: &str) -> Result<DecodedForm> {
    let d = decode_form_checked(&doc.root)
        .map_err(|_| fail(object, format!("{which} form does not decode")))?;
    if !d.layout.serialization.is_4_59() || doc.root.at(1) != Some(&d.form.to_raw()) {
        return Err(fail(
            object,
            format!("{which} form body is not reproduced by its decoding"),
        ));
    }
    Ok(d)
}

pub fn encode_input_field_visible(entry: &str, raw: &[u8], wanted: &Doc) -> Result<Vec<u8>> {
    let bad = |what: &str| fail(entry, what);
    let original = brace::parse_doc(raw).map_err(|_| bad("original form entry does not parse"))?;
    if !strict(&original) {
        return Err(bad("strict BOM-prefixed 4/59 original form required"));
    }
    if brace::write_doc(&original) != raw {
        return Err(bad("original form entry does not round-trip exactly"));
    }
    if !strict(wanted) {
        return Err(bad("strict BOM-prefixed 4/59 wanted form required"));
    }
    let (Some(o), Some(w)) = (original.root.as_list(), wanted.root.as_list()) else {
        return Err(bad("form entry is not a list"));
    };
    let framing = original.lead != wanted.lead || original.trailer != wanted.trailer;
    if framing
        || o.len() != w.len()
        || o.iter()
            .zip(w)
            .enumerate()
            .any(|(i, (a, b))| i != 1 && a != b)
    {
        return Err(bad("only the form body may differ from the original entry"));
    }
    let od = decode_exact(entry, &original, "original")?;
    let wd = decode_exact(entry, wanted, "wanted")?;
    let items = index(&od.form);
    if index(&wd.form) != items {
        return Err(bad("form item identity or structure differs"));
    }
    let m = embedded_mapping(entry)?;
    let mut expected = od.form.clone();
    let mut changed = 0usize;
    for (path, id, tag) in &items {
        if *tag != "InputField" {
            continue;
        }
        let (Some(was), Some(now)) = (item_at(&od.form, path), item_at(&wd.form, path)) else {
            return Err(bad("item path absent"));
        };
        let (was, now) = (was.body.get(m.slot), now.body.get(m.slot));
        if m.visible(was).is_none() || m.visible(now).is_none() {
            return Err(fail(
                entry,
                format!("InputField {id}: Visible slot value outside the mapping"),
            ));
        }
        if was != now
            && let Some(now) = now
        {
            let slot = item_at_mut(&mut expected, path)
                .and_then(|it| it.body.get_mut(m.slot))
                .ok_or_else(|| bad("item path absent"))?;
            *slot = now.clone();
            changed = changed.saturating_add(1);
        }
    }
    if wanted.root.at(1) != Some(&expected.to_raw()) {
        return Err(bad("only InputField.Visible may change in the form body"));
    }
    let out = brace::write_doc(wanted);
    let diff = raw.iter().zip(&out).filter(|(a, b)| a != b).count();
    if out.len() != raw.len() || Some(diff) != changed.checked_mul(m.atom_diff()) {
        return Err(bad(
            "encoded form differs beyond the changed Visible values",
        ));
    }
    let parsed = brace::parse_doc(&out).map_err(|_| bad("encoded form does not parse"))?;
    if parsed != *wanted || index(&decode_exact(entry, &parsed, "encoded")?.form) != items {
        return Err(bad("encoded form does not decode to the wanted form"));
    }
    Ok(out)
}

fn form_slots<'a>(
    schema: &mdschema::Schema,
    objects: &'a mut [MdObject],
    out: &mut Vec<(brace::Guid, &'a mut Option<Form>)>,
) {
    for o in objects {
        if schema
            .kind(o.kind.as_str())
            .is_some_and(|k| k.props.iter().any(|(name, _)| name == "FormType"))
        {
            out.push((o.id, &mut o.form));
        }
        for f in &mut o.forms {
            if f.kind == SubKind::Form {
                out.push((f.id, &mut f.form));
            }
        }
        form_slots(schema, &mut o.subsystems, out);
    }
}

pub fn set_input_field_visible(
    model: &mut Configuration,
    form: &str,
    item: &str,
    visible: bool,
) -> Result<()> {
    let bad = |what: String| fail(form, what);
    let want = form.to_lowercase();
    let names = &model.index.names;
    let mut slots = Vec::new();
    form_slots(mdschema::schema()?, &mut model.objects, &mut slots);
    let mut hits: Vec<&mut Option<Form>> = slots
        .into_iter()
        .filter(|(id, _)| names.get(id).is_some_and(|n| n.to_lowercase() == want))
        .map(|(_, slot)| slot)
        .collect();
    let target = match hits.as_mut_slice() {
        [one] => one.as_mut(),
        [] => return Err(bad("unknown form".into())),
        _ => return Err(bad("ambiguous form".into())),
    };
    let missing = || bad("form has no retained source document (load with keep_source)".into());
    let target = target.ok_or_else(missing)?;
    let doc = match target.source.as_ref() {
        Some(FormSource::Doc(data)) => match &**data {
            EntryData::Brace(doc) => doc,
            _ => return Err(bad("retained source is not a brace document".into())),
        },
        Some(FormSource::Text(_)) => {
            return Err(bad(
                "retained source is exact entry text, not a decoded brace document (loaded without keep_source \
                 or received as text)"
                    .into(),
            ));
        }
        _ => return Err(missing()),
    };
    if !strict(doc) {
        return Err(bad("strict BOM-prefixed 4/59 form required".into()));
    }
    let d = decode_exact(form, doc, "retained")?;
    let m = embedded_mapping(form)?;
    let want_item = item.to_lowercase();
    let found: Vec<_> = index(&d.form)
        .into_iter()
        .filter(|(path, _, _)| {
            item_at(&d.form, path)
                .and_then(Item::name)
                .is_some_and(|n| n.to_lowercase() == want_item)
        })
        .collect();
    let (path, _, tag) = match found.as_slice() {
        [one] => one,
        [] => return Err(bad(format!("unknown item {item}"))),
        _ => return Err(bad(format!("ambiguous item {item}"))),
    };
    if *tag != "InputField" {
        return Err(bad(format!("item {item} is {tag}, not InputField")));
    }
    let mut body = d.form.clone();
    let slot = item_at_mut(&mut body, path)
        .and_then(|it| it.body.get_mut(m.slot))
        .ok_or_else(|| bad("Visible slot absent".into()))?;
    let current = m.visible(Some(slot));
    if current.is_none() {
        return Err(bad(format!(
            "item {item}: Visible slot value outside the mapping"
        )));
    }
    if current == Some(visible) {
        return Ok(());
    }
    *slot = m.value(visible).clone();
    let mut new_doc = doc.clone();
    *new_doc
        .root
        .get_mut(&[1])
        .ok_or_else(|| bad("form body absent".into()))? = body.to_raw();
    encode_input_field_visible(form, &brace::write_doc(doc), &new_doc)?;
    target.source = Some(FormSource::Doc(Arc::new(EntryData::Brace(new_doc))));
    Ok(())
}
