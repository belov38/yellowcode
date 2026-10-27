use crate::{Configuration, startup::ManagedIndex};
use brace::Guid;

pub trait Lookup {
    fn name(&self, id: &Guid) -> Option<&str>;
    fn type_name(&self, id: &Guid) -> Option<&str>;
    fn sub_header(&self, id: &Guid) -> Option<&[usize]>;
    fn common_picture(&self, name: &str) -> bool;
}
impl Lookup for Configuration {
    fn name(&self, id: &Guid) -> Option<&str> {
        self.index.names.get(id).map(String::as_str)
    }
    fn type_name(&self, id: &Guid) -> Option<&str> {
        self.index.types.get(id).map(String::as_str)
    }
    fn sub_header(&self, id: &Guid) -> Option<&[usize]> {
        self.index.sub_hdr.get(id).map(Vec::as_slice)
    }
    fn common_picture(&self, name: &str) -> bool {
        self.index
            .names
            .values()
            .any(|value| value.strip_prefix("CommonPicture.") == Some(name))
    }
}
impl Lookup for ManagedIndex {
    fn name(&self, id: &Guid) -> Option<&str> {
        self.names.get(id).map(|n| n.as_str())
    }
    fn type_name(&self, id: &Guid) -> Option<&str> {
        self.types.get(id).map(|n| n.as_str())
    }
    fn sub_header(&self, id: &Guid) -> Option<&[usize]> {
        self.sub_hdr.get(id).map(|p| p.as_slice())
    }
    fn common_picture(&self, name: &str) -> bool {
        self.names
            .iter()
            .any(|(_, value)| value.as_str().strip_prefix("CommonPicture.") == Some(name))
    }
}
pub(crate) fn std_picture(key: &str) -> Option<&'static str> {
    ui("std_pictures", key).or_else(|| {
        mdschema::static_schema()
            .table("std_pictures.json")
            .and_then(|table| table.get("std_pictures"))
            .and_then(|values| values.get(key))
            .and_then(|value| value.as_str())
    })
}
pub(crate) fn mxl(table: &str, key: &str) -> Option<&'static str> {
    mdschema::static_schema()
        .table("templates/tables.json")
        .and_then(|tables| tables.get(table))
        .and_then(|values| values.get(key))
        .and_then(|value| value.as_str())
}
pub(crate) fn has_mxl_object(table: &str) -> bool {
    mdschema::static_schema()
        .table("templates/tables.json")
        .and_then(|tables| tables.get(table))
        .and_then(|value| value.as_object())
        .is_some()
}
pub(crate) fn ui(table: &str, key: &str) -> Option<&'static str> {
    if !["web_colors", "win_colors", "style_colors", "std_pictures"].contains(&table) {
        return None;
    }
    mdschema::static_schema()
        .table("ui.json")
        .and_then(|tables| tables.get(table))
        .and_then(|values| values.get(key))
        .and_then(|value| value.as_str())
}
pub(crate) fn merged(table: &str, key: &str) -> Option<&'static str> {
    mxl(table, key).or_else(|| ui(table, key))
}
