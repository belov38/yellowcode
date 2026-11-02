use crate::codec::StageRunner;
use crate::model::{Configuration, MdObject, Module, SubObject};
use crate::{EntryData, EntrySource, Error, ModuleKind, Result, SubKind, Value};
use md_model::forms::{Form, FormSource};
use std::collections::HashSet;

const BOM: &[u8] = b"\xef\xbb\xbf";

pub(super) struct EncodedChange {
    pub entry: String,
    pub stored: Vec<u8>,
    pub what: &'static str,
    pub path: &'static str,
}

const MODULE_CHANGE: &str = "form module text";
const BODY_CHANGE: &str = "form InputField.Visible";

struct SubPermit<'a> {
    owner: &'a MdObject,
    original_owner: &'a MdObject,
    form: &'a SubObject,
    original_form: &'a SubObject,
}

#[derive(Default)]
pub(super) struct Prepared<'a> {
    subs: Vec<SubPermit<'a>>,
    common: Vec<(&'a MdObject, &'a MdObject)>,
    sub_bodies: Vec<SubPermit<'a>>,
    common_bodies: Vec<(&'a MdObject, &'a MdObject)>,
    changes: Vec<EncodedChange>,
}

fn refuse(entry: &str, what: &str) -> Error {
    Error::Build {
        object: entry.into(),
        path: "form module".into(),
        what: what.into(),
    }
}

fn refuse_body(entry: &str, what: &str) -> Error {
    Error::Build {
        object: entry.into(),
        path: "form body".into(),
        what: what.into(),
    }
}

fn unique(objects: &[MdObject], seen: &mut HashSet<brace::Guid>) -> Result<()> {
    for o in objects {
        if !seen.insert(o.id) {
            return Err(refuse(&o.id.to_string(), "duplicate form owner or target"));
        }
        for f in &o.forms {
            if !seen.insert(f.id) {
                return Err(refuse(&f.id.to_string(), "duplicate form owner or target"));
            }
        }
        unique(&o.subsystems, seen)?;
    }
    Ok(())
}

impl<'a> Prepared<'a> {
    pub fn prepare(
        model: &'a Configuration,
        original: &'a Configuration,
        base: &dyn EntrySource,
        runner: &dyn StageRunner,
    ) -> Result<Self> {
        unique(&model.objects, &mut HashSet::new())?;
        unique(&original.objects, &mut HashSet::new())?;
        let mut prepared = Self::default();
        prepared.objects(&model.objects, &original.objects, base, runner)?;
        Ok(prepared)
    }

    fn objects(
        &mut self,
        model: &'a [MdObject],
        original: &'a [MdObject],
        base: &dyn EntrySource,
        runner: &dyn StageRunner,
    ) -> Result<()> {
        for (o, b) in model.iter().zip(original) {
            if o.id != b.id || o.kind != b.kind {
                continue;
            }
            for (f, q) in o.forms.iter().zip(&b.forms) {
                if f.id == q.id
                    && f.kind == SubKind::Form
                    && q.kind == SubKind::Form
                    && f.module != q.module
                {
                    let (Some(m), Some(n)) = (&f.module, &q.module) else {
                        continue;
                    };
                    if m.kind != ModuleKind::FormModule || n.kind != ModuleKind::FormModule {
                        continue;
                    }
                    self.changes
                        .push(encode(f.id, f.form.as_ref(), q.form.as_ref(), m, n, base)?);
                    self.subs.push(SubPermit {
                        owner: o,
                        original_owner: b,
                        form: f,
                        original_form: q,
                    });
                } else if f.id == q.id
                    && f.kind == SubKind::Form
                    && q.kind == SubKind::Form
                    && f.module == q.module
                    && let Some((form, original_form)) = body_edit(f.form.as_ref(), q.form.as_ref())
                {
                    self.changes.push(encode_body(
                        f.id,
                        form,
                        original_form,
                        base,
                        runner,
                        zraw::DEFAULT_LIMIT,
                    )?);
                    self.sub_bodies.push(SubPermit {
                        owner: o,
                        original_owner: b,
                        form: f,
                        original_form: q,
                    });
                }
            }
            let m: Vec<_> = o
                .modules
                .iter()
                .filter(|m| m.kind == ModuleKind::FormModule)
                .collect();
            let n: Vec<_> = b
                .modules
                .iter()
                .filter(|m| m.kind == ModuleKind::FormModule)
                .collect();
            if m.len() > 1 || n.len() > 1 {
                return Err(refuse(&o.id.to_string(), "duplicate form module"));
            }
            let body = if m == n {
                body_edit(o.form.as_ref(), b.form.as_ref())
            } else {
                None
            };
            if (m != n && (!m.is_empty() || !n.is_empty())) || body.is_some() {
                let has_form_type = mdschema::schema()?
                    .kind(o.kind.as_str())
                    .is_some_and(|k| k.props.iter().any(|(name, _)| name == "FormType"));
                let managed = |o: &MdObject| {
                    o.props
                        .iter()
                        .any(|p| p.name == "FormType" && p.value == Value::Text("Managed".into()))
                };
                let common = has_form_type && managed(o) && managed(b);
                if let Some((form, original_form)) = body
                    && common
                {
                    self.changes.push(encode_body(
                        o.id,
                        form,
                        original_form,
                        base,
                        runner,
                        zraw::DEFAULT_LIMIT,
                    )?);
                    self.common_bodies.push((o, b));
                } else if let ([m], [n]) = (m.as_slice(), n.as_slice())
                    && common
                {
                    self.changes
                        .push(encode(o.id, o.form.as_ref(), b.form.as_ref(), m, n, base)?);
                    self.common.push((o, b));
                }
            }
            self.objects(&o.subsystems, &b.subsystems, base, runner)?;
        }
        Ok(())
    }

    pub fn permits_sub(
        &self,
        owner: &MdObject,
        original_owner: &MdObject,
        form: &SubObject,
        original_form: &SubObject,
    ) -> bool {
        self.subs.iter().any(|p| {
            std::ptr::eq(p.owner, owner)
                && std::ptr::eq(p.original_owner, original_owner)
                && std::ptr::eq(p.form, form)
                && std::ptr::eq(p.original_form, original_form)
        })
    }

    pub fn permits_common(&self, object: &MdObject, original_object: &MdObject) -> bool {
        self.common
            .iter()
            .any(|(o, b)| std::ptr::eq(*o, object) && std::ptr::eq(*b, original_object))
    }

    pub fn permits_sub_body(
        &self,
        owner: &MdObject,
        original_owner: &MdObject,
        form: &SubObject,
        original_form: &SubObject,
    ) -> bool {
        self.sub_bodies.iter().any(|p| {
            std::ptr::eq(p.owner, owner)
                && std::ptr::eq(p.original_owner, original_owner)
                && std::ptr::eq(p.form, form)
                && std::ptr::eq(p.original_form, original_form)
        })
    }

    pub fn permits_common_body(&self, object: &MdObject, original_object: &MdObject) -> bool {
        self.common_bodies
            .iter()
            .any(|(o, b)| std::ptr::eq(*o, object) && std::ptr::eq(*b, original_object))
    }

    pub fn validate_publication(&self, names: &HashSet<&str>) -> Result<()> {
        for change in &self.changes {
            if !names.contains(change.entry.as_str()) {
                return Err(refuse(
                    &change.entry,
                    "prepared target absent from captured publication list",
                ));
            }
        }
        Ok(())
    }

    pub fn into_changes(self) -> Vec<EncodedChange> {
        self.changes
    }
}

fn retained(form: Option<&Form>) -> Option<&brace::Doc> {
    match form?.source.as_ref()? {
        FormSource::Doc(data) => match &**data {
            EntryData::Brace(doc) => Some(doc),
            _ => None,
        },
        _ => None,
    }
}

fn body_edit<'f>(
    form: Option<&'f Form>,
    original: Option<&'f Form>,
) -> Option<(&'f Form, &'f Form)> {
    let (form, original) = (form?, original?);
    let Form {
        items,
        attributes,
        commands,
        parameters,
        events,
        source: _,
        shared: _,
    } = form;
    let same = *items == original.items
        && *attributes == original.attributes
        && *commands == original.commands
        && *parameters == original.parameters
        && *events == original.events;
    (same && form.source != original.source && retained(Some(form)).is_some())
        .then_some((form, original))
}

fn strict_entry(
    stored: &[u8],
    limit: u64,
    bad: impl Fn(&'static str) -> Error,
) -> Result<(Vec<u8>, brace::Doc)> {
    let raw = zraw::inflate(stored, limit).map_err(|_| bad("invalid deflated form entry"))?;
    let doc = brace::parse_doc(&raw).map_err(|_| bad("strict form document required"))?;
    if !doc.bom
        || !doc.lead.is_empty()
        || doc.root.at(0) != Some(&brace::Node::atom("4"))
        || doc.root.get(&[1, 0]) != Some(&brace::Node::atom("59"))
    {
        return Err(bad("strict BOM-prefixed 4/59 form required"));
    }
    Ok((raw, doc))
}

fn encode_body(
    id: brace::Guid,
    form: &Form,
    original_form: &Form,
    base: &dyn EntrySource,
    runner: &dyn StageRunner,
    limit: u64,
) -> Result<EncodedChange> {
    let entry = format!("{id}.0");
    let bad = |what| refuse_body(&entry, what);
    let stored = base
        .stored(&entry)?
        .ok_or_else(|| Error::MissingEntry(entry.clone()))?;
    let (raw, doc) = strict_entry(&stored, limit, bad)?;
    if retained(Some(original_form)) != Some(&doc) {
        return Err(bad("form or retained source differs from current base"));
    }
    let wanted = retained(Some(form)).ok_or_else(|| bad("retained form source required"))?;
    let bytes = runner.edit_form_body(&entry, &raw, wanted)?;
    if brace::parse_doc(&bytes).ok().as_ref() != Some(wanted) {
        return Err(bad("encoded form differs from the retained source"));
    }
    let stored = zraw::deflate(&bytes, zraw::LEVEL_1C).map_err(|e| Error::Format {
        entry: entry.clone(),
        what: e.to_string(),
    })?;
    Ok(EncodedChange {
        entry,
        stored,
        what: BODY_CHANGE,
        path: "form body",
    })
}

fn body_only(wanted: &brace::Doc, base: &brace::Doc) -> bool {
    let (Some(body), Some(base_body)) = (wanted.root.at(1), base.root.at(1)) else {
        return false;
    };
    if body == base_body {
        return false;
    }
    let mut rest = wanted.clone();
    match rest.root.get_mut(&[1]) {
        Some(slot) => *slot = base_body.clone(),
        None => return false,
    }
    rest == *base
}

fn projection(doc: &brace::Doc) -> Option<Vec<u8>> {
    let text = doc.root.at(2)?.as_str()?;
    if text.is_empty() {
        return None;
    }
    let mut bytes = BOM.to_vec();
    bytes.extend_from_slice(text.as_bytes());
    Some(bytes)
}

fn encode(
    id: brace::Guid,
    form: Option<&Form>,
    original_form: Option<&Form>,
    module: &Module,
    original_module: &Module,
    base: &dyn EntrySource,
) -> Result<EncodedChange> {
    let entry = format!("{id}.0");
    let bad = |what| refuse(&entry, what);
    let stored = base
        .stored(&entry)?
        .ok_or_else(|| Error::MissingEntry(entry.clone()))?;
    let (_, doc) = strict_entry(&stored, zraw::DEFAULT_LIMIT, bad)?;
    if retained(original_form) == Some(&doc)
        && body_edit(form, original_form).is_some()
        && retained(form).is_some_and(|wanted| body_only(wanted, &doc))
    {
        return Err(bad(
            "module and body of the same form both changed (edit them in separate builds)",
        ));
    }
    if retained(form) != Some(&doc)
        || retained(original_form) != Some(&doc)
        || form != original_form
    {
        return Err(bad("form or retained source differs from current base"));
    }
    if projection(&doc).as_deref() != Some(original_module.source.as_slice()) {
        return Err(bad(
            "original form module projection differs from current base",
        ));
    }
    let text = module
        .source
        .strip_prefix(BOM)
        .and_then(|b| std::str::from_utf8(b).ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("nonempty BOM-prefixed UTF-8 module required"))?;
    let mut replacement = doc;
    let target = replacement
        .root
        .get_mut(&[2])
        .ok_or_else(|| bad("missing form module string"))?;
    *target = brace::Node::str(text);
    let bytes = brace::write_doc(&replacement);
    let parsed = brace::parse_doc(&bytes).map_err(|_| bad("encoded form does not parse"))?;
    if parsed != replacement || projection(&parsed).as_deref() != Some(module.source.as_slice()) {
        return Err(bad("encoded form or module projection differs"));
    }
    let stored = zraw::deflate(&bytes, zraw::LEVEL_1C).map_err(|e| Error::Format {
        entry: entry.clone(),
        what: e.to_string(),
    })?;
    Ok(EncodedChange {
        entry,
        stored,
        what: MODULE_CHANGE,
        path: "form module",
    })
}
