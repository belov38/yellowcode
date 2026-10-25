use crate::entries::{Entries, EntryData};
use crate::index::Index;
use brace::Guid;
use std::collections::HashMap;

pub(crate) use md_xmlout::codec::predefined::*;

pub(crate) fn predefined_tables(e: &Entries, ix: &Index) -> HashMap<Guid, HashMap<Guid, String>> {
    let mut out = HashMap::new();
    for u in &ix.order {
        let Some((kind, _)) = ix.objs.get(u) else {
            continue;
        };
        let Some(suffix) = table_suffix(*kind) else {
            continue;
        };
        if let Ok(Some(d)) = e.doc(&format!("{u}.{suffix}"))
            && let EntryData::Brace(doc) = &*d
        {
            let mut items = HashMap::new();
            index_table(*kind, &doc.root, &mut items);
            out.insert(*u, items);
        }
    }
    out
}

pub fn predefined_names(e: &Entries, ix: &Index) -> HashMap<Guid, String> {
    let mut out = HashMap::new();
    for u in &ix.order {
        let Some((kind, _)) = ix.objs.get(u) else {
            continue;
        };
        let Some(suffix) = table_suffix(*kind) else {
            continue;
        };
        if let Ok(Some(d)) = e.doc(&format!("{u}.{suffix}"))
            && let EntryData::Brace(doc) = &*d
        {
            index_table(*kind, &doc.root, &mut out);
        }
    }
    out
}
