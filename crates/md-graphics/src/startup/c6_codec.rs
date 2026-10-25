use crate::codec::FlowchartError;
use crate::graph::{
    self,
    read::{Read, ReadError},
};
use crate::startup::{
    ManagedIndex, ManagedObjectsError, c6_route,
    index::{GuidText, text},
    kind_spec,
};
use crate::templates::{self, Template, TemplateType, allocation::Allocation, context::Lookup};
use crate::{EntryData, ManagedEntries, ManagedEntrySource};
use brace::Guid;
use memory_budget::Charged;

fn template_type<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    index: &ManagedIndex,
    id: Guid,
    common: bool,
) -> Result<Option<TemplateType>, ManagedObjectsError<S::Error>> {
    let name = GuidText::new(id);
    let data = match entries.doc(name.as_str()?) {
        Ok(Some(data)) => data,
        Ok(None) => return Ok(None),
        Err(error) if entries.recoverable_content_error(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let EntryData::Brace(doc) = &*data else {
        return Ok(None);
    };
    let key = if common {
        doc.root.get(&[1, 2])
    } else {
        index
            .sub_header(&id)
            .and_then(|header| header.split_last())
            .and_then(|(_, prefix)| doc.root.get(prefix))
            .and_then(|node| node.at(1))
    }
    .and_then(brace::Node::text);
    let kind = if common { "CommonTemplate" } else { "Template" };
    Ok(key
        .and_then(|key| {
            kind_spec(kind)?
                .props()
                .find(|(name, _)| *name == "TemplateType")?
                .1
                .field("map")?
                .get(key)?
                .as_str()
        })
        .and_then(TemplateType::from_name))
}
#[doc(hidden)]
pub fn one_template<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    index: &ManagedIndex,
    id: Guid,
    common: bool,
    keep: bool,
    diagnostics: &mut crate::startup::ManagedDiagnostics,
) -> Result<Option<Charged<Template>>, ManagedObjectsError<S::Error>> {
    let Some(kind) = template_type(entries, index, id, common)? else {
        return Ok(None);
    };
    let allocation = Allocation::Managed(entries.memory_budget());
    let id = GuidText::new(id);
    let name = text(entries.memory_budget(), &[id.as_str()?, ".0"])?;
    let data = entries.get(name.as_str())?;
    let raw = entries.raw(name.as_str())?;
    let files = match (data.as_deref(), raw.as_deref()) {
        (Some(data), Some(raw)) => {
            match templates::convert_with(index, data, raw, kind, allocation) {
                Ok(files) => files,
                Err(ReadError::Memory(error)) => return Err(error.into()),
                Err(ReadError::Semantic { code, .. }) => {
                    diagnostics.miss(&[
                        "templates:",
                        kind.name(),
                        ":",
                        if code == "path" { "path" } else { "failed" },
                    ])?;
                    allocation.vector(0)?.finish()
                }
            }
        }
        _ => allocation.vector(0)?.finish(),
    };
    if !keep {
        entries.forget(name.as_str());
    }
    Ok(Some(
        files
            .map(|files| Template {
                kind: Some(kind),
                files,
            })
            .managed()?,
    ))
}
#[doc(hidden)]
pub fn flowchart<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    index: &ManagedIndex,
    id: Guid,
    data: &EntryData,
    keep: bool,
) -> crate::codec::FlowchartXml<S::Error> {
    let allocation = Allocation::Managed(entries.memory_budget());
    let read = Read::new(allocation);
    let route = c6_route::task_of(entries, index, id, keep)?;
    Ok(templates::tree::entry(data, allocation)
        .map_err(|error| templates::mxlxml::source_error(error, read))
        .and_then(|tree| graph::gschema::to_xml_with(index, tree.root(), Some(&route), allocation))
        .map_err(|error| match error {
            ReadError::Memory(error) => FlowchartError::Memory(error),
            ReadError::Semantic { .. } => FlowchartError::Semantic,
        }))
}
