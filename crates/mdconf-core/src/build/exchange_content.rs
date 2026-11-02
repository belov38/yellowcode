use super::form_modules::EncodedChange;
use crate::model::{Configuration, MdObject};
use crate::parts::{ContentItem, parse_content};
use crate::{EntrySource, Error, MdKind, Result};
use brace::{Guid, Node};
use std::collections::{HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

const CONTENT_INFLATED_LIMIT: u64 = 16 << 20;
const CONTENT_STORED_LIMIT: usize = 16 << 20;

pub(super) const UNSUPPORTED: &str = "edit of parts.exchange_content: only AutoRecord of existing items is \
                                      written (adding, removing, replacing, renaming or reordering items is not)";

#[derive(Default)]
pub(super) struct Prepared<'a> {
    plans: Vec<(&'a MdObject, &'a MdObject)>,
    changes: Vec<EncodedChange>,
}

fn refuse(entry: &str, what: &str) -> Error {
    Error::Build {
        object: entry.into(),
        path: "parts.exchange_content".into(),
        what: what.into(),
    }
}

fn flags_only(m: &MdObject, b: &MdObject) -> bool {
    let (Some(m), Some(b)) = (&m.parts.exchange_content, &b.parts.exchange_content) else {
        return false;
    };
    m.len() == b.len()
        && m.iter().zip(b).all(|(x, y)| x.object == y.object)
        && m.iter().zip(b).any(|(x, y)| x.auto_record != y.auto_record)
}

impl<'a> Prepared<'a> {
    pub fn prepare(
        model: &'a Configuration,
        original: &'a Configuration,
        base: &dyn EntrySource,
    ) -> Result<Self> {
        let mut prepared = Self::default();
        for (o, b) in model.objects.iter().zip(&original.objects) {
            if o.id != b.id || o.kind != MdKind::ExchangePlan || b.kind != MdKind::ExchangePlan {
                continue;
            }
            if flags_only(o, b) {
                prepared.changes.push(encode(model, original, o, b, base)?);
                prepared.plans.push((o, b));
            }
        }
        Ok(prepared)
    }

    pub fn permits(&self, plan: &MdObject, original_plan: &MdObject) -> bool {
        self.plans
            .iter()
            .any(|(o, b)| std::ptr::eq(*o, plan) && std::ptr::eq(*b, original_plan))
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

fn flag_positions(root: &Node) -> std::result::Result<HashMap<Guid, usize>, &'static str> {
    let list = root.as_list().ok_or("source is not a list")?;
    if list.first().and_then(Node::as_atom) != Some("2") {
        return Err("unsupported source version or token type");
    }
    let n = list
        .get(1)
        .and_then(Node::as_atom)
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or("source count is not a nonnegative integer atom")?;
    if list.get(1) != Some(&Node::atom(n.to_string())) || n > list.len().saturating_sub(2) / 2 {
        return Err("source count is noncanonical or exceeds available pairs");
    }
    let mut positions = HashMap::new();
    for k in 0..n {
        let at = 2 + 2 * k;
        let id = list
            .get(at)
            .and_then(Node::as_atom)
            .and_then(Guid::parse)
            .ok_or("source reference is not a UUID atom")?;
        if positions.insert(id, at + 1).is_some() {
            return Err("duplicate source UUID");
        }
        if !matches!(list.get(at + 1).and_then(Node::as_atom), Some("0" | "1")) {
            return Err("source AutoRecord requires a 0/1 atom");
        }
    }
    match list.get(2 + 2 * n..).unwrap_or_default() {
        [] => {}
        [x] if x.as_atom() == Some("0") => {}
        _ => return Err("unsupported source tail"),
    }
    Ok(positions)
}

fn encode(
    model: &Configuration,
    original: &Configuration,
    plan: &MdObject,
    original_plan: &MdObject,
    base: &dyn EntrySource,
) -> Result<EncodedChange> {
    let entry = format!("{}.1", plan.id);
    let bad = |what: &str| refuse(&entry, what);
    let (Some(desired), Some(current)) =
        (plan.exchange_content(), original_plan.exchange_content())
    else {
        return Err(bad("typed content absent"));
    };
    let version = |cfg: &Configuration| -> Option<Guid> {
        let mut hits = cfg
            .parts
            .dump_info
            .as_ref()?
            .versions
            .iter()
            .filter(|(name, _)| *name == entry);
        match (hits.next(), hits.next()) {
            (Some((_, guid)), None) => Some(*guid),
            _ => None,
        }
    };
    if model.parts.dump_info.is_none() || original.parts.dump_info.is_none() {
        return Err(bad(
            "content entry version unavailable: the model or base model has no versions (load with every codec stage)",
        ));
    }
    match (version(model), version(original)) {
        (Some(m), Some(b)) if m == b => {}
        (Some(_), Some(_)) => {
            return Err(bad(
                "content entry version differs from the base: the base is not the CF the model was loaded from",
            ));
        }
        _ => return Err(bad("content entry version absent or not unique")),
    }
    let stored = base
        .stored_bounded(&entry, CONTENT_STORED_LIMIT)?
        .ok_or_else(|| Error::MissingEntry(entry.clone()))?;
    let raw = zraw::inflate(&stored, CONTENT_INFLATED_LIMIT).map_err(|e| match e {
        zraw::Error::TooLarge(_) => bad("content entry inflates over the limit"),
        e => refuse(&entry, &format!("invalid deflated content entry: {e}")),
    })?;
    let doc = brace::parse_doc(&raw).map_err(|_| bad("strict content document required"))?;
    if brace::write_doc(&doc) != raw {
        return Err(bad("content entry spelling is not reproducible"));
    }
    let positions = flag_positions(&doc.root).map_err(bad)?;
    let typed = |cfg: &Configuration, root: &Node| -> Result<Vec<ContentItem>> {
        parse_content(cfg, root)
            .map_err(|why| refuse(&entry, &format!("typed content reader refused: {why}")))
    };
    if typed(original, &doc.root)? != current {
        return Err(bad(
            "content entry differs from the base model content (the base changed after load)",
        ));
    }
    let mut replacement = doc;
    let list = replacement
        .root
        .as_list_mut()
        .ok_or_else(|| bad("source is not a list"))?;
    for (wanted, now) in desired.iter().zip(current) {
        if wanted.auto_record == now.auto_record {
            continue;
        }
        let at = wanted
            .object
            .id
            .and_then(|id| positions.get(&id))
            .ok_or_else(|| bad("desired UUID absent from source"))?;
        *list
            .get_mut(*at)
            .ok_or_else(|| bad("source flag position absent"))? =
            Node::atom(if wanted.auto_record { "1" } else { "0" });
    }
    let bytes = brace::write_doc(&replacement);
    let parsed = brace::parse_doc(&bytes).map_err(|_| bad("encoded content does not parse"))?;
    if parsed != replacement || typed(model, &parsed.root)? != desired {
        return Err(bad(
            "encoded content does not reproduce the desired typed values",
        ));
    }
    let stored = zraw::deflate(&bytes, zraw::LEVEL_1C).map_err(|e| Error::Format {
        entry: entry.clone(),
        what: e.to_string(),
    })?;
    Ok(EncodedChange {
        entry,
        stored,
        what: "exchange plan content AutoRecord",
        path: "exchange plan content",
    })
}

fn name_key(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

pub fn set_auto_record(
    model: &mut Configuration,
    plan: &str,
    object: &str,
    auto_record: bool,
) -> Result<()> {
    let refuse = |what: String| Error::Build {
        object: plan.to_string(),
        path: "Content.AutoRecord".into(),
        what,
    };
    let want_plan = name_key(plan);
    let want_object = name_key(object);
    let mut plans = model.objects.iter_mut().filter(|o| {
        o.kind == MdKind::ExchangePlan
            && name_key(&format!("{}.{}", o.kind.as_str(), o.name)) == want_plan
    });
    let target = match (plans.next(), plans.next()) {
        (Some(target), None) => target,
        (None, _) => return Err(refuse("unknown exchange plan".into())),
        (Some(_), Some(_)) => return Err(refuse("ambiguous exchange plan".into())),
    };
    let items = target
        .parts
        .exchange_content
        .as_mut()
        .ok_or_else(|| refuse("exchange plan without typed content".into()))?;
    let mut hits = items
        .iter_mut()
        .filter(|i| name_key(&i.object.name) == want_object);
    match (hits.next(), hits.next()) {
        (Some(item), None) => {
            item.auto_record = auto_record;
            Ok(())
        }
        (None, _) => Err(refuse(format!(
            "{object} is not in the content (adding items is not supported)"
        ))),
        (Some(_), Some(_)) => Err(refuse(format!("{object} is in the content twice"))),
    }
}
