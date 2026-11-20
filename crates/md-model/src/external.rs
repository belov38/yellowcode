use crate::{Diagnostics, MdKind, MdObject};
use brace::Guid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalKind {
    DataProcessor,
    Report,
}
impl ExternalKind {
    pub fn xml_name(self) -> &'static str {
        match self {
            Self::DataProcessor => "ExternalDataProcessor",
            Self::Report => "ExternalReport",
        }
    }
    pub fn class_id(self) -> Guid {
        let bytes = match self {
            Self::DataProcessor => [
                0xc3, 0x83, 0x1e, 0xc8, 0xd8, 0xd5, 0x4f, 0x93, 0x8a, 0x22, 0xf9, 0xbf, 0xae, 0x07,
                0x32, 0x7f,
            ],
            Self::Report => [
                0xe4, 0x1a, 0xff, 0x26, 0x25, 0xcf, 0x4b, 0xb6, 0xb6, 0xc1, 0x3f, 0x47, 0x8a, 0x75,
                0xf3, 0x74,
            ],
        };
        Guid::from_bytes(bytes)
    }
    pub fn base_kind(self) -> MdKind {
        match self {
            Self::DataProcessor => MdKind::DataProcessor,
            Self::Report => MdKind::Report,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyObject {
    pub id: Guid,
    pub source_id: Guid,
    pub class_id: Guid,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyType {
    pub type_id: Guid,
    pub owner: Guid,
    pub category: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyDataPath {
    pub child: Guid,
    pub owner_type: Guid,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyStandardDataPath {
    pub index: i32,
    pub owner_type: Guid,
    pub target_index: i32,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CopyInfo {
    pub objects: Vec<CopyObject>,
    pub types: Vec<CopyType>,
    pub data_paths: Vec<CopyDataPath>,
    pub standard_data_paths: Vec<CopyStandardDataPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceMetadata {
    pub kind: crate::SubKind,
    pub version: u32,
    pub extra_flag: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceHeader {
    pub id: Guid,
    pub version: u32,
    pub extra_flags: [u32; 2],
    pub metadata: Option<SourceMetadata>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalDocument {
    pub kind: ExternalKind,
    pub wrapper_id: Guid,
    pub object: MdObject,
    pub compat: Option<u32>,
    pub copy_info: CopyInfo,
    pub diagnostics: Diagnostics,
    pub source_headers: Vec<SourceHeader>,
    #[doc(hidden)]
    pub index: crate::index::Index,
}
impl ExternalDocument {
    pub fn new(kind: ExternalKind, wrapper_id: Guid, object: MdObject) -> Self {
        Self {
            kind,
            wrapper_id,
            object,
            compat: None,
            copy_info: CopyInfo::default(),
            diagnostics: Diagnostics::default(),
            source_headers: Vec::new(),
            index: Default::default(),
        }
    }
    pub fn name(&self, id: Guid) -> Option<&str> {
        self.index.names.get(&id).map(String::as_str)
    }
    pub fn type_name(&self, id: Guid) -> Option<&str> {
        self.index.types.get(&id).map(String::as_str)
    }
    pub fn set_name(&mut self, id: Guid, name: String) {
        self.index.names.insert(id, name);
    }
    pub fn set_type_name(&mut self, id: Guid, name: String) {
        self.index.types.insert(id, name);
    }
    pub fn names(&self) -> impl Iterator<Item = (Guid, &str)> {
        self.index
            .names
            .iter()
            .map(|(id, name)| (*id, name.as_str()))
    }
    pub fn type_names(&self) -> impl Iterator<Item = (Guid, &str)> {
        self.index
            .types
            .iter()
            .map(|(id, name)| (*id, name.as_str()))
    }
}
