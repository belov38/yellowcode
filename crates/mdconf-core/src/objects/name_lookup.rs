use super::codecs::{self, Ctx, Owner};
use crate::index::Index;
use brace::{Guid, Node};
use std::cell::OnceCell;
use std::collections::HashMap;

pub(crate) struct NameLookup<'a> {
    ids: HashMap<&'a str, Vec<Guid>>,
    #[cfg(test)]
    pub(crate) index: &'a Index,
}

impl<'a> NameLookup<'a> {
    pub(crate) fn new(index: &'a Index) -> Option<Self> {
        let mut ids: HashMap<&'a str, Vec<Guid>> = HashMap::new();
        ids.try_reserve(index.names.len()).ok()?;
        for (id, name) in &index.names {
            let same = ids.entry(name.as_str()).or_default();
            same.try_reserve(1).ok()?;
            same.push(*id);
        }
        Some(Self {
            ids,
            #[cfg(test)]
            index,
        })
    }

    pub(crate) fn once<'c>(cell: &'c OnceCell<Self>, index: &'a Index) -> Option<&'c Self> {
        if cell.get().is_none() {
            let _ = cell.set(Self::new(index)?);
        }
        cell.get()
    }

    pub(crate) fn tabular_parent(
        &self,
        ctx: &Ctx<'_>,
        owner: &Owner,
        wanted: &str,
        segment: &Node,
    ) -> Result<Option<Node>, ()> {
        let Some([head, id]) = segment.as_list() else {
            return Ok(None);
        };
        if head.text() != Some("0") {
            return Ok(None);
        }
        let Some(name) = id
            .text()
            .and_then(Guid::parse)
            .and_then(|g| ctx.index.names.get(&g))
        else {
            return Ok(None);
        };
        let Some((object, rest)) = name.split_once(".TabularSection.") else {
            return Ok(None);
        };
        let (Some((t, a)), Some((kind, obj))) =
            (rest.split_once(".Attribute."), object.split_once('.'))
        else {
            return Ok(None);
        };
        if [kind, obj, t, a].iter().any(|p| p.is_empty())
            || [obj, t, a].iter().any(|p| p.contains('.'))
        {
            return Ok(None);
        }
        let candidates = self.ids(&format!("{object}.TabularSection.{t}"));
        let Some(first) = candidates.first() else {
            return Ok(None);
        };
        let ts = Node::list(vec![Node::int(0), Node::guid(*first)]);
        if codecs::field_path(ctx, owner, &[ts.clone(), segment.clone()]).as_deref() != Some(wanted)
        {
            return Ok(None);
        }
        match candidates {
            [_] => Ok(Some(ts)),
            _ => Err(()),
        }
    }

    pub(crate) fn ids(&self, name: &str) -> &[Guid] {
        self.ids.get(name).map_or(&[], Vec::as_slice)
    }
}
