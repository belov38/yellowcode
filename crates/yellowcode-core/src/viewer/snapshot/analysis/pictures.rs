use super::{SelectedAnalysis, SelectedFormError};
use crate::viewer::analysis_parent::{AnalysisError, FormXmlLease, PictureLease};
use mdconf::{ManagedEntrySource, MdKind, startup::ViewerDataIdentity};
use serde::Serialize;
use std::time::{Duration, Instant};
#[derive(Serialize)]
struct Metadata<'a> {
    op: &'static str,
    platform: &'a str,
    snapshot: ViewerDataIdentity,
    ext: &'a str,
}
impl<S: ManagedEntrySource> SelectedAnalysis<'_, S> {
    fn picture_answer(&self) -> SelectedAnalysis<'_, S> {
        SelectedAnalysis {
            snapshot: self.snapshot,
            parent: self.parent.for_picture_answer(),
            canceled: self.canceled,
        }
    }
    pub fn configuration_picture_projection(
        &self,
        name: &str,
    ) -> Result<Option<PictureLease>, SelectedFormError<S::Error>> {
        self.picture_answer()
            .configuration_picture_before(name, Instant::now() + Duration::from_secs(120))
    }
    pub(super) fn configuration_picture_before(
        &self,
        name: &str,
        deadline: Instant,
    ) -> Result<Option<PictureLease>, SelectedFormError<S::Error>> {
        self.check_before(deadline)?;
        let open = self.snapshot.open();
        let rows = open.objects();
        let mut by_name = None;
        for roots in [true, false] {
            for row in rows {
                self.check_before(deadline)?;
                if row.parent.is_none() == roots
                    && row.object.kind == MdKind::CommonPicture
                    && row.object.name == name
                {
                    by_name = Some(row.object.id);
                    break;
                }
            }
            if by_name.is_some() {
                break;
            }
        }
        if let Some(id) = by_name
            && let Some(result) = self.common_picture_before(id, deadline)?
        {
            return Ok(Some(result));
        }
        if let Some(id) = brace::Guid::parse(name) {
            let mut found = None;
            for row in rows {
                self.check_before(deadline)?;
                if row.object.id == id {
                    found = Some(row.object.kind);
                    break;
                }
            }
            if found == Some(MdKind::CommonPicture)
                && let Some(result) = self.common_picture_before(id, deadline)?
            {
                return Ok(Some(result));
            }
        }
        let ordinal = match name {
            "Logo" => Some(0),
            "Splash" => Some(1),
            "MainSectionPicture" => Some(2),
            _ => None,
        };
        if let Some(ordinal) = ordinal {
            let selected = open
                .context()
                .read_configuration_picture(ordinal)?
                .ok_or(AnalysisError::Protocol)?;
            self.check_before(deadline)?;
            if let Some(picture) = selected.value.as_ref() {
                return self
                    .picture_bytes_before(&picture.ext, &picture.data, deadline)
                    .map(Some);
            }
        }
        self.check_before(deadline)?;
        Ok(None)
    }
    fn common_picture_before(
        &self,
        id: brace::Guid,
        deadline: Instant,
    ) -> Result<Option<PictureLease>, SelectedFormError<S::Error>> {
        self.check_before(deadline)?;
        let selected = self
            .snapshot
            .open()
            .context()
            .read_common_picture(id)?
            .ok_or(AnalysisError::Protocol)?;
        self.check_before(deadline)?;
        match selected.value.as_ref() {
            Some(picture) => self
                .picture_bytes_before(&picture.ext, &picture.data, deadline)
                .map(Some),
            None => {
                self.check_before(deadline)?;
                Ok(None)
            }
        }
    }
    fn picture_bytes_before(
        &self,
        ext: &str,
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<PictureLease, SelectedFormError<S::Error>> {
        self.check_before(deadline)?;
        let metadata = Metadata {
            op: "picture_resource",
            platform: self.snapshot.catalog.platform(),
            snapshot: self.snapshot.open().data_identity(),
            ext,
        };
        let result = self
            .parent
            .picture_resource_before(&metadata, bytes, deadline)?;
        self.check_before(deadline)?;
        Ok(result)
    }
    pub fn embedded_picture_projection(
        &self,
        form: &FormXmlLease,
        name: &str,
    ) -> Result<Option<PictureLease>, SelectedFormError<S::Error>> {
        self.picture_answer().embedded_picture_before(
            form,
            name,
            Instant::now() + Duration::from_secs(120),
        )
    }
    pub(super) fn embedded_picture_before(
        &self,
        form: &FormXmlLease,
        name: &str,
        deadline: Instant,
    ) -> Result<Option<PictureLease>, SelectedFormError<S::Error>> {
        self.check_before(deadline)?;
        if ui_style::package::valid_path(name) {
            let projection = form.view()?;
            self.check_before(deadline)?;
            if let mdconf::forms::wire::Outcome::Present(present) = projection.outcome {
                for (path, bytes) in present.pictures() {
                    self.check_before(deadline)?;
                    if path == name {
                        let ext = name.rsplit('.').next().ok_or(AnalysisError::Protocol)?;
                        return self.picture_bytes_before(ext, bytes, deadline).map(Some);
                    }
                }
            }
        }
        self.check_before(deadline)?;
        Ok(None)
    }
}
