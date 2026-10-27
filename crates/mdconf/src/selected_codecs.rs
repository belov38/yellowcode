use crate::facade::FullCodecs;
use crate::startup::{
    ManagedConfigurationError, ManagedDiagnostics, ManagedViewerContext, SelectedPart,
};
use crate::{ManagedEntrySource, MdKind, SubKind};
use brace::Guid;
use mdconf_core::{
    codec::base::startup::index::{GuidText, text},
    codec::{FlowchartError, ManagedCodecs},
};

type Result<T, E> = std::result::Result<T, ManagedConfigurationError<E>>;

pub fn read_selected_form<S: ManagedEntrySource>(
    context: &ManagedViewerContext<S>,
    owner: Guid,
    child: Option<Guid>,
) -> Result<Option<SelectedPart<crate::forms::Form>>, S::Error> {
    let index = context.index();
    let Some((kind, Some(_))) = index.objs.get(&owner) else {
        return Ok(None);
    };
    let id = match child {
        None if *kind == MdKind::CommonForm => owner,
        Some(id)
            if index.subs.get(&owner).is_some_and(|subs| {
                subs.iter()
                    .any(|(kind, child)| *kind == SubKind::Form && *child == id)
            }) =>
        {
            id
        }
        _ => return Ok(None),
    };
    let entries = context.entries()?;
    let mut diagnostics = ManagedDiagnostics::new(index.memory_budget())?;
    let shared = FullCodecs.form_state_with_retention(&entries, index, false)?;
    let value = FullCodecs.form(&entries, id, &shared, false, &mut diagnostics)?;
    Ok(Some(SelectedPart { value, diagnostics }))
}

pub fn read_selected_template<S: ManagedEntrySource>(
    context: &ManagedViewerContext<S>,
    owner: Guid,
    child: Option<Guid>,
) -> Result<Option<SelectedPart<crate::templates::Template>>, S::Error> {
    let index = context.index();
    let Some((kind, Some(_))) = index.objs.get(&owner) else {
        return Ok(None);
    };
    let id = match child {
        None if *kind == MdKind::CommonTemplate => owner,
        Some(id)
            if index.subs.get(&owner).is_some_and(|subs| {
                subs.iter()
                    .any(|(kind, child)| *kind == SubKind::Template && *child == id)
            }) =>
        {
            id
        }
        _ => return Ok(None),
    };
    let entries = context.entries()?;
    let mut diagnostics = ManagedDiagnostics::new(index.memory_budget())?;
    let value = FullCodecs.template(
        &entries,
        index,
        id,
        child.is_none(),
        false,
        &mut diagnostics,
    )?;
    Ok(Some(SelectedPart { value, diagnostics }))
}

pub fn read_selected_flowchart<S: ManagedEntrySource>(
    context: &ManagedViewerContext<S>,
    owner: Guid,
) -> Result<Option<SelectedPart<crate::graph::Flowchart>>, S::Error> {
    let index = context.index();
    if !index.order.contains(&owner)
        || !index
            .objs
            .get(&owner)
            .is_some_and(|(kind, name)| *kind == MdKind::BusinessProcess && name.is_some())
    {
        return Ok(None);
    }
    let mut diagnostics = ManagedDiagnostics::new(index.memory_budget())?;
    let entries = context.entries()?;
    let guid = GuidText::new(owner);
    let name = text(index.memory_budget(), &[guid.as_str()?, ".7"])?;
    let data = match entries.get(name.as_str()) {
        Ok(data) => data,
        Err(error) if entries.recoverable_content_error(&error) => {
            diagnostics.miss(&["flowchart:failed"])?;
            return Ok(Some(SelectedPart {
                value: None,
                diagnostics,
            }));
        }
        Err(error) => return Err(error.into()),
    };
    let value = match data {
        None => None,
        Some(data) => match FullCodecs.flowchart(&entries, index, owner, &data, false)? {
            Ok(xml) => Some(xml.map(|xml| crate::graph::Flowchart { xml }).managed()?),
            Err(FlowchartError::Memory(error)) => return Err(error.into()),
            Err(FlowchartError::Semantic) => {
                diagnostics.miss(&["flowchart:failed"])?;
                None
            }
        },
    };
    Ok(Some(SelectedPart { value, diagnostics }))
}
