use super::{SelectedAnalysis, SelectedFormError};
use crate::viewer::{
    analysis_parent::AnalysisError,
    body::ViewerBody,
    path::{self, ManagedTarget},
};
use form_model::autoform::facts;
use mdconf::{ManagedEntrySource, ScriptVariant, startup::ViewerDataIdentity};
use memory_budget::BudgetedWriter;
use serde::Serialize;
use std::time::{Duration, Instant};
const WIRE_LIMIT: u64 = 256 << 20;
#[derive(Serialize)]
struct Metadata<'a> {
    op: &'static str,
    platform: &'a str,
    snapshot: ViewerDataIdentity,
    object_ordinal: usize,
    lang: &'static str,
}
impl<S: ManagedEntrySource> SelectedAnalysis<'_, S> {
    pub fn generated_roles_body(
        &self,
        path: &str,
    ) -> Result<ViewerBody, SelectedFormError<S::Error>> {
        let deadline = Instant::now() + Duration::from_secs(120);
        let body = self.generated_roles_before(path, deadline)?;
        self.check_before(deadline)?;
        Ok(body)
    }
    fn generated_roles_before(
        &self,
        path: &str,
        deadline: Instant,
    ) -> Result<ViewerBody, SelectedFormError<S::Error>> {
        self.check_before(deadline)?;
        let open = self.snapshot.open();
        let target = path::resolve_managed(open, &self.snapshot.names(), path)?;
        self.check_before(deadline)?;
        let ManagedTarget::Object { ordinal, object } = target else {
            return Ok(ViewerBody::Static(b"[]"));
        };
        let selected = open
            .context()
            .read_object(object.id)?
            .ok_or(AnalysisError::Protocol)?;
        self.check_before(deadline)?;
        let mut source = BudgetedWriter::new(self.snapshot.catalog.memory_budget(), WIRE_LIMIT)
            .map_err(AnalysisError::from)?;
        facts::encode(&*selected.object, |bytes| {
            self.check_before(deadline)?;
            source.try_write(bytes)?;
            self.check_before(deadline)
        })
        .map_err(|error| match error {
            facts::FactWriteError::Sink(error) => error,
            facts::FactWriteError::TooDeep | facts::FactWriteError::Overflow => {
                AnalysisError::Resource
            }
        })?;
        drop(selected);
        self.check_before(deadline)?;
        let metadata = Metadata {
            op: "generated_roles",
            platform: self.snapshot.catalog.platform(),
            snapshot: open.data_identity(),
            object_ordinal: ordinal,
            lang: match open.context().settings().script_variant {
                ScriptVariant::English => "en",
                ScriptVariant::Russian => "ru",
            },
        };
        let body = self
            .parent
            .run_before(&metadata, source.as_slice(), deadline)?;
        self.check_before(deadline)?;
        Ok(body)
    }
}
