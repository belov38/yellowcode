use super::SelectedAnalysis;
use crate::viewer::{
    analysis_parent::{AnalysisError, FormXmlLease},
    path::{self, ManagedResolveError, ManagedTarget},
};
use mdconf::{
    ManagedEntrySource, MdKind, SubKind,
    startup::{ManagedConfigurationError, ViewerDataIdentity},
};
use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum SelectedFormError<E> {
    #[error("{0}")]
    Path(#[from] ManagedResolveError),
    #[error("{0}")]
    Model(#[from] ManagedConfigurationError<E>),
    #[error("{0}")]
    Native(#[from] AnalysisError),
}
#[derive(Serialize)]
struct Metadata<'a> {
    op: &'static str,
    platform: &'a str,
    snapshot: ViewerDataIdentity,
    form_ordinal: usize,
    owner: Option<&'a str>,
    shared: bool,
}
impl<S: ManagedEntrySource> SelectedAnalysis<'_, S> {
    pub fn form_xml_projection(
        &self,
        path: &str,
    ) -> Result<Option<FormXmlLease>, SelectedFormError<S::Error>> {
        let deadline = Instant::now() + Duration::from_secs(120);
        self.check_before(deadline)?;
        let open = self.snapshot.open();
        let target = path::resolve_managed(open, &self.snapshot.names(), path)?;
        let Some(FormTarget {
            ordinal,
            owner,
            child,
        }) = form_target(target, || self.check_before(deadline))?
        else {
            return Ok(None);
        };
        self.form_xml_at(ordinal, owner, child, deadline)
    }
    pub(in crate::viewer::snapshot) fn form_xml_at(
        &self,
        ordinal: usize,
        owner: brace::Guid,
        child: Option<brace::Guid>,
        deadline: Instant,
    ) -> Result<Option<FormXmlLease>, SelectedFormError<S::Error>> {
        self.check_before(deadline)?;
        let open = self.snapshot.open();
        let id = child.unwrap_or(owner);
        let context = open.context();
        let selected = mdconf::startup::read_selected_form(context, owner, child)?;
        self.check_before(deadline)?;
        let Some(form) = selected.as_ref().and_then(|v| v.value.as_ref()) else {
            return Ok(None);
        };
        let facts = mdconf::startup::write_form_facts_checked(context, form, || {
            self.check_before(deadline)
        })?;
        let source = form
            .source
            .as_ref()
            .and_then(|s| s.bytes())
            .ok_or(AnalysisError::Protocol)?;
        let owner = context
            .index()
            .names
            .get(&id)
            .and_then(|n| n.as_str().rsplit_once(".Form."))
            .map(|(owner, _)| owner);
        let metadata = Metadata {
            op: "form_xml",
            platform: self.snapshot.catalog.platform(),
            snapshot: open.data_identity(),
            form_ordinal: ordinal,
            owner,
            shared: facts.has_shared,
        };
        let result = self
            .parent
            .form_xml_before(&metadata, source, facts.bytes(), deadline)?;
        self.check_before(deadline)?;
        Ok(Some(result))
    }
}

#[derive(Debug)]
struct FormTarget {
    ordinal: usize,
    owner: brace::Guid,
    child: Option<brace::Guid>,
}

fn form_target<E>(
    target: ManagedTarget<'_>,
    check: impl FnOnce() -> Result<(), AnalysisError>,
) -> Result<Option<FormTarget>, SelectedFormError<E>> {
    check()?;
    let selected = match target {
        ManagedTarget::Object { ordinal, object } if object.kind == MdKind::CommonForm => {
            (ordinal, object.id, None)
        }
        ManagedTarget::Sub {
            ordinal,
            owner,
            sub,
        } if sub.kind == SubKind::Form => (ordinal, owner.id, Some(sub.id)),
        _ => return Ok(None),
    };
    let (ordinal, owner, child) = selected;
    Ok(Some(FormTarget {
        ordinal,
        owner,
        child,
    }))
}
