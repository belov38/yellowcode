use crate::error::{Error, Result};
use std::io::{self, Read, Write};

pub const SIGNATURE: &[u8; 8] = b"1CIBDmpF";
const SIGNATURE_EV: &[u8; 8] = b"EVIBDmpF";
pub(crate) const HEADER_LEN: u64 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtVersion {
    F2,
    F3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DtHeader {
    pub version: DtVersion,
    pub ev: bool,
}

pub fn read_header(r: &mut impl Read) -> Result<DtHeader> {
    let mut b = [0u8; 9];
    r.read_exact(&mut b).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => Error::Dt {
            offset: 0,
            what: "short header".into(),
        },
        _ => Error::Io(e),
    })?;
    let (sig, ver) = (b.get(..8).unwrap_or_default(), b.get(8).copied());
    let ev = if sig == SIGNATURE {
        false
    } else if sig == SIGNATURE_EV {
        true
    } else {
        return Err(Error::Dt {
            offset: 0,
            what: "bad signature".into(),
        });
    };
    let version = match ver {
        Some(b'2') => DtVersion::F2,
        Some(b'3') => DtVersion::F3,
        Some(b'1') => return Err(Error::Unsupported("dt format F1".into())),
        _ => {
            return Err(Error::Dt {
                offset: 8,
                what: "bad format version byte".into(),
            });
        }
    };
    Ok(DtHeader { version, ev })
}

pub fn write_header(w: &mut impl Write, h: DtHeader) -> Result<()> {
    if h.ev {
        return Err(Error::Unsupported("EVIBDmpF header".into()));
    }
    w.write_all(SIGNATURE)?;
    w.write_all(match h.version {
        DtVersion::F2 => b"2",
        DtVersion::F3 => b"3",
    })?;
    Ok(())
}

fn zraw_err(e: zraw::Error) -> Error {
    match e {
        zraw::Error::Zlib(m) => Error::Unsupported(format!("zlib: {m}")),
        other => Error::Dt {
            offset: 0,
            what: format!("deflate: {other}"),
        },
    }
}

pub fn deflate_writer<W: Write>(w: W) -> Result<zraw::DeflateWriter<W>> {
    zraw::DeflateWriter::new(w, zraw::LEVEL_1C).map_err(zraw_err)
}

pub fn inflate_reader<R: Read>(r: R) -> Result<zraw::InflateReader<R>> {
    zraw::InflateReader::new(r).map_err(zraw_err)
}
