use std::io::Read;

pub const ACCEPT_ENCODING: &str = "zstd,deflate;q=0.5";

pub const MAX_DECODED: u64 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    #[error("cannot decode Content-Encoding: {0}")]
    Unsupported(String),
    #[error("{codec} decode failed: {msg}")]
    Corrupt { codec: &'static str, msg: String },
    #[error("decoded body exceeds {MAX_DECODED} bytes")]
    TooLarge,
}

pub fn zstd_compress(data: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
}

pub fn zstd_decompress(data: &[u8]) -> Result<Vec<u8>, BodyError> {
    zstd_decompress_to(data, MAX_DECODED)
}

fn zstd_decompress_to(data: &[u8], limit: u64) -> Result<Vec<u8>, BodyError> {
    let corrupt = |e: &dyn std::fmt::Display| BodyError::Corrupt {
        codec: "zstd",
        msg: e.to_string(),
    };
    let dec = ruzstd::decoding::StreamingDecoder::new(data).map_err(|e| corrupt(&e))?;
    read_bounded_to(dec, "zstd", limit)
}

fn read_bounded_to(r: impl Read, codec: &'static str, limit: u64) -> Result<Vec<u8>, BodyError> {
    let mut out = Vec::new();
    let n = r
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| BodyError::Corrupt {
            codec,
            msg: e.to_string(),
        })?;
    if n as u64 > limit {
        return Err(BodyError::TooLarge);
    }
    Ok(out)
}

pub fn decode(body: &[u8], encoding: Option<&str>) -> Result<Vec<u8>, BodyError> {
    decode_bounded(body, encoding, MAX_DECODED)
}

pub fn decode_bounded(
    body: &[u8],
    encoding: Option<&str>,
    limit: u64,
) -> Result<Vec<u8>, BodyError> {
    let enc = encoding.unwrap_or("").trim().to_ascii_lowercase();
    match enc.as_str() {
        "" | "identity" if body.len() as u64 > limit => Err(BodyError::TooLarge),
        "" | "identity" => Ok(body.to_vec()),
        "zstd" => zstd_decompress_to(body, limit),
        "deflate" => {
            match read_bounded_to(flate2::read::DeflateDecoder::new(body), "deflate", limit) {
                Ok(v) => Ok(v),
                Err(BodyError::TooLarge) => Err(BodyError::TooLarge),
                Err(_) => read_bounded_to(flate2::read::ZlibDecoder::new(body), "deflate", limit),
            }
        }
        other => Err(BodyError::Unsupported(other.to_string())),
    }
}
