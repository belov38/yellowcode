use base64::{Engine, engine::general_purpose::STANDARD};
use brace::Guid;
use std::ops::Range;

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub input_bytes: usize,
    pub decoded_bytes: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RootData<'a, B = Vec<u8>> {
    pub source: &'a [u8],
    pub version: u32,
    pub version_span: Range<usize>,
    pub id: Guid,
    pub id_span: Range<usize>,
    pub binary: Option<B>,
    pub binary_span: Option<Range<usize>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ErrorKind {
    InputLimit,
    DecodedLimit,
    Expected(&'static str),
    UnsupportedVersion(u32),
    InvalidBase64,
    TrailingContent,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Error {
    pub offset: usize,
    pub kind: ErrorKind,
}
fn error(offset: usize, kind: ErrorKind) -> Error {
    Error { offset, kind }
}
fn whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 11 | 12)
}
fn skip_space(input: &[u8], at: &mut usize) {
    while input.get(*at).copied().is_some_and(whitespace) {
        *at += 1;
    }
}
fn expect(input: &[u8], at: &mut usize, byte: u8, name: &'static str) -> Result<(), Error> {
    skip_space(input, at);
    if input.get(*at) != Some(&byte) {
        return Err(error(*at, ErrorKind::Expected(name)));
    }
    *at += 1;
    Ok(())
}
fn atom(input: &[u8], at: &mut usize) -> Range<usize> {
    skip_space(input, at);
    let start = *at;
    while input
        .get(*at)
        .is_some_and(|b| !whitespace(*b) && !matches!(b, b',' | b'{' | b'}' | b'"'))
    {
        *at += 1;
    }
    start..*at
}

pub(crate) fn parse(input: &[u8], limits: Limits) -> Result<RootData<'_>, Error> {
    parse_with(input, limits, |body, length, decoded_length, start| {
        let mut compact = Vec::with_capacity(length);
        compact.extend(body.iter().copied().filter(|b| !matches!(b, b'\r' | b'\n')));
        let mut decoded = vec![0; decoded_length];
        let actual = STANDARD
            .decode_slice(&compact, &mut decoded)
            .map_err(|_| error(start, ErrorKind::InvalidBase64))?;
        if actual != decoded_length {
            return Err(error(start, ErrorKind::InvalidBase64));
        }
        Ok(decoded)
    })
}

fn parse_with<'a, B, E: From<Error>>(
    input: &'a [u8],
    limits: Limits,
    decode: impl FnOnce(&[u8], usize, usize, usize) -> Result<B, E>,
) -> Result<RootData<'a, B>, E> {
    if input.len() > limits.input_bytes {
        return Err(error(0, ErrorKind::InputLimit).into());
    }
    let mut at = if input.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    expect(input, &mut at, b'{', "opening brace")?;
    let version_span = atom(input, &mut at);
    let version = std::str::from_utf8(
        input
            .get(version_span.clone())
            .ok_or_else(|| error(version_span.start, ErrorKind::Expected("version span")))?,
    )
    .ok()
    .and_then(|s| s.parse::<u32>().ok())
    .ok_or_else(|| {
        error(
            version_span.start,
            ErrorKind::Expected("unsigned root version"),
        )
    })?;
    if version != 2 {
        return Err(error(version_span.start, ErrorKind::UnsupportedVersion(version)).into());
    }
    expect(input, &mut at, b',', "comma before GUID")?;
    let id_span = atom(input, &mut at);
    let id = std::str::from_utf8(
        input
            .get(id_span.clone())
            .ok_or_else(|| error(id_span.start, ErrorKind::Expected("GUID span")))?,
    )
    .ok()
    .and_then(Guid::parse)
    .ok_or_else(|| error(id_span.start, ErrorKind::Expected("configuration GUID")))?;
    skip_space(input, &mut at);
    let (binary, binary_span) = if input.get(at) == Some(&b',') {
        at += 1;
        let start = at;
        while input.get(at).is_some_and(|b| !matches!(b, b'}' | b',')) {
            at += 1;
        }
        if input.get(at) == Some(&b',') {
            return Err(error(at, ErrorKind::TrailingContent).into());
        }
        if input.get(at) != Some(&b'}') {
            return Err(error(at, ErrorKind::Expected("closing brace")).into());
        }
        let span = start..at;
        let body = input
            .get(span.clone())
            .ok_or_else(|| error(start, ErrorKind::InvalidBase64))?;
        let length = body.iter().filter(|b| !matches!(b, b'\r' | b'\n')).count();
        let padding = body
            .iter()
            .rev()
            .filter(|b| !matches!(b, b'\r' | b'\n'))
            .take_while(|b| **b == b'=')
            .count();
        if length % 4 != 0 || padding > 2 {
            return Err(error(start, ErrorKind::InvalidBase64).into());
        }
        let decoded_length = (length / 4)
            .checked_mul(3)
            .and_then(|n| n.checked_sub(padding))
            .ok_or_else(|| error(start, ErrorKind::InvalidBase64))?;
        if decoded_length > limits.decoded_bytes {
            return Err(error(start, ErrorKind::DecodedLimit).into());
        }
        let decoded = decode(body, length, decoded_length, start)?;
        (Some(decoded), Some(span))
    } else {
        (None, None)
    };
    expect(input, &mut at, b'}', "closing brace")?;
    skip_space(input, &mut at);
    if at != input.len() {
        return Err(error(at, ErrorKind::TrailingContent).into());
    }
    Ok(RootData {
        source: input,
        version,
        version_span,
        id,
        id_span,
        binary,
        binary_span,
    })
}

pub(crate) const STORED_LIMIT: usize = 1 << 20;
pub(crate) const INFLATED_LIMIT: usize = 4 << 20;
pub(crate) const DECODED_LIMIT: usize = 3 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootInfo {
    meta: crate::EntryMeta,
    stored: Vec<u8>,
    source: Vec<u8>,
    version: u32,
    id: Guid,
    version_span: Range<usize>,
    id_span: Range<usize>,
    binary: Option<Vec<u8>>,
    binary_span: Option<Range<usize>>,
}
#[derive(Debug, thiserror::Error)]
pub enum ManagedRootError {
    #[error("{0}")]
    Memory(#[from] memory_budget::BudgetError),
    #[error("{0}")]
    Inflate(#[from] zraw::ManagedInflateError),
    #[error("root wire shape at {offset}: {kind:?}")]
    Parse { offset: usize, kind: ErrorKind },
    #[error("stored root exceeds {STORED_LIMIT} bytes")]
    StoredTooLarge,
}
impl From<Error> for ManagedRootError {
    fn from(error: Error) -> Self {
        Self::Parse {
            offset: error.offset,
            kind: error.kind,
        }
    }
}

impl RootInfo {
    pub fn read_budgeted(
        meta: &crate::EntryMeta,
        stored: memory_budget::Charged<Vec<u8>>,
    ) -> Result<memory_budget::Charged<Self>, ManagedRootError> {
        use memory_budget::BudgetedVec;
        if stored.len() > STORED_LIMIT {
            return Err(ManagedRootError::StoredTooLarge);
        }
        let budget = stored.reservation().budget().clone();
        let category = stored.reservation().category();
        let source = zraw::inflate_budgeted(&stored, INFLATED_LIMIT as u64, &budget, category)?;
        let parsed = parse_with(
            &source,
            Limits {
                input_bytes: INFLATED_LIMIT,
                decoded_bytes: DECODED_LIMIT,
            },
            |body, length, decoded_length, start| -> Result<_, ManagedRootError> {
                let mut compact = BudgetedVec::try_with_capacity(&budget, category, length)?;
                for byte in body.iter().copied().filter(|b| !matches!(b, b'\r' | b'\n')) {
                    compact.try_push(byte)?;
                }
                let mut decoded =
                    BudgetedVec::try_with_capacity(&budget, category, decoded_length)?;
                for _ in 0..decoded_length {
                    decoded.try_push(0)?;
                }
                let actual = STANDARD
                    .decode_slice(&*compact, &mut decoded)
                    .map_err(|_| error(start, ErrorKind::InvalidBase64))?;
                if actual != decoded_length {
                    return Err(error(start, ErrorKind::InvalidBase64).into());
                }
                Ok(decoded.into_charged_vec())
            },
        )?;
        let binary = match parsed.binary {
            Some(binary) => binary.map(Some),
            None => BudgetedVec::<u8>::try_with_capacity(&budget, category, 0)?
                .into_charged_vec()
                .map(|_| None),
        };
        let mut name = BudgetedVec::try_with_capacity(&budget, category, meta.name.len())?;
        name.try_extend_from_slice(meta.name.as_bytes())?;
        let name = name
            .into_charged_vec()
            .try_map(String::from_utf8)
            .map_err(|_| memory_budget::BudgetError::Overflow)?;
        let meta = name.map(|name| crate::EntryMeta {
            name,
            ctime: meta.ctime,
            mtime: meta.mtime,
            attrs: meta.attrs,
        });
        let version = parsed.version;
        let id = parsed.id;
        let version_span = parsed.version_span;
        let id_span = parsed.id_span;
        let binary_span = parsed.binary_span;
        Ok(stored.try_zip(source)?.try_zip(meta)?.try_zip(binary)?.map(
            |(((stored, source), meta), binary)| Self {
                meta,
                stored,
                source,
                version,
                id,
                version_span,
                id_span,
                binary,
                binary_span,
            },
        ))
    }
    pub(crate) fn read(meta: crate::EntryMeta, stored: Vec<u8>) -> crate::Result<Self> {
        let fail = |what: String| crate::Error::Format {
            entry: "root".into(),
            what,
        };
        let source = zraw::inflate(&stored, INFLATED_LIMIT as u64)
            .map_err(|e| fail(format!("root inflation: {e}")))?;
        let parsed = parse(
            &source,
            Limits {
                input_bytes: INFLATED_LIMIT,
                decoded_bytes: DECODED_LIMIT,
            },
        )
        .map_err(|e| fail(format!("root wire shape at {}: {:?}", e.offset, e.kind)))?;
        let info = Self {
            meta,
            stored,
            version: parsed.version,
            id: parsed.id,
            version_span: parsed.version_span,
            id_span: parsed.id_span,
            binary: parsed.binary,
            binary_span: parsed.binary_span,
            source,
        };
        Ok(info)
    }
    pub fn meta(&self) -> &crate::EntryMeta {
        &self.meta
    }
    pub fn stored(&self) -> &[u8] {
        &self.stored
    }
    pub fn source(&self) -> &[u8] {
        &self.source
    }
    pub fn version(&self) -> u32 {
        self.version
    }
    pub fn id(&self) -> Guid {
        self.id
    }
    pub fn version_span(&self) -> Range<usize> {
        self.version_span.clone()
    }
    pub fn id_span(&self) -> Range<usize> {
        self.id_span.clone()
    }
    pub fn binary(&self) -> Option<&[u8]> {
        self.binary.as_deref()
    }
    pub fn binary_span(&self) -> Option<Range<usize>> {
        self.binary_span.clone()
    }
}

impl md_model::CodecState for RootInfo {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn same(&self, other: &dyn md_model::CodecState) -> bool {
        other
            .as_any()
            .downcast_ref::<RootInfo>()
            .is_some_and(|other| self == other)
    }
}
