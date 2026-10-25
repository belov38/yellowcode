use bsl_prim::{Collation, Number, Strength};
use ibschema::values::type_code;
use query_ir::QValue;

const NULL: u8 = 0;
const TYPE: u8 = 9;
const SYS_ENUM: u8 = 10;
const BINARY: u8 = 11;
const STORAGE: u8 = 12;
const NESTED: u8 = 13;
const POINT_IN_TIME: u8 = 14;
const BOUNDARY: u8 = 15;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(pub Vec<u8>);

fn number_body(n: &Number, out: &mut Vec<u8>) {
    if n.is_zero() {
        out.push(0x02);
        return;
    }
    let int = n.int_digits();
    let (exp, digits): (i32, String) = if int != "0" {
        let e = i32::try_from(int.len()).unwrap_or(i32::MAX);
        (e, format!("{int}{}", n.frac_digits()))
    } else {
        let frac = n.frac_digits();
        let lead = frac.bytes().take_while(|&b| b == b'0').count();
        let e = i32::try_from(lead).map_or(i32::MIN, |l| -l);
        (e, frac.get(lead..).unwrap_or("").to_string())
    };
    let mut body = Vec::with_capacity(digits.len() + 5);
    body.extend_from_slice(&((exp as u32) ^ 0x8000_0000).to_be_bytes());
    body.extend(digits.bytes().map(|b| b - b'0' + 1));
    body.push(0);
    if n.is_negative() {
        out.push(0x01);
        out.extend(body.iter().map(|b| !b));
    } else {
        out.push(0x03);
        out.extend(body);
    }
}

pub(crate) fn key_text(s: &[u16]) -> &[u16] {
    let end = s.iter().rposition(|&u| u != 0x20).map_or(0, |p| p + 1);
    s.get(..end).unwrap_or(&[])
}

fn moment_body(m: &query_ir::value::Moment, out: &mut Vec<u8>) {
    out.extend_from_slice(&((m.date.ticks() as u64) ^ (1 << 63)).to_be_bytes());
    match &m.reference {
        None => out.push(0),
        Some((table, id)) => {
            out.push(1);
            out.extend_from_slice(&table.to_be_bytes());
            out.extend_from_slice(id);
        }
    }
}

pub fn key_of(v: &QValue, coll: &dyn Collation) -> Key {
    let mut k = Vec::new();
    match v {
        QValue::Null => k.push(NULL),
        QValue::Undefined => k.push(type_code::UNDEFINED),
        QValue::Bool(b) => {
            k.push(type_code::BOOL);
            k.push(u8::from(*b));
        }
        QValue::Number(n) => {
            k.push(type_code::NUMBER);
            number_body(n, &mut k);
        }
        QValue::Date(d) => {
            k.push(type_code::DATE);
            k.extend_from_slice(&((d.ticks() as u64) ^ (1 << 63)).to_be_bytes());
        }
        QValue::String(s) => {
            k.push(type_code::STRING);
            k.extend(coll.sort_key(Strength::Secondary, key_text(&s.0)));
        }
        QValue::Uuid(u) => {
            k.push(type_code::UUID);
            k.extend_from_slice(u);
        }
        QValue::Ref { table, id } => {
            k.push(type_code::REF);
            k.extend_from_slice(&table.to_be_bytes());
            k.extend_from_slice(id);
        }
        QValue::Type(t) => {
            k.push(TYPE);
            k.extend(format!("{t:?}").bytes());
        }
        QValue::Binary(b) => {
            k.push(BINARY);
            k.extend_from_slice(b);
        }
        QValue::Storage(b) => {
            k.push(STORAGE);
            k.extend_from_slice(b);
        }
        QValue::Nested(_) => k.push(NESTED),
        QValue::SysEnum(t, i) => {
            k.push(SYS_ENUM);
            k.extend_from_slice(&t.0.to_be_bytes());
            k.extend_from_slice(&i.to_be_bytes());
        }
        QValue::PointInTime(m) => {
            k.push(POINT_IN_TIME);
            moment_body(m, &mut k);
        }
        QValue::Boundary(b) => {
            k.push(BOUNDARY);
            k.push(u8::from(b.include));
            k.extend(key_of(&b.point, coll).0);
        }
    }
    Key(k)
}

pub fn key_of_totals(v: &QValue, coll: &dyn Collation) -> Key {
    match v {
        QValue::String(s) => {
            let mut k = vec![type_code::STRING];
            k.extend(coll.sort_key(Strength::Secondary, &s.0));
            Key(k)
        }
        _ => key_of(v, coll),
    }
}
