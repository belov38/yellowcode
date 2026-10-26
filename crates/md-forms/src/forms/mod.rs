mod codecs_a;
mod codecs_b;
mod ctx;
pub(crate) use ctx::SET_CATEGORIES;
pub(crate) mod decode;
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod dump_fixture;
mod fe;
pub(crate) use fe::PREFIX as NAMESPACE_PREFIXES;
mod model;
mod py;
mod query;
mod section_codecs;
mod sections;
#[doc(hidden)]
pub mod shared;
pub(crate) mod upgrade;
pub(crate) mod variant;
mod write;
mod xmlconv;

use std::sync::Arc;

use brace::Guid;

use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::xml_out::{DumpSink, Part, PartStatus, paths};
use crate::{Configuration, MdKind, MdObject, Result, SubKind};

pub use md_model::forms::{Form, FormAttribute, FormCommand, FormItem, FormParameter};
pub(crate) use md_model::forms::{FormShared, FormSource};
pub(crate) use model::{attribute_types, form_from_decoded};
use shared::Shared;
pub use variant::Serialization;

pub(crate) fn form_shared(shared: &FormShared) -> Option<&Shared> {
    match shared {
        FormShared::Native(value) => crate::state::<Shared>(&**value),
        FormShared::Managed(value) => {
            let value = value.downcast_ref::<Shared>();
            debug_assert!(value.is_some(), "form state of another type");
            value
        }
    }
}

pub fn serialization(form: &Form) -> Option<Serialization> {
    let source = form.source.as_ref()?;
    match source.entry() {
        Some(EntryData::Brace(doc)) => Serialization::of(&doc.root),
        Some(_) => None,
        None => Serialization::of_text(source.bytes()?),
    }
}

#[doc(hidden)]
pub fn summarize(root: &brace::Node) -> Option<Form> {
    decode::decode_form(root).map(|d| form_from_decoded(&d))
}

#[doc(hidden)]
pub fn translate_entry(
    root: &brace::Node,
) -> std::result::Result<(brace::Node, upgrade::Stats), String> {
    upgrade::translate_entry(root)
}

#[doc(hidden)]
pub fn canonical_for_twin(root: &brace::Node) -> Option<brace::Node> {
    upgrade::canon::canonical(root)
}

#[doc(hidden)]
pub fn item_names_and_reverse(root: &brace::Node) -> std::result::Result<Vec<String>, String> {
    let d = decode::decode_form(root).ok_or("form decode failed")?;
    if root.at(1) != Some(&d.form.to_raw()) {
        return Err("form to_raw differs".into());
    }
    let mut names = Vec::new();
    for it in d.items() {
        if let Some(n) = it.name() {
            names.push(n.to_string());
        }
    }
    Ok(names)
}

pub type FormXml = (String, Vec<(String, Vec<u8>)>, bool);

fn find_form(cfg: &Configuration, id: Guid) -> Option<&Form> {
    let mut stack: Vec<&MdObject> = cfg.objects.iter().collect();
    while let Some(o) = stack.pop() {
        if o.kind == MdKind::CommonForm
            && o.id == id
            && let Some(f) = &o.form
        {
            return Some(f);
        }
        if let Some(f) = o
            .forms
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.form.as_ref())
        {
            return Some(f);
        }
        stack.extend(&o.subsystems);
    }
    None
}

pub fn form_xml(cfg: &Configuration, id: Guid) -> Option<FormXml> {
    write_form(cfg, id, find_form(cfg, id)?)
}

fn write_form(cfg: &Configuration, id: Guid, f: &Form) -> Option<FormXml> {
    let shared = form_shared(f.shared.as_ref()?)?;
    let owner = cfg
        .index
        .names
        .get(&id)
        .and_then(|n| n.rsplit_once(".Form."))
        .map(|(o, _)| o);
    let source = f.source.as_ref()?;
    let parsed;
    let root = if let Some(data) = source.entry() {
        let EntryData::Brace(doc) = data else {
            return None;
        };
        &doc.root
    } else {
        parsed = brace::parse_doc(source.bytes()?).ok()?;
        &parsed.root
    };
    if Serialization::of(root).is_some_and(Serialization::is_4_59) {
        return write::form_to_xml(cfg, shared, root, owner);
    }
    let d = decode::decode_form(root)?;
    let mut stats = upgrade::Stats::default();
    let entry = upgrade::writer_set::for_writer(&d, &mut stats).ok()?;
    write::form_to_xml(cfg, shared, &entry, owner)
}

pub fn form_refusal(cfg: &Configuration, id: Guid) -> Option<String> {
    let f = find_form(cfg, id)?;
    if serialization(f).is_none_or(Serialization::is_4_59) {
        return None;
    }
    let source = f.source.as_ref()?;
    let refusal = match source.entry() {
        Some(EntryData::Brace(doc)) => upgrade::refusal_of(&doc.root),
        Some(_) => return None,
        None => upgrade::refusal_of(&brace::parse_doc(source.bytes()?).ok()?.root),
    };
    if refusal.is_some() {
        return refusal;
    }
    write_form(cfg, id, f)
        .is_none()
        .then(|| "writer-failed".to_string())
}

#[doc(hidden)]
pub fn writer_entry(
    root: &brace::Node,
) -> std::result::Result<(brace::Node, upgrade::Stats), String> {
    upgrade::writer_entry(root)
}

fn plain_picture_path(p: &str) -> bool {
    p.split('/')
        .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(['\\', ':', '\0']))
}

fn put_files(sink: &mut DumpSink, dir: &str, (text, pictures, deferred): FormXml) -> Result<()> {
    let files = std::iter::once((format!("{dir}/Ext/Form.xml"), text.into_bytes())).chain(
        pictures
            .into_iter()
            .filter(|(p, _)| plain_picture_path(p))
            .map(|(p, b)| (format!("{dir}/Ext/Form/{p}"), b)),
    );
    for (rel, data) in files {
        if deferred {
            sink.defer(&rel);
        } else {
            sink.put(Part::Forms, &rel, &data)?;
        }
    }
    Ok(())
}

#[doc(hidden)]
pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    let mut stack: Vec<&MdObject> = cfg.objects.iter().rev().collect();
    while let Some(o) = stack.pop() {
        if o.kind == MdKind::CommonForm
            && let Some(x) = o.form.as_ref().and_then(|f| write_form(cfg, o.id, f))
        {
            put_files(sink, &paths::object_dir(cfg, o), x)?;
        }
        for s in &o.forms {
            if let Some(x) = s.form.as_ref().and_then(|f| write_form(cfg, s.id, f)) {
                put_files(sink, &paths::sub_dir(cfg, o, s), x)?;
            }
        }
        stack.extend(o.subsystems.iter().rev());
    }
    Ok(PartStatus::Written)
}

fn load_form(
    cx: &LoadCtx,
    id: Guid,
    shared: &Arc<Shared>,
    misses: &mut FormMisses,
) -> Option<Form> {
    let name = format!("{id}.0");
    let data = cx.entries.doc(&name).ok().flatten();
    let form = data.as_ref().and_then(|data| {
        let EntryData::Brace(doc) = &**data else {
            return None;
        };
        if !doc.bom || !doc.lead.is_empty() {
            return None;
        }
        let d = match decode::decode_form_with(&doc.root, shared.tables()) {
            Ok(d) => d,
            Err(failure) => {
                misses.failed(id, failure);
                return None;
            }
        };
        let events = match codecs_a::event_list(shared.tables(), d.events()) {
            Ok(Some(events)) => events,
            _ => Vec::new(),
        };
        if d.layout.strict {
            let mut stats = upgrade::Stats::default();
            if let Err(refusal) = upgrade::translate(&d, &mut stats) {
                misses.untranslatable(d.layout.serialization, &refusal);
            } else if let Ok(tables) = cx.resource().upgrade_tables() {
                let n = upgrade::writer_set::table_bag_21_hits(&d, tables);
                if n > 0 {
                    misses.writer_set(d.layout.serialization, "table-bag-21", n);
                }
            }
        }
        let mut f = form_from_decoded(&d);
        f.events = events;
        f.shared = Some(FormShared::Native(Arc::<Shared>::clone(shared)));
        f.source = Some(if cx.opts.keep_source {
            FormSource::Doc(Arc::clone(data))
        } else {
            FormSource::Text(brace::write_doc(doc).into())
        });
        Some(f)
    });
    if !cx.opts.keep_source {
        cx.entries.forget(&name);
    }
    form
}

pub(crate) const SERIALIZATION_UUIDS: u32 = 10;

#[derive(Default)]
struct FormMisses(std::collections::BTreeMap<String, u32>);

impl FormMisses {
    fn add(&mut self, key: String) -> u32 {
        let n = self.0.entry(key).or_insert(0);
        *n = n.saturating_add(1);
        *n
    }

    fn failed(&mut self, id: Guid, failure: decode::Failure) {
        self.add("form:failed".to_string());
        match failure {
            decode::Failure::Shape => {}
            decode::Failure::Unsupported(s) => {
                if self.add(format!("form:serialization:{s}")) <= SERIALIZATION_UUIDS {
                    self.add(format!("form:serialization-uuid:{s}:{id}"));
                }
            }
            decode::Failure::Rejected(layout) => {
                if layout.strict {
                    self.add(format!("form:failed:{}", layout.serialization));
                }
            }
        }
    }

    fn untranslatable(&mut self, s: Serialization, refusal: &upgrade::Refusal) {
        self.add(format!("form:untranslatable:{s}"));
        self.add(format!("form:untranslatable:{s}:{refusal}"));
    }

    fn writer_set(&mut self, s: Serialization, rule: &str, n: usize) {
        let key = format!("form:writer-set:{rule}:{s}");
        for _ in 0..n {
            self.add(key.clone());
        }
    }
}

#[doc(hidden)]
pub fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let shared = Arc::new(shared::build(cx, cfg)?);
    let mut misses = FormMisses::default();
    let mut stack: Vec<&mut MdObject> = cfg.objects.iter_mut().collect();
    while let Some(o) = stack.pop() {
        if o.kind == MdKind::CommonForm {
            o.form = load_form(cx, o.id, &shared, &mut misses);
        }
        for f in &mut o.forms {
            if f.kind == SubKind::Form {
                f.form = load_form(cx, f.id, &shared, &mut misses);
            }
        }
        stack.extend(o.subsystems.iter_mut());
    }
    for (key, count) in misses.0 {
        let n = cfg.diagnostics.misses.entry(key).or_insert(0);
        *n = n.saturating_add(count);
    }
    Ok(())
}

pub(crate) mod tables;
