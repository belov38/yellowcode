use super::{SelectedAnalysis, SelectedAnalysisError};
use crate::viewer::{analysis_parent::AnalysisError, body::ViewerBody};
use form_model::{
    ResolverOwner, preview_answers,
    preview_wire::{Request, Requests, WriteError},
};
use mdconf::{ManagedEntrySource, startup::ManagedSelectionError};
use memory_budget::{BudgetError, BudgetedVec, BudgetedWriter, Category};
use std::time::{Duration, Instant};

struct Row<'a> {
    ordinal: usize,
    request: Request<'a>,
    owner: Option<ResolverOwner<'a>>,
}
impl Row<'_> {
    fn key(&self) -> Option<(&'static str, &str)> {
        self.owner.map(|owner| (owner.kind.as_str(), owner.name))
    }
}
fn encoded(error: WriteError<AnalysisError>) -> AnalysisError {
    match error {
        WriteError::Sink(error) => error,
        WriteError::Overflow => BudgetError::Overflow.into(),
    }
}
impl<S: ManagedEntrySource> SelectedAnalysis<'_, S> {
    pub fn preview_type_answers(
        &self,
        requests: Requests<'_>,
        wire_limit: u64,
    ) -> Result<ViewerBody, SelectedAnalysisError<S::Error>> {
        self.preview_type_answers_before(
            requests,
            wire_limit,
            Instant::now() + Duration::from_secs(120),
        )
    }
    pub(super) fn preview_type_answers_before(
        &self,
        requests: Requests<'_>,
        wire_limit: u64,
        deadline: Instant,
    ) -> Result<ViewerBody, SelectedAnalysisError<S::Error>> {
        self.check_before(deadline)?;
        let budget = self.snapshot.catalog.memory_budget();
        let mut rows = BudgetedVec::try_with_capacity(budget, Category::Metadata, requests.len())
            .map_err(AnalysisError::from)?;
        let mut path = BudgetedVec::new(budget, Category::Metadata).map_err(AnalysisError::from)?;
        for (ordinal, request) in requests.iter().enumerate() {
            self.check_before(deadline)?;
            let request = request.map_err(|_| AnalysisError::Protocol)?;
            self.preview_segments(request, &mut path, deadline)?;
            rows.try_push(Row {
                ordinal,
                request,
                owner: ResolverOwner::parse(request.owner, path.as_slice()),
            })
            .map_err(AnalysisError::from)?;
        }
        rows.as_mut_slice()
            .sort_unstable_by(|a, b| a.key().cmp(&b.key()));
        self.check_before(deadline)?;
        let mut output = BudgetedWriter::new(budget, wire_limit).map_err(AnalysisError::from)?;
        let mut sink = |bytes: &[u8]| -> Result<(), AnalysisError> {
            self.check_before(deadline)?;
            output.try_write(bytes)?;
            self.check_before(deadline)
        };
        preview_answers::encode_header(requests.len(), &mut sink).map_err(encoded)?;
        let open = self.snapshot.open();
        let mut begin = 0;
        while let Some(first) = rows.get(begin) {
            self.check_before(deadline)?;
            let mut end = begin + 1;
            while rows.get(end).is_some_and(|row| row.key() == first.key()) {
                self.check_before(deadline)?;
                end += 1;
            }
            let mut found = None;
            if let Some(owner) = first.owner {
                for roots in [true, false] {
                    for (ordinal, resident) in open.objects().iter().enumerate() {
                        self.check_before(deadline)?;
                        if resident.parent.is_none() == roots
                            && resident.object.kind == owner.kind
                            && resident.object.name == owner.name
                        {
                            found = Some(ordinal);
                            break;
                        }
                    }
                    if found.is_some() {
                        break;
                    }
                }
            }
            let selected = match found {
                Some(ordinal) => Some(
                    open.read_selected_object(
                        open.object_key(ordinal)
                            .ok_or(ManagedSelectionError::MissingValidated)?,
                    )?,
                ),
                None => None,
            };
            self.check_before(deadline)?;
            for row in rows.get(begin..end).ok_or(AnalysisError::Protocol)? {
                self.check_before(deadline)?;
                self.preview_segments(row.request, &mut path, deadline)?;
                let value = match (row.owner, selected.as_ref()) {
                    (Some(owner), Some(selected)) => {
                        if selected.object.kind != owner.kind || selected.object.name != owner.name
                        {
                            return Err(ManagedSelectionError::MissingValidated.into());
                        }
                        owner.resolve_selected(&selected.object, path.as_slice())
                    }
                    _ => None,
                };
                preview_answers::encode_answer(row.ordinal, value, &mut sink).map_err(encoded)?;
                self.check_before(deadline)?;
            }
            drop(selected);
            self.check_before(deadline)?;
            begin = end;
        }
        self.check_before(deadline)?;
        Ok(ViewerBody::Owned(output.into_buffer()))
    }
    fn preview_segments<'a>(
        &self,
        request: Request<'a>,
        path: &mut BudgetedVec<&'a str>,
        deadline: Instant,
    ) -> Result<(), AnalysisError> {
        path.clear();
        for segment in request.segments() {
            self.check_before(deadline)?;
            path.try_push(segment.map_err(|_| AnalysisError::Protocol)?)?;
        }
        self.check_before(deadline)
    }
}
