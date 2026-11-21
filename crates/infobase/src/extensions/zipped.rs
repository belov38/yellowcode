use super::{Error, Result};
use brace::binary::{Atom, AtomValue, BinNode, ReadLimits, Reader};
use memory_budget::{BudgetError, Category, MemoryBudget};

pub(super) const ZIPPED_BYTES: usize = 16 << 10;

const LOCAL_STRING: &str = "87024738-fc2a-4436-ada1-df79d395c424";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZippedInfo {
    pub hash: [u8; 20],
    pub unsafe_action_warnings: bool,
    pub block: [u8; 8],
    pub safe_mode: bool,
    pub security_profile: String,
    pub synonym: Vec<(String, String)>,
    pub active: bool,
    pub main_roles: bool,
}

pub(super) fn read_list(bytes: &[u8], budget: &MemoryBudget) -> Result<Vec<BinNode>> {
    if bytes.len() > ZIPPED_BYTES {
        return Err(Error::Limit("extension generation value"));
    }
    let _scratch = budget
        .try_reserve(Category::ReaderWorking, 512 << 10)
        .map_err(BudgetError::from)?;
    let mut reader = Reader::new(bytes);
    reader
        .set_depth_limit(32)
        .map_err(|_| Error::Limit("generation depth"))?;
    reader.begin_budget(ReadLimits {
        atom_bytes: ZIPPED_BYTES as u64,
        retained_bytes: ZIPPED_BYTES as u64,
        nodes: 1024,
    });
    let malformed = || Error::Invalid("malformed generation stream");
    let Some(BinNode::List(body)) = reader.read_value().map_err(|_| malformed())? else {
        return Err(malformed());
    };
    if reader.next_event().map_err(|_| malformed())?.is_some() {
        return Err(malformed());
    }
    Ok(body)
}

fn flag(atom: &Atom, what: &'static str) -> Result<bool> {
    atom.as_bool().ok_or(Error::Unsupported(what))
}

fn local_string(text: &str) -> Result<Vec<(String, String)>> {
    let bad = || Error::Unsupported("extension synonym");
    let node = brace::parse(text).map_err(|_| bad())?;
    let Some([sharp, ty, pairs]) = node.as_list() else {
        return Err(bad());
    };
    if sharp.as_str() != Some("#") || ty.as_atom() != Some(LOCAL_STRING) {
        return Err(bad());
    }
    let Some([count, rest @ ..]) = pairs.as_list() else {
        return Err(bad());
    };
    let n = count.as_u32().ok_or_else(bad)? as usize;
    if rest.len() != n.checked_mul(2).ok_or_else(bad)? {
        return Err(bad());
    }
    rest.as_chunks::<2>()
        .0
        .iter()
        .map(|[lang, text]| {
            Ok((
                lang.as_str().ok_or_else(bad)?.to_owned(),
                text.as_str().ok_or_else(bad)?.to_owned(),
            ))
        })
        .collect()
}

pub fn zipped_info(bytes: &[u8], budget: &MemoryBudget) -> Result<ZippedInfo> {
    let body = read_list(bytes, budget)?;
    let [
        BinNode::Atom(version),
        BinNode::List(locator),
        BinNode::Atom(warnings),
        BinNode::Atom(block),
        BinNode::Atom(safe_mode),
        BinNode::Atom(profile),
        BinNode::Atom(synonym),
        BinNode::Atom(reserved),
        BinNode::Atom(active),
        BinNode::Atom(main_roles),
    ] = body.as_slice()
    else {
        return Err(Error::Unsupported("extension zipped information layout"));
    };
    let [BinNode::Atom(key_version), BinNode::Atom(key)] = locator.as_slice() else {
        return Err(Error::Unsupported("extension generation locator"));
    };
    if version.as_int() != Some(2) || key_version.as_int() != Some(1) {
        return Err(Error::Unsupported("extension generation version"));
    }
    let AtomValue::Narrow(raw) = key.value() else {
        return Err(Error::Invalid("malformed generation stream"));
    };
    let hash: [u8; 20] = raw
        .as_slice()
        .try_into()
        .map_err(|_| Error::Invalid("malformed generation stream"))?;
    let AtomValue::Narrow(raw) = block.value() else {
        return Err(Error::Unsupported("extension zipped block"));
    };
    let block: [u8; 8] = raw
        .as_slice()
        .try_into()
        .map_err(|_| Error::Unsupported("extension zipped block"))?;
    if reserved.as_int() != Some(0) {
        return Err(Error::Unsupported("extension zipped reserved value"));
    }
    let security_profile = profile
        .as_str()
        .ok_or(Error::Unsupported("extension security profile"))?;
    let synonym = synonym
        .as_str()
        .ok_or(Error::Unsupported("extension synonym"))?;
    Ok(ZippedInfo {
        hash,
        unsafe_action_warnings: flag(warnings, "extension unsafe action warnings")?,
        block,
        safe_mode: flag(safe_mode, "extension safe mode")?,
        security_profile,
        synonym: local_string(&synonym)?,
        active: flag(active, "extension activity")?,
        main_roles: flag(main_roles, "extension main roles")?,
    })
}
