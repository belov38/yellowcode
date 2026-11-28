use base64::{Engine as _, engine::general_purpose::STANDARD};
use brace::{Guid, Node};
use memory_budget::{BudgetError, BudgetedVec, Category, Limits, MemoryBudget, Reservation};

const STORED_LIMIT: usize = 1 << 20;
const INPUT_LIMIT: usize = 4 << 20;
const WORKING_LIMIT: u64 = 64 << 20;
const CATEGORY: Category = Category::ReaderWorking;
const ROOT_FOLDS: [&[u8]; 3] = [b"\r\r\n", b"\r\n", b"\n"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigInfoEntry {
    pub name: String,
    pub stored_sha1: [u8; 20],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigInfo {
    pub format_version: u32,
    pub compatibility: u32,
    pub compatibility_feature: Option<Guid>,
    pub root_id: Guid,
    pub entries: Vec<ConfigInfoEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigInfoError {
    #[error("configinfo: {0}")]
    Source(#[source] crate::Error),
    #[error("configinfo: {0}{1}")]
    Brace(#[source] brace::Error, BraceContext),
    #[error("unsupported configinfo structure version {0}")]
    UnsupportedVersion(u32),
    #[error("configinfo: expected {0}")]
    Invalid(&'static str),
    #[error("configinfo input exceeds {INPUT_LIMIT} bytes")]
    InputTooLarge,
    #[error("configinfo stored input exceeds {STORED_LIMIT} bytes")]
    StoredTooLarge,
    #[error("configinfo: {0}")]
    Memory(#[from] BudgetError),
    #[error("configinfo: {0}")]
    Inflate(#[from] zraw::ManagedInflateError),
}
type Result<T> = std::result::Result<T, ConfigInfoError>;

const CONTEXT_TOKENS: usize = 12;
const CONTEXT_DIGITS: usize = 6;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BraceContext(String);

impl BraceContext {
    pub fn new(input: &[u8], offset: usize) -> Self {
        use std::fmt::Write as _;
        let offset = offset.min(input.len());
        let tokens = || {
            let mut at = 0;
            std::iter::from_fn(move || {
                let rest = input.get(at..).filter(|rest| !rest.is_empty())?;
                let (len, kind) = token(rest);
                at += len;
                Some((at - len, at, kind))
            })
        };
        let (here, found) = tokens()
            .enumerate()
            .find(|(_, (_, end, _))| offset < *end)
            .map_or_else(
                || (tokens().count(), None),
                |(index, (.., kind))| (index, Some(kind)),
            );
        let first = here.saturating_sub(CONTEXT_TOKENS);
        let mut window = String::new();
        if first > 0 {
            window.push('…');
        }
        let mut more = false;
        for (index, (start, end, kind)) in tokens().enumerate().skip(first) {
            if index > here + CONTEXT_TOKENS {
                more = true;
                break;
            }
            if index == here {
                window.push_str("<<HERE>>");
            }
            match kind {
                Token::Byte(byte) => escape_byte(&mut window, byte),
                _ => window.push_str(&describe(input.get(start..end).unwrap_or_default(), kind)),
            }
        }
        if found.is_none() {
            window.push_str("<<HERE>>");
        }
        if more {
            window.push('…');
        }
        let token = match found {
            None => "end of input".to_owned(),
            Some(Token::Quoted) => "a quoted string".to_owned(),
            Some(Token::Byte(byte @ (b'{' | b'}' | b','))) => format!("'{}'", byte as char),
            Some(Token::Byte(_)) => "whitespace".to_owned(),
            Some(kind @ Token::Atom) => {
                let atom = input.get(offset..).unwrap_or_default();
                let len = token(atom).0;
                format!(
                    "atom `{}`",
                    describe(atom.get(..len).unwrap_or_default(), kind)
                )
            }
        };
        let mut out = String::new();
        let _ = write!(out, "; found {token}; context: `{window}`");
        Self(out)
    }
}

impl std::fmt::Display for BraceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy)]
enum Token {
    Byte(u8),
    Quoted,
    Atom,
}

fn token(input: &[u8]) -> (usize, Token) {
    let Some(&first) = input.first() else {
        return (1, Token::Byte(0));
    };
    if first == b'"' {
        let mut index = 1;
        while let Some(&byte) = input.get(index) {
            index += 1;
            if byte == b'"' {
                if input.get(index) != Some(&b'"') {
                    return (index, Token::Quoted);
                }
                index += 1;
            }
        }
        return (input.len(), Token::Quoted);
    }
    if brace_space(first) || b"{},".contains(&first) {
        return (1, Token::Byte(first));
    }
    let len = input
        .iter()
        .take_while(|&&byte| !brace_space(byte) && !b"{},\"".contains(&byte))
        .count();
    (len, Token::Atom)
}

fn describe(text: &[u8], kind: Token) -> String {
    if let Token::Quoted = kind {
        let closed = text.len() > 1 && text.ends_with(b"\"");
        return if closed { "\"…\"" } else { "\"…" }.to_owned();
    }
    if text.len() <= CONTEXT_DIGITS && text.iter().all(u8::is_ascii_digit) {
        return String::from_utf8_lossy(text).into_owned();
    }
    if std::str::from_utf8(text)
        .ok()
        .and_then(Guid::parse)
        .is_some()
    {
        return "<uuid>".to_owned();
    }
    let class = if text.iter().copied().all(base64_char) {
        "b64"
    } else {
        "atom"
    };
    format!("<{class}:{}>", text.len())
}

fn escape_byte(out: &mut String, byte: u8) {
    use std::fmt::Write as _;
    match byte {
        b'\r' => out.push_str("\\r"),
        b'\n' => out.push_str("\\n"),
        b'\t' => out.push_str("\\t"),
        0x20..=0x7e => out.push(byte as char),
        _ => {
            let _ = write!(out, "\\x{byte:02x}");
        }
    }
}

fn invalid(what: &'static str) -> ConfigInfoError {
    ConfigInfoError::Invalid(what)
}
fn decimal_uint(atom: &str) -> Option<u32> {
    if !atom.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    atom.parse().ok()
}

fn uint(node: &Node) -> Result<u32> {
    node.as_atom()
        .and_then(decimal_uint)
        .ok_or_else(|| invalid("unsigned 32-bit integer"))
}
fn list(node: &Node) -> Result<&[Node]> {
    node.as_list().ok_or_else(|| invalid("list"))
}
fn zero(node: &Node) -> Result<()> {
    if uint(node)? != 0 {
        return Err(invalid("zero reserved field"));
    }
    Ok(())
}
fn compatibility_fields(version: u32, fields: &Node) -> Result<(u32, Option<Guid>)> {
    let (mode, feature) = match version {
        216 => {
            let [mode, reserved] = list(fields)? else {
                return Err(invalid("compatibility pair"));
            };
            zero(reserved)?;
            (mode, None)
        }
        217 => {
            let [mode, count, features] = list(fields)? else {
                return Err(invalid("compatibility mode, count and feature list"));
            };
            let [feature] = list(features)? else {
                return Err(invalid("one compatibility feature"));
            };
            if uint(count)? != 1 {
                return Err(invalid("compatibility feature count of one"));
            }
            let feature = feature
                .as_guid()
                .ok_or_else(|| invalid("compatibility feature UUID"))?;
            (mode, Some(feature))
        }
        _ => return Err(ConfigInfoError::UnsupportedVersion(version)),
    };
    Ok((uint(mode)?, feature))
}

fn brace_space(byte: u8) -> bool {
    byte.is_ascii_whitespace() || byte == 11
}

fn skip_space(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&byte| !brace_space(byte))
        .unwrap_or(bytes.len());
    bytes.get(start..).unwrap_or_default()
}

fn prefix_uint(bytes: &[u8]) -> Option<u32> {
    let atom = std::str::from_utf8(bytes).ok()?;
    decimal_uint(atom.trim_matches(|c: char| c.is_ascii() && brace_space(c as u8)))
}

fn version_prefix(body: &[u8]) -> Option<u32> {
    let mut envelope = skip_space(body)
        .strip_prefix(b"{")?
        .splitn(2, |&b| b == b',');
    if prefix_uint(envelope.next()?)? != 0 {
        return None;
    }
    let version = skip_space(envelope.next()?).strip_prefix(b"{")?;
    let mut fields = version.splitn(2, |&b| matches!(b, b',' | b'}'));
    let number = fields.next()?;
    fields.next()?;
    prefix_uint(number)
}

pub(crate) fn check_manifest_input(input: &[u8]) -> Result<&[u8]> {
    if input.len() > INPUT_LIMIT {
        return Err(ConfigInfoError::InputTooLarge);
    }
    let body = input.strip_prefix(b"\xef\xbb\xbf").unwrap_or(input);
    if let Some(version) = version_prefix(body)
        && !matches!(version, 216 | 217)
    {
        return Err(ConfigInfoError::UnsupportedVersion(version));
    }
    Ok(body)
}

fn root_descriptor(body: &[u8]) -> Option<(usize, usize)> {
    let version = skip_space(body).strip_prefix(b"{")?;
    let mut depth = 1usize;
    let end = version
        .iter()
        .take_while(|&&byte| byte != b'"')
        .position(|&byte| {
            match byte {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            depth == 0
        })?;
    let after_version = skip_space(version.get(end + 1..)?);
    let root = skip_space(after_version.strip_prefix(b",")?).strip_prefix(b"{")?;
    let close = root.iter().position(|&byte| byte == b'}')?;
    let root_start = body.len() - root.len();
    let root = root.get(..close)?;
    let payload = skip_space(root.splitn(3, |&byte| byte == b',').nth(2)?);
    let start = root_start + root.len() - payload.len();
    Some((start, root_start + close))
}

fn base64_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}

fn folded_descriptor(body: &[u8]) -> Option<(usize, usize)> {
    let (start, end) = root_descriptor(body)?;
    let region = body.get(start..end)?;
    let text = region.trim_ascii_end();
    if !text.iter().any(|&byte| brace_space(byte))
        || !text
            .iter()
            .all(|&byte| base64_char(byte) || brace_space(byte))
    {
        return None;
    }
    let mut rest = text;
    let (mut width, mut fold) = (None, None);
    loop {
        let line_len = rest
            .iter()
            .position(|&byte| brace_space(byte))
            .unwrap_or(rest.len());
        let (line, tail) = rest.split_at(line_len);
        if tail.is_empty() {
            return (!line.is_empty() && line.len() <= width?).then_some((start, end));
        }
        if line.is_empty()
            || !line.len().is_multiple_of(4)
            || *width.get_or_insert(line.len()) != line.len()
        {
            return None;
        }
        let this = ROOT_FOLDS.into_iter().find(|fold| tail.starts_with(fold))?;
        if *fold.get_or_insert(this) != this {
            return None;
        }
        rest = tail.get(this.len()..)?;
    }
}

fn canonical_base64(atom: &str) -> bool {
    const CHUNK: usize = 1024;
    let bytes = atom.as_bytes();
    let padding = bytes.iter().rev().take_while(|&&byte| byte == b'=').count();
    let data = bytes.get(..bytes.len() - padding).unwrap_or_default();
    if !bytes.len().is_multiple_of(4) || padding > 2 || data.contains(&b'=') {
        return false;
    }
    let mut output = [0; CHUNK / 4 * 3];
    bytes
        .chunks(CHUNK)
        .all(|chunk| STANDARD.decode_slice(chunk, &mut output).is_ok())
}

fn budget() -> Result<MemoryBudget> {
    Ok(MemoryBudget::try_new(Limits::new(WORKING_LIMIT))?)
}

fn forecast(input: &[u8]) -> Result<u64> {
    Ok(forecast_with_depth(input)?.0)
}

pub(crate) fn forecast_with_depth(input: &[u8]) -> std::result::Result<(u64, u64), BudgetError> {
    let (mut nodes, mut lists) = (2u64, 1u64);
    let (mut quoted, mut atom) = (false, false);
    let (mut depth, mut maximum_depth) = (0u64, 0u64);
    let mut bytes = input.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if quoted {
            if byte == b'"' {
                if bytes.peek() == Some(&b'"') {
                    bytes.next();
                } else {
                    quoted = false;
                }
            }
            continue;
        }
        match byte {
            b'"' => {
                quoted = true;
                atom = false;
                nodes = add(nodes, 1)?;
            }
            b'{' => {
                depth = add(depth, 1)?;
                maximum_depth = maximum_depth.max(depth);
                lists = add(lists, 1)?;
                nodes = add(nodes, 2)?;
                atom = false;
            }
            b',' => {
                nodes = add(nodes, 1)?;
                atom = false;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                atom = false;
            }
            b' ' | b'\t' | b'\r' | b'\n' | 11 | 12 => atom = false,
            _ if !atom => {
                nodes = add(nodes, 1)?;
                atom = true;
            }
            _ => {}
        }
    }
    use memory_budget::Charged;
    use std::mem::{align_of, size_of};
    let charged = size_of::<Charged<Node>>() as u64;
    let top = size_of::<(usize, Charged<Node>)>() as u64;
    let items_header = size_of::<BudgetedVec<Charged<Node>>>() as u64;
    let node_header = size_of::<BudgetedVec<Node>>() as u64;
    let bytes_header = size_of::<BudgetedVec<u8>>() as u64;
    let alignment = align_of::<BudgetedVec<Charged<Node>>>().max(align_of::<usize>()) as u64;
    let frame = add(
        add(items_header, size_of::<usize>() as u64)?,
        add(size_of::<bool>() as u64, mul(2, alignment)?)?,
    )?;
    let per_node = add(
        add(mul(3, add(charged, top)?)?, size_of::<Node>() as u64)?,
        add(bytes_header, size_of::<ConfigInfoEntry>() as u64)?,
    )?;
    let per_list = add(mul(3, frame)?, add(items_header, node_header)?)?;
    let fixed = add(
        add(bytes_header, items_header)?,
        add(
            size_of::<BudgetedVec<(usize, Charged<Node>)>>() as u64,
            size_of::<Vec<ConfigInfoEntry>>() as u64,
        )?,
    )?;
    let bytes = add(
        add(mul(nodes, per_node)?, mul(lists, per_list)?)?,
        add(mul(input.len() as u64, 3)?, add(fixed, 2)?)?,
    )?;
    Ok((bytes, maximum_depth))
}
fn add(left: u64, right: u64) -> std::result::Result<u64, BudgetError> {
    left.checked_add(right).ok_or(BudgetError::Overflow)
}
fn mul(left: u64, right: u64) -> std::result::Result<u64, BudgetError> {
    left.checked_mul(right).ok_or(BudgetError::Overflow)
}
fn preflight(input: &[u8], budget: &MemoryBudget) -> Result<()> {
    drop(
        budget
            .try_reserve(CATEGORY, forecast(input)?)
            .map_err(BudgetError::from)?,
    );
    Ok(())
}

pub(crate) struct RetainedConfigInfo {
    pub(crate) value: ConfigInfo,
    _hold: Reservation,
}

impl ConfigInfo {
    pub fn parse(input: &[u8]) -> Result<Self> {
        Self::parse_with(input, &budget()?)
    }

    pub fn read(source: &dyn crate::EntrySource) -> Result<Option<Self>> {
        let Some(size) = source
            .stored_len("configinfo")
            .map_err(ConfigInfoError::Source)?
        else {
            return Ok(None);
        };
        if size > STORED_LIMIT as u64 {
            return Err(ConfigInfoError::StoredTooLarge);
        }
        let budget = budget()?;
        let _stored_hold = budget
            .try_reserve(CATEGORY, STORED_LIMIT as u64)
            .map_err(BudgetError::from)?;
        let stored = source
            .stored_bounded("configinfo", STORED_LIMIT)
            .map_err(ConfigInfoError::Source)?
            .ok_or_else(|| invalid("configinfo entry present during read"))?;
        if stored.capacity() > STORED_LIMIT {
            return Err(ConfigInfoError::StoredTooLarge);
        }
        let inflated = zraw::inflate_budgeted(&stored, INPUT_LIMIT as u64, &budget, CATEGORY)?;
        Self::parse_with(&inflated, &budget).map(Some)
    }

    fn parse_with(input: &[u8], budget: &MemoryBudget) -> Result<Self> {
        Self::parse_retained(input, budget).map(|owner| owner.value)
    }

    pub(crate) fn parse_retained(
        input: &[u8],
        budget: &MemoryBudget,
    ) -> Result<RetainedConfigInfo> {
        let body = check_manifest_input(input)?;
        let bom = input.len() - body.len();
        preflight(body, budget)?;
        let mut wrapped = BudgetedVec::try_with_capacity(
            budget,
            CATEGORY,
            body.len().checked_add(2).ok_or(BudgetError::Overflow)?,
        )?;
        wrapped.try_push(b'{')?;
        if let Some((start, end)) = folded_descriptor(body) {
            let region = body.get(start..end).ok_or(BudgetError::Overflow)?;
            wrapped.try_extend_from_slice(body.get(..start).ok_or(BudgetError::Overflow)?)?;
            let mut joined = 0;
            for line in region.split(|&byte| brace_space(byte)) {
                wrapped.try_extend_from_slice(line)?;
                joined += line.len();
            }
            for _ in joined..region.len() {
                wrapped.try_push(b' ')?;
            }
            wrapped.try_extend_from_slice(body.get(end..).ok_or(BudgetError::Overflow)?)?;
        } else {
            wrapped.try_extend_from_slice(body)?;
        }
        wrapped.try_push(b'}')?;
        let tree =
            brace::parse_bytes_budgeted(&wrapped, budget, CATEGORY).map_err(
                |error| match error {
                    brace::ManagedParseError::Memory(error) => ConfigInfoError::Memory(error),
                    brace::ManagedParseError::Parse(mut error) => {
                        error.offset = error
                            .offset
                            .saturating_sub(1)
                            .saturating_add(bom)
                            .min(input.len());
                        let context = BraceContext::new(input, error.offset);
                        ConfigInfoError::Brace(error, context)
                    }
                },
            )?;
        let [version, root, versions] = list(&tree)? else {
            return Err(invalid("exactly three values"));
        };
        let [reserved, version] = list(version)? else {
            return Err(invalid("version envelope"));
        };
        zero(reserved)?;
        let [format_version, reserved, compatibility] = list(version)? else {
            return Err(invalid("version triple"));
        };
        let format_version = uint(format_version)?;
        let (compatibility, compatibility_feature) =
            compatibility_fields(format_version, compatibility)?;
        zero(reserved)?;
        let [root_version, root_id, descriptor] = list(root)? else {
            return Err(invalid("root triple"));
        };
        if uint(root_version)? != 2 {
            return Err(invalid("version-2 root"));
        }
        match descriptor.as_atom() {
            None => return Err(invalid("root payload atom")),
            Some(atom) if !canonical_base64(atom) => {
                return Err(invalid("supported root descriptor"));
            }
            Some(_) => {}
        }
        let root_id = root_id
            .as_atom()
            .and_then(Guid::parse)
            .ok_or_else(|| invalid("root UUID"))?;
        let Some((count, pairs)) = list(versions)?.split_first() else {
            return Err(invalid("entry count"));
        };
        let count = uint(count)? as usize;
        let (pairs, remainder) = pairs.as_chunks::<2>();
        if !remainder.is_empty() || count != pairs.len() {
            return Err(invalid("entry count matching name/digest pairs"));
        }
        let names_size = pairs.iter().try_fold(0usize, |sum, [name, _]| {
            let name = name.as_str().ok_or_else(|| invalid("quoted entry name"))?;
            sum.checked_add(name.len())
                .ok_or(ConfigInfoError::Memory(BudgetError::Overflow))
        })?;
        let output_size = count
            .checked_mul(std::mem::size_of::<ConfigInfoEntry>())
            .and_then(|size| size.checked_add(names_size))
            .ok_or(BudgetError::Overflow)?;
        let output_hold = budget
            .try_reserve(CATEGORY, output_size as u64)
            .map_err(BudgetError::from)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count)
            .map_err(|_| BudgetError::AllocationFailed)?;
        for [name, digest] in pairs {
            let name = name.as_str().ok_or_else(|| invalid("quoted entry name"))?;
            let digest = digest
                .as_atom()
                .ok_or_else(|| invalid("base64 SHA-1 atom"))?;
            let mut stored_sha1 = [0; 20];
            if STANDARD
                .decode_slice(digest, &mut stored_sha1)
                .map_err(|_| invalid("base64 SHA-1 digest"))?
                != 20
            {
                return Err(invalid("20-byte SHA-1 digest"));
            }
            let mut owned_name = String::new();
            owned_name
                .try_reserve_exact(name.len())
                .map_err(|_| BudgetError::AllocationFailed)?;
            owned_name.push_str(name);
            entries.push(ConfigInfoEntry {
                name: owned_name,
                stored_sha1,
            });
        }
        Ok(RetainedConfigInfo {
            value: Self {
                format_version,
                compatibility,
                compatibility_feature,
                root_id,
                entries,
            },
            _hold: output_hold,
        })
    }
}

impl crate::CfFile {
    pub fn extension_info(&self) -> Result<Option<ConfigInfo>> {
        ConfigInfo::read(self)
    }
}
