pub use crate::preview_wire::{DecodeError, WriteError};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mime {
    Png,
    Svg,
    Jpeg,
    Gif,
    Bmp,
    Icon,
}
impl Mime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Svg => "image/svg+xml",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Bmp => "image/bmp",
            Self::Icon => "image/x-icon",
        }
    }
    pub fn from_mime(s: &str) -> Option<Self> {
        match s {
            "image/png" => Some(Self::Png),
            "image/svg+xml" => Some(Self::Svg),
            "image/jpeg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/bmp" => Some(Self::Bmp),
            "image/x-icon" => Some(Self::Icon),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image<'a> {
    pub mime: Mime,
    pub bytes: &'a [u8],
}
pub fn encode<E>(
    image: Option<Image<'_>>,
    mut sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut put = |bytes: &[u8]| sink(bytes).map_err(WriteError::Sink);
    let Some(image) = image else {
        return put(b"IMG1\0");
    };
    put(b"IMG1\x01")?;
    put(&[match image.mime {
        Mime::Png => 0,
        Mime::Svg => 1,
        Mime::Jpeg => 2,
        Mime::Gif => 3,
        Mime::Bmp => 4,
        Mime::Icon => 5,
    }])?;
    put(&u64::try_from(image.bytes.len())
        .map_err(|_| WriteError::Overflow)?
        .to_le_bytes())?;
    put(image.bytes)
}
pub fn decode_checked<E>(
    bytes: &[u8],
    maximum: usize,
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<Option<Image<'_>>, DecodeError<E>> {
    check().map_err(DecodeError::Check)?;
    if bytes.get(..4) != Some(b"IMG1") {
        return Err(DecodeError::Protocol);
    }
    let image = match bytes.get(4) {
        Some(0) if bytes.len() == 5 => None,
        Some(1) => {
            let mime = match bytes.get(5) {
                Some(0) => Mime::Png,
                Some(1) => Mime::Svg,
                Some(2) => Mime::Jpeg,
                Some(3) => Mime::Gif,
                Some(4) => Mime::Bmp,
                Some(5) => Mime::Icon,
                _ => return Err(DecodeError::Protocol),
            };
            let raw = bytes
                .get(6..14)
                .ok_or(DecodeError::Protocol)?
                .try_into()
                .map_err(|_| DecodeError::Protocol)?;
            let length =
                usize::try_from(u64::from_le_bytes(raw)).map_err(|_| DecodeError::Protocol)?;
            if length > maximum {
                return Err(DecodeError::Protocol);
            }
            let data = bytes.get(14..).ok_or(DecodeError::Protocol)?;
            if data.len() != length {
                return Err(DecodeError::Protocol);
            }
            check().map_err(DecodeError::Check)?;
            Some(Image { mime, bytes: data })
        }
        _ => return Err(DecodeError::Protocol),
    };
    check().map_err(DecodeError::Check)?;
    Ok(image)
}
