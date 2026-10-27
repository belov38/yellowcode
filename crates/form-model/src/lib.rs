#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

#[cfg(feature = "mdconf")]
pub mod autoform;
#[cfg(feature = "mdconf")]
mod build;
mod desc;
mod discovery;
mod dom;
mod parse;
mod props;
#[cfg(feature = "mdconf")]
mod resolver;
pub mod stdcmd;
mod types;

#[cfg(feature = "mdconf")]
pub use build::{BuiltForm, MdResolver, from_configuration, type_desc};
pub use desc::{
    Attribute, Column, Command, FormDesc, Item, ItemKind, LStr, ParseStats, PictureRef,
};
pub use discovery::{ExternalTypeRequest, discover_type_requests};
#[cfg(feature = "mdconf")]
pub mod preview_answers;
pub mod preview_layout;
pub mod preview_picture;
pub mod preview_render;
pub mod preview_wire;
pub use parse::parse_form_xml;
pub use props::{GroupKind, HAlign, Location, Props, Spacing, TitleLocation};
#[cfg(feature = "mdconf")]
pub use resolver::{ResolvedView, ResolverOwner, TitleView, TypeNamesView, TypeView};
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
