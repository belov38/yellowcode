use base64::{Engine as _, engine::general_purpose::STANDARD};
use brace::{Guid, Node};
use memory_budget::{BudgetError, BudgetedVec, Category, Limits, MemoryBudget, Reservation};

const STORED_LIMIT: usize = 1 << 20;
const INPUT_LIMIT: usize = 4 << 20;
const WORKING_LIMIT: u64 = 64 << 20;
const CATEGORY: Category = Category::ReaderWorking;
const ROOT_PAYLOAD_BYTES: usize = 64;
const ROOT_FOLD_COLUMN: usize = 64;
const ROOT_FOLD: &[u8] = b"\r\r\n";
const ROOT_BASE64_BYTES: usize = ROOT_PAYLOAD_BYTES.div_ceil(3) * 4;

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
    #[error("configinfo: {0}")]
    Brace(#[source] brace::Error),
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

fn root_payload_fold(body: &[u8]) -> Option<(usize, usize)> {
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
    let end = ROOT_BASE64_BYTES + ROOT_FOLD.len();
    let (payload, trailing) = payload.split_at_checked(end)?;
    let (first, folded) = payload.split_at(ROOT_FOLD_COLUMN);
    let last = folded.strip_prefix(ROOT_FOLD)?;
    if !trailing.iter().copied().all(brace_space)
        || !first
            .iter()
            .chain(last)
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return None;
    }
    Some((start + ROOT_FOLD_COLUMN, start + end))
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
    pub(crate) six_parts: bool,
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
        Self::parse_profile(input, budget, false)
    }

    pub(crate) fn parse_profile(
        input: &[u8],
        budget: &MemoryBudget,
        installed: bool,
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
        let folded = installed.then(|| folded_descriptor(body)).flatten();
        if let Some(locator) = &folded {
            let encoded = locator
                .encoded
                .get(..locator.len)
                .ok_or(BudgetError::Overflow)?;
            wrapped.try_extend_from_slice(locator.prefix)?;
            wrapped.try_extend_from_slice(encoded)?;
            wrapped.try_extend_from_slice(locator.padding)?;
            wrapped.try_extend_from_slice(locator.suffix)?;
        } else {
            wrapped.try_extend_from_slice(body)?;
        }
        wrapped.try_push(b'}')?;
        if let Some((fold, end)) = root_payload_fold(body).filter(|_| folded.is_none()) {
            let suffix = wrapped
                .get_mut(fold + 1..end + 1)
                .ok_or_else(|| invalid("root payload fold within the manifest"))?;
            suffix.rotate_left(ROOT_FOLD.len());
        }
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
                        ConfigInfoError::Brace(error)
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
        let descriptor = descriptor.as_atom();
        let six_parts = folded.is_some() && descriptor.is_some_and(|atom| !atom.is_empty());
        let payload = |atom: &str| {
            atom.is_empty()
                || (atom.len() == ROOT_BASE64_BYTES
                    && STANDARD.decode_slice(atom, &mut [0; ROOT_PAYLOAD_BYTES])
                        == Ok(ROOT_PAYLOAD_BYTES))
        };
        if !six_parts && !descriptor.is_some_and(payload) {
            return Err(invalid(match descriptor {
                _ if installed => "supported root descriptor",
                None => "root payload atom",
                Some(_) => "empty or 64-byte Base64 root payload",
            }));
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
            six_parts,
            _hold: output_hold,
        })
    }
}

struct FoldedDescriptor<'a> {
    prefix: &'a [u8],
    suffix: &'a [u8],
    padding: &'static [u8],
    encoded: [u8; 256],
    len: usize,
}

fn folded_descriptor(body: &[u8]) -> Option<FoldedDescriptor<'_>> {
    let mut rest = body.get(body.iter().position(|&b| b == b'}')?..)?;
    for &marker in b"}}},{2," {
        rest = rest.trim_ascii_start().strip_prefix(&[marker])?;
    }
    rest = rest.trim_ascii_start();
    let id = std::str::from_utf8(rest.get(..36)?)
        .ok()
        .and_then(Guid::parse)?;
    if id.is_nil() {
        return None;
    }
    rest = rest.get(36..)?.trim_ascii_start().strip_prefix(b",")?;
    let start = body.len().checked_sub(rest.len())?;
    let raw_len = rest.iter().take(269).position(|&b| b == b'}')?;
    let (lines, trailing, decoded): (&[usize], bool, usize) = match raw_len {
        178 => (&[64, 64, 44], false, 128),
        268 => (&[64, 64, 64, 64], true, 192),
        _ => return None,
    };
    let mut encoded = [0; 256];
    let mut output = 0;
    for (index, &width) in lines.iter().enumerate() {
        let (line, tail) = rest.split_at_checked(width)?;
        encoded
            .get_mut(output..output + width)?
            .copy_from_slice(line);
        output += width;
        rest = if index + 1 < lines.len() || trailing {
            tail.strip_prefix(b"\r\r\n")?
        } else {
            tail
        };
    }
    if rest.first() != Some(&b'}')
        || STANDARD.decode_slice(encoded.get(..output)?, &mut [0; 192]) != Ok(decoded)
    {
        return None;
    }
    Some(FoldedDescriptor {
        prefix: body.get(..start)?,
        suffix: body.get(start.checked_add(raw_len)?..)?,
        padding: b"            ".get(..raw_len.checked_sub(output)?)?,
        encoded,
        len: output,
    })
}

impl crate::CfFile {
    pub fn extension_info(&self) -> Result<Option<ConfigInfo>> {
        ConfigInfo::read(self)
    }
}
