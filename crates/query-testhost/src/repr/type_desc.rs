use brace::{Guid, LimitedParseError, Node};
use memory_budget::{BudgetError, Category, Charged, Limits, MemoryBudget};
use platform_catalog::catalog;
use query_ir::{DateQ, NumQ, Other, QType, QuerySchema, RefAtom, StrQ};

const STORAGE_PLAIN_HEADER: [u8; 2] = [0x01, 0x01];
const HEADER_LEN: usize = 10;
const MAX_TEXT_BYTES: usize = 1 << 20;
const MAX_NODES: usize = 16_384;
const MAX_DEPTH: usize = 8;
const MAX_NUMBER_DIGITS: u8 = 32;

const TYPE_DESCRIPTION_GUID_BYTES: [u8; 16] = [
    0xf5, 0xc6, 0x50, 0x50, 0x3b, 0xbb, 0x11, 0xd5, 0xb9, 0x88, 0x00, 0x50, 0xba, 0xe0, 0xa9, 0x5d,
];

pub fn column_may_be_type_description(ty: &QType) -> bool {
    ty.others.contains(&Other::TypeDescription)
}

pub fn decode_value_type_blob(bytes: &[u8], schema: &QuerySchema) -> Option<QType> {
    let budget = MemoryBudget::new(Limits::new(u64::MAX));
    try_decode_value_type_blob(bytes, schema, &budget)
        .ok()
        .flatten()
        .map(|charged| (*charged).clone())
}

pub fn try_decode_value_type_blob(
    bytes: &[u8],
    schema: &QuerySchema,
    budget: &MemoryBudget,
) -> Result<Option<Charged<QType>>, BudgetError> {
    let Some(payload) = extract_plain_payload(bytes) else {
        return Ok(None);
    };
    let node = match brace::parse_bytes_budgeted_limited(
        payload,
        budget,
        Category::Execution,
        MAX_NODES,
        MAX_DEPTH,
    ) {
        Ok(node) => node,
        Err(LimitedParseError::Memory(err)) => return Err(err),
        Err(LimitedParseError::Parse(_) | LimitedParseError::Policy { .. }) => return Ok(None),
    };
    let Some(parsed) = parse_type_description(&node, schema) else {
        return Ok(None);
    };
    drop(node);
    let charged = parsed.try_clone_budgeted(budget)?;
    Ok(Some(charged))
}

fn extract_plain_payload(bytes: &[u8]) -> Option<&[u8]> {
    let header = bytes.get(..HEADER_LEN)?;
    if header.get(..2) != Some(&STORAGE_PLAIN_HEADER) {
        return None;
    }
    let len_bytes: [u8; 8] = header.get(2..HEADER_LEN)?.try_into().ok()?;
    let declared = u64::from_le_bytes(len_bytes);
    if declared == 0 || declared > MAX_TEXT_BYTES as u64 {
        return None;
    }
    let len = usize::try_from(declared).ok()?;
    let rest = bytes.get(HEADER_LEN..)?;
    if rest.len() != len {
        return None;
    }
    Some(rest)
}

fn parse_type_description(root: &Node, schema: &QuerySchema) -> Option<QType> {
    let [tag, guid_node, pattern_node] = root.as_list()? else {
        return None;
    };
    if tag.as_str() != Some("#") {
        return None;
    }
    let root_guid = parse_guid_atom(guid_node)?;
    if *root_guid.as_bytes() != TYPE_DESCRIPTION_GUID_BYTES {
        return None;
    }
    let pattern = pattern_node.as_list()?;
    let (pattern_tag, entries) = pattern.split_first()?;
    if pattern_tag.as_str() != Some("Pattern") {
        return None;
    }

    let mut out = QType::default();
    for entry in entries {
        let fields = entry.as_list()?;
        let (kind, qualifiers) = fields.split_first()?;
        match kind.as_str()? {
            "B" if qualifiers.is_empty() => {
                out.boolean = true;
            }
            "L" if qualifiers.is_empty() => {
                out.null = true;
            }
            "T" if qualifiers.is_empty() => {
                out.others.push(Other::Type);
            }
            "N" => {
                out.number = Some(parse_number_qualifiers(qualifiers)?);
            }
            "S" => {
                out.string = Some(parse_string_qualifiers(qualifiers)?);
            }
            "D" => {
                out.date = Some(parse_date_qualifiers(qualifiers)?);
            }
            "R" => {
                let _ = parse_length_qualifiers(qualifiers)?;
                out.others.push(Other::Binary);
            }
            "#" => {
                let [guid_node] = qualifiers else {
                    return None;
                };
                let guid = parse_guid_atom(guid_node)?;
                let resolved = resolve_guid(guid, schema)?;
                merge_resolved(&mut out, resolved);
            }
            _ => return None,
        }
    }

    out.refs.sort_unstable();
    out.refs.dedup();
    out.others.sort_unstable();
    out.others.dedup();
    Some(out)
}

fn parse_guid_atom(node: &Node) -> Option<Guid> {
    let text = node.as_atom()?;
    if text.len() != 36 {
        return None;
    }
    Guid::parse(text)
}

fn parse_u32_atom(node: &Node) -> Option<u32> {
    let text = node.as_atom()?;
    if text.is_empty() || text.len() > 10 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_u8_atom(node: &Node) -> Option<u8> {
    let v = parse_u32_atom(node)?;
    u8::try_from(v).ok()
}

fn parse_number_qualifiers(qualifiers: &[Node]) -> Option<NumQ> {
    if qualifiers.is_empty() {
        return Some(NumQ {
            digits: 0,
            scale: 0,
            nonneg: false,
        });
    }
    let [digits, scale, sign] = qualifiers else {
        return None;
    };
    let digits = parse_u8_atom(digits)?;
    let scale = parse_u8_atom(scale)?;
    if digits > MAX_NUMBER_DIGITS || scale > digits {
        return None;
    }
    let nonneg = match sign.as_atom()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    Some(NumQ {
        digits,
        scale,
        nonneg,
    })
}

fn parse_length_qualifiers(qualifiers: &[Node]) -> Option<(u32, bool)> {
    if qualifiers.is_empty() {
        return Some((0, false));
    }
    let [len, allowed] = qualifiers else {
        return None;
    };
    let len = parse_u32_atom(len)?;
    let fixed = match allowed.as_atom()? {
        "0" => true,
        "1" => false,
        _ => return None,
    };
    Some((len, fixed))
}

fn parse_string_qualifiers(qualifiers: &[Node]) -> Option<StrQ> {
    let (len, fixed) = parse_length_qualifiers(qualifiers)?;
    Some(StrQ { len, fixed })
}

fn parse_date_qualifiers(qualifiers: &[Node]) -> Option<DateQ> {
    match qualifiers {
        [] => Some(DateQ::DateTime),
        [part] => match part.as_str()? {
            "D" => Some(DateQ::Date),
            "T" => Some(DateQ::Time),
            "F" => Some(DateQ::DateTime),
            _ => None,
        },
        _ => None,
    }
}

fn resolve_guid(guid: Guid, schema: &QuerySchema) -> Option<QType> {
    for (index, info) in schema.refs().iter().enumerate() {
        if info.type_uuid == guid {
            let atom = RefAtom(u32::try_from(index).ok()?);
            return Some(QType::reference(atom));
        }
    }
    for (index, info) in schema.refs().iter().enumerate() {
        if info.kind == md_model::MdKind::BusinessProcess {
            let bp = RefAtom(u32::try_from(index).ok()?);
            let rp_type = QType::other(Other::RoutePoint(bp));
            let mut rp_guid = None;
            let _ = schema.try_visit_type_atoms(&rp_type, |g, _| {
                rp_guid = Some(g);
                Ok::<(), ()>(())
            });
            if rp_guid == Some(guid) {
                return Some(rp_type);
            }
        }
    }
    let cat = catalog();
    let idx = cat.find_type_by_guid(guid.as_bytes())?;
    let info = cat.type_info(idx);
    if let Some(qt) = schema.type_by_name(&[&info.name_en]) {
        return Some(qt);
    }
    if info.name_en == "ValueTable" {
        return Some(QType::other(Other::ValueTable));
    }
    None
}

fn merge_resolved(dst: &mut QType, src: QType) {
    dst.null |= src.null;
    dst.boolean |= src.boolean;
    if let Some(n) = src.number {
        dst.number.get_or_insert(n);
    }
    if let Some(s) = src.string {
        dst.string.get_or_insert(s);
    }
    if let Some(d) = src.date {
        dst.date.get_or_insert(d);
    }
    dst.refs.extend(src.refs);
    dst.others.extend(src.others);
}
