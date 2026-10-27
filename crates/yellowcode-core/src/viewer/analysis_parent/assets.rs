use super::{AnalysisError, PictureLease};
use form_model::{
    preview_layout::{Picture, Pictures},
    preview_picture::Image,
};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, MemoryBudget};
struct Asset {
    key: BudgetedText,
    image: Option<PictureLease>,
}
pub struct PictureAssets {
    rows: BudgetedVec<Asset>,
}
impl PictureAssets {
    pub fn collect<E: From<AnalysisError>>(
        budget: &MemoryBudget,
        pictures: Pictures<'_>,
        mut resolve: impl FnMut(Picture<'_>) -> Result<Option<PictureLease>, E>,
        mut check: impl FnMut() -> Result<(), AnalysisError>,
    ) -> Result<Self, E> {
        let memory = |error: BudgetError| E::from(AnalysisError::from(error));
        check().map_err(E::from)?;
        let mut rows: BudgetedVec<Asset> =
            BudgetedVec::new(budget, Category::Metadata).map_err(memory)?;
        let mut total = 0usize;
        for picture in pictures.iter() {
            check().map_err(E::from)?;
            let picture = picture.map_err(|_| E::from(AnalysisError::Protocol))?;
            let (prefix, name) = match picture {
                Picture::Std(_) => continue,
                Picture::Config(name) => ("config:", name),
                Picture::Embedded(name) => ("form:", name),
            };
            let mut seen = false;
            for row in rows.as_slice() {
                check().map_err(E::from)?;
                if row.key.as_str().strip_prefix(prefix) == Some(name) {
                    seen = true;
                    break;
                }
            }
            if seen {
                continue;
            }
            let len = prefix
                .len()
                .checked_add(name.len())
                .ok_or_else(|| memory(BudgetError::Overflow))?;
            let limit = u64::try_from(len).map_err(|_| memory(BudgetError::Overflow))?;
            let mut key = BudgetedText::try_with_capacity(budget, Category::Metadata, limit, len)
                .map_err(memory)?;
            key.try_push_str(prefix).map_err(memory)?;
            key.try_push_str(name).map_err(memory)?;
            rows.try_reserve(1).map_err(memory)?;
            check().map_err(E::from)?;
            let mut image = resolve(picture)?;
            check().map_err(E::from)?;
            if let Some(lease) = image.as_ref() {
                let value = lease.view().map_err(E::from)?;
                check().map_err(E::from)?;
                let next = value
                    .and_then(|value| total.checked_add(value.bytes.len()))
                    .filter(|next| *next <= ui_style::package::MAX_PACKAGE);
                if let Some(next) = next {
                    total = next;
                } else {
                    image = None;
                }
            }
            check().map_err(E::from)?;
            rows.try_push(Asset { key, image }).map_err(memory)?;
        }
        check().map_err(E::from)?;
        Ok(Self { rows })
    }
    pub(super) fn encode(
        &self,
        mut sink: impl FnMut(&[u8]) -> Result<(), AnalysisError>,
    ) -> Result<(), AnalysisError> {
        let count = self
            .rows
            .as_slice()
            .iter()
            .filter(|row| row.image.is_some())
            .count();
        form_model::preview_render::encode_assets(
            count,
            self.iter().map(|row| {
                row.map(|(key, image)| form_model::preview_render::Asset { key, image })
            }),
            &mut sink,
        )
        .map_err(super::preview_encoding)
    }
    pub(super) fn encoded_len(
        &self,
        mut check: impl FnMut() -> Result<(), AnalysisError>,
    ) -> Result<usize, AnalysisError> {
        let mut length = 0usize;
        self.encode(|bytes| {
            check()?;
            length = length
                .checked_add(bytes.len())
                .ok_or(AnalysisError::Resource)?;
            Ok(())
        })?;
        check()?;
        Ok(length)
    }
    pub fn iter(&self) -> impl Iterator<Item = Result<(&str, Image<'_>), AnalysisError>> {
        self.rows.as_slice().iter().filter_map(|row| {
            let lease = row.image.as_ref()?;
            match lease.view() {
                Ok(Some(image)) => Some(Ok((row.key.as_str(), image))),
                Ok(None) => Some(Err(AnalysisError::Protocol)),
                Err(error) => Some(Err(error)),
            }
        })
    }
}
