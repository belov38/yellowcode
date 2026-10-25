use crate::{Configuration, MdKind};
use brace::Guid;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Picture {
    pub data: Vec<u8>,
    pub ext: String,
    pub load_transparent: bool,
    pub transparent_pixel: Option<(i64, i64)>,
}

impl Configuration {
    pub fn common_picture(&self, name: &str) -> Option<&Picture> {
        self.find(MdKind::CommonPicture, name)?
            .parts
            .picture
            .as_ref()
    }

    pub fn common_picture_by_id(&self, id: Guid) -> Option<&Picture> {
        let o = self.object(id)?;
        if o.kind != MdKind::CommonPicture {
            return None;
        }
        o.parts.picture.as_ref()
    }

    pub fn config_picture(&self, name: &str) -> Option<&Picture> {
        self.parts
            .pictures
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p)
    }
}
