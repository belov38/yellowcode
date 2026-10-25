#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

#[cfg(feature = "mdconf")]
pub mod autoform;
#[cfg(feature = "mdconf")]
mod build;
mod desc;
mod dom;
mod parse;
mod props;
pub mod stdcmd;
mod types;

#[cfg(feature = "mdconf")]
pub use build::{BuiltForm, MdResolver, from_configuration, type_desc};
pub use desc::{
    Attribute, Column, Command, FormDesc, Item, ItemKind, LStr, ParseStats, PictureRef,
};
pub use parse::parse_form_xml;
pub use props::{GroupKind, HAlign, Location, Props, Spacing, TitleLocation};
pub use types::{DateParts, NoConfig, Resolved, TypeDesc, TypeKind, TypeResolver};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    Xml(String),
    NotAForm(String),
    TooDeep,
    TooLarge,
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Xml(e) => write!(f, "malformed form XML: {e}"),
            ModelError::NotAForm(n) => write!(f, "not a form: root element `{n}`"),
            ModelError::TooDeep => write!(f, "form items nested deeper than {MAX_DEPTH}"),
            ModelError::TooLarge => write!(f, "form XML larger than {MAX_XML_BYTES} bytes"),
        }
    }
}

impl std::error::Error for ModelError {}

pub const MAX_DEPTH: usize = 256;
pub const MAX_XML_BYTES: usize = 64 << 20;
