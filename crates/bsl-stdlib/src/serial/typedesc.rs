use super::codec::{InternalReader, InternalWriter, ValueCodec};
use super::kind::{guid_text, parse_guid, type_by_guid, type_guid};
use crate::typedesc::{
    AllowedLength, AllowedSign, BinaryQualifiers, DateParts, DateQualifiers, NumberQualifiers,
    StringQualifiers, TypeDescription, idx, new_type_description,
};
use brace::Node;
use bsl_vm::{Exception, Execution, ObjRef, TypeId, TypeIdx, Value};
use std::{mem::size_of, sync::Arc};

struct Description;

pub(super) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    vec![(idx("TypeDescription"), Arc::new(Description))]
}

impl ValueCodec for Description {
    fn internal_write(
        &self,
        ex: &mut Execution,
        value: &ObjRef,
        writer: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(td) = value.as_any().downcast_ref::<TypeDescription>() else {
            return Ok(None);
        };
        writer.account(ex, (td.types().len() as u64 + 1) * size_of::<Node>() as u64)?;
        let mut pattern = Vec::new();
        pattern
            .try_reserve_exact(td.types().len().saturating_add(1))
            .map_err(|_| ex.out_of_memory())?;
        pattern.push(Node::str("Pattern"));
        for &ty in td.types() {
            if ty == TypeId::UNDEFINED {
                continue;
            }
            writer.account(ex, 256)?;
            let fields = match ty {
                TypeId::BOOL => vec![Node::str("B")],
                TypeId::NULL => vec![Node::str("L")],
                TypeId::TYPE => vec![Node::str("T")],
                TypeId::NUMBER => {
                    let q = td.number();
                    if q == NumberQualifiers::default() {
                        vec![Node::str("N")]
                    } else {
                        vec![
                            Node::str("N"),
                            Node::int(i64::from(q.digits)),
                            Node::int(i64::from(q.fraction)),
                            Node::int(i64::from(q.sign == AllowedSign::Nonnegative)),
                        ]
                    }
                }
                TypeId::STRING => {
                    let q = td.string();
                    length_fields("S", q.length, q.allowed)
                }
                TypeId::DATE => match td.date().parts {
                    DateParts::DateTime => vec![Node::str("D")],
                    DateParts::Date => vec![Node::str("D"), Node::str("D")],
                    DateParts::Time => vec![Node::str("D"), Node::str("T")],
                },
                _ if ty == TypeId::of_catalog(idx("BinaryData")) => {
                    let q = td.binary();
                    length_fields("R", q.length, q.allowed)
                }
                _ => {
                    let Some(guid) = type_guid(ex, ty) else {
                        return Ok(None);
                    };
                    vec![Node::str("#"), Node::Atom(guid_text(&guid))]
                }
            };
            pattern.push(Node::List(fields));
        }
        Ok(Some(vec![Node::List(pattern)]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        data: &[Node],
        reader: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let [Node::List(pattern)] = data else {
            return Err(reader.format_error(ex));
        };
        let Some((tag, entries)) = pattern.split_first() else {
            return Err(reader.format_error(ex));
        };
        if tag.as_str() != Some("Pattern") {
            return Err(reader.format_error(ex));
        }
        ex.charge(entries.len() as u64)?;
        ex.reserve(
            (entries.len() as u64)
                .saturating_mul(4 * size_of::<TypeId>() as u64)
                .saturating_add(crate::collections::retained_rc_size::<TypeDescription>()),
        )?;
        let mut types = Vec::new();
        types
            .try_reserve_exact(entries.len())
            .map_err(|_| ex.out_of_memory())?;
        let mut number = None;
        let mut string = None;
        let mut date = None;
        let mut binary = None;
        for entry in entries {
            let Some(fields) = entry.as_list() else {
                return Err(reader.format_error(ex));
            };
            let Some((tag, qualifiers)) = fields.split_first() else {
                return Err(reader.format_error(ex));
            };
            let ty = match tag.as_str() {
                Some("B") if qualifiers.is_empty() => TypeId::BOOL,
                Some("L") if qualifiers.is_empty() => TypeId::NULL,
                Some("T") if qualifiers.is_empty() => TypeId::TYPE,
                Some("N") => {
                    number = Some(read_number(qualifiers).ok_or_else(|| reader.format_error(ex))?);
                    TypeId::NUMBER
                }
                Some("S") => {
                    let (length, allowed) =
                        read_length(qualifiers).ok_or_else(|| reader.format_error(ex))?;
                    string = Some(StringQualifiers { length, allowed });
                    TypeId::STRING
                }
                Some("R") => {
                    let (length, allowed) =
                        read_length(qualifiers).ok_or_else(|| reader.format_error(ex))?;
                    binary = Some(BinaryQualifiers { length, allowed });
                    TypeId::of_catalog(idx("BinaryData"))
                }
                Some("D") => {
                    let parts = match qualifiers {
                        [] => DateParts::DateTime,
                        [part] if part.as_str() == Some("D") => DateParts::Date,
                        [part] if part.as_str() == Some("T") => DateParts::Time,
                        _ => return Err(reader.format_error(ex)),
                    };
                    date = Some(DateQualifiers { parts });
                    TypeId::DATE
                }
                Some("#") => {
                    let [guid] = qualifiers else {
                        return Err(reader.format_error(ex));
                    };
                    let guid = guid
                        .as_atom()
                        .filter(|text| text.len() == 36)
                        .and_then(parse_guid)
                        .ok_or_else(|| reader.format_error(ex))?;
                    type_by_guid(ex, &guid).ok_or_else(|| reader.format_error(ex))?
                }
                _ => return Err(reader.format_error(ex)),
            };
            types.push(ty);
        }
        let n = types.len() as u64;
        ex.charge(n.saturating_mul(u64::from(n.max(1).ilog2()) + 1))?;
        Ok(new_type_description(TypeDescription::new(
            &types,
            number,
            string,
            date,
            binary,
            ex.runtime(),
        )))
    }
}

fn length_fields(tag: &str, length: u32, allowed: AllowedLength) -> Vec<Node> {
    if length == 0 && allowed == AllowedLength::Variable {
        vec![Node::str(tag)]
    } else {
        vec![
            Node::str(tag),
            Node::int(i64::from(length)),
            Node::int(i64::from(allowed == AllowedLength::Variable)),
        ]
    }
}

fn read_u32(node: &Node) -> Option<u32> {
    let text = node.as_atom()?;
    (text.len() <= 10).then_some(())?;
    text.parse().ok()
}

fn read_number(fields: &[Node]) -> Option<NumberQualifiers> {
    if fields.is_empty() {
        return Some(NumberQualifiers::default());
    }
    let [digits, fraction, sign] = fields else {
        return None;
    };
    let digits = read_u32(digits)?;
    let fraction = read_u32(fraction)?;
    if digits > 32 || fraction > digits {
        return None;
    }
    let sign = match sign.as_atom()? {
        "0" => AllowedSign::Any,
        "1" => AllowedSign::Nonnegative,
        _ => return None,
    };
    Some(NumberQualifiers {
        digits,
        fraction,
        sign,
    })
}

fn read_length(fields: &[Node]) -> Option<(u32, AllowedLength)> {
    if fields.is_empty() {
        return Some((0, AllowedLength::Variable));
    }
    let [length, allowed] = fields else {
        return None;
    };
    let allowed = match allowed.as_atom()? {
        "0" => AllowedLength::Fixed,
        "1" => AllowedLength::Variable,
        _ => return None,
    };
    Some((read_u32(length)?, allowed))
}
