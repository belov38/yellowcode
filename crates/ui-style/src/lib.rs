#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

#[cfg(feature = "packages")]
pub mod package;

pub mod captions;
pub mod metrics;
pub mod pictures;
pub mod style;

pub use metrics::{FontRole, Metrics};
pub use pictures::{PicFormat, StdPicture, StdPictureInfo, std_picture, std_pictures};
pub use style::{Style, StyleEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Interface {
    Taxi,
    V85,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[cfg(feature = "font-files")]
pub const FONT_FILES: &[(&str, &[u8])] = &[
    (
        "liberation-sans-regular.ttf",
        include_bytes!("../fonts/LiberationSans-Regular.ttf"),
    ),
    (
        "liberation-sans-bold.ttf",
        include_bytes!("../fonts/LiberationSans-Bold.ttf"),
    ),
];

#[cfg(feature = "skin")]
pub mod skin;
