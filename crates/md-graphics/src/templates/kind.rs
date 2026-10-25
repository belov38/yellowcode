use super::TemplateType;
use crate::Configuration;
use crate::entries::{Entries, EntryData};
use brace::Guid;

pub(crate) fn template_type(
    cfg: &Configuration,
    e: &Entries,
    id: Guid,
    common: bool,
) -> Option<TemplateType> {
    let data = e.doc(&id.to_string()).ok()??;
    let EntryData::Brace(doc) = &*data else {
        return None;
    };
    let mut path: Vec<usize>;
    let kind = if common {
        path = vec![1, 2];
        "CommonTemplate"
    } else {
        let h = cfg.index.sub_hdr.get(&id)?;
        path = h.get(..h.len().checked_sub(1)?)?.to_vec();
        path.push(1);
        "Template"
    };
    let value = doc.root.get(&path)?;
    let key = value.text()?.to_string();
    let spec = mdschema::schema().ok()?.kind(kind)?;
    let (_, prop) = spec.props.iter().find(|(n, _)| n == "TemplateType")?;
    TemplateType::from_name(prop.map.get(&key)?)
}
