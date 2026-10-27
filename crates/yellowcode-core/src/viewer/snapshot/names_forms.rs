use super::names_metadata::{NamesCancellation, check, path};
use super::{ManagedNamesMetadataError, ManagedSnapshot, SelectedFormError};
use crate::viewer::analysis_parent::FormXmlLease;
use mdconf::{ManagedEntrySource, MdKind};
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};

type Result<T, E> = std::result::Result<T, ManagedNamesMetadataError<E>>;
impl<S: ManagedEntrySource> ManagedSnapshot<S> {
    pub fn visit_names_forms(
        &self,
        executable: &Path,
        spill_dir: &Path,
        canceled: &dyn NamesCancellation,
        visit: &mut impl FnMut(&str, &[u8], bool) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        self.visit_names_forms_before(
            executable,
            spill_dir,
            canceled,
            Instant::now() + Duration::from_secs(120),
            visit,
        )
    }
    pub(super) fn visit_names_forms_before(
        &self,
        executable: &Path,
        spill_dir: &Path,
        canceled: &dyn NamesCancellation,
        deadline: Instant,
        visit: &mut impl FnMut(&str, &[u8], bool) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let interrupted = || canceled.is_canceled();
        let deadline = canceled.deadline().map_or(deadline, |at| at.min(deadline));
        let analysis = self
            .analysis(executable, spill_dir, canceled.flag())
            .with_control(Some(deadline), &interrupted);
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            form_check(canceled, deadline)?;
            if row.object.kind == MdKind::CommonForm
                && let Some(lease) = analysis.form_xml_at(ordinal, row.object.id, None, deadline)?
            {
                let directory = self.names_object_dir(ordinal, canceled)?;
                self.visit_form_files(&lease, directory.as_str(), canceled, deadline, visit)?;
            }
            for child in row.object.forms.iter() {
                form_check(canceled, deadline)?;
                if let Some(lease) =
                    analysis.form_xml_at(ordinal, row.object.id, Some(child.id), deadline)?
                {
                    let directory = self.names_object_dir(ordinal, canceled)?;
                    let directory = path(
                        self.catalog.memory_budget(),
                        &[directory.as_str(), "/Forms/", &child.name],
                    )?;
                    self.visit_form_files(&lease, directory.as_str(), canceled, deadline, visit)?;
                }
            }
        }
        form_check(canceled, deadline)
    }
    fn visit_form_files(
        &self,
        lease: &FormXmlLease,
        directory: &str,
        canceled: &dyn NamesCancellation,
        deadline: Instant,
        visit: &mut impl FnMut(&str, &[u8], bool) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        form_check(canceled, deadline)?;
        let projection = lease.view().map_err(SelectedFormError::Native)?;
        let mdconf::forms::wire::Outcome::Present(value) = projection.outcome else {
            return Ok(());
        };
        let budget = self.catalog.memory_budget();
        let key = path(budget, &[directory, "/Ext/Form.xml"])?;
        form_check(canceled, deadline)?;
        visit(key.as_str(), value.xml.as_bytes(), value.deferred)?;
        form_check(canceled, deadline)?;
        drop(key);
        for (name, bytes) in value.pictures() {
            form_check(canceled, deadline)?;
            if mdconf::forms::plain_picture_path(name) {
                let key = path(budget, &[directory, "/Ext/Form/", name])?;
                form_check(canceled, deadline)?;
                visit(key.as_str(), bytes, value.deferred)?;
                form_check(canceled, deadline)?;
            }
        }
        form_check(canceled, deadline)
    }
}

fn form_check<E>(canceled: &dyn NamesCancellation, deadline: Instant) -> Result<(), E> {
    check(canceled)?;
    if Instant::now() >= deadline {
        return Err(SelectedFormError::Native(
            crate::viewer::analysis_parent::AnalysisError::Timeout,
        )
        .into());
    }
    Ok(())
}
