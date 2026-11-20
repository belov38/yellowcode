use super::{Control, Error, Result};
use brace::{
    Node,
    binary::{AtomValue, BinNode},
};
use onecd::{FieldType, TableDescr};

pub(super) struct Field {
    pub logical: &'static str,
    pub physical: &'static str,
    pub code: &'static str,
    pub length: i64,
    pub precision: i64,
    pub physical_type: FieldType,
    pub physical_length: u32,
}
const fn field(
    logical: &'static str,
    physical: &'static str,
    code: &'static str,
    length: i64,
    precision: i64,
    physical_type: FieldType,
    physical_length: u32,
) -> Field {
    Field {
        logical,
        physical,
        code,
        length,
        precision,
        physical_type,
        physical_length,
    }
}
pub(super) const FIELDS: [Field; 10] = [
    field("ID", "_IDRRef", "R", 0, 0, FieldType::B, 16),
    field(
        "ExtensionOrder",
        "_ExtensionOrder",
        "N",
        9,
        0,
        FieldType::N,
        9,
    ),
    field(
        "ExtName",
        "_ExtName",
        "S",
        0x80000100 - 1,
        0,
        FieldType::NVC,
        255,
    ),
    field("UpdateTime", "_UpdateTime", "T", 0, 0, FieldType::DT, 0),
    field(
        "ExtensionUsePurpose",
        "_ExtensionUsePurpose",
        "N",
        2,
        0,
        FieldType::N,
        2,
    ),
    field(
        "ExtensionScope",
        "_ExtensionScope",
        "N",
        2,
        0,
        FieldType::N,
        2,
    ),
    field(
        "ExtensionZippedInfo",
        "_ExtensionZippedInfo",
        "B",
        0x80000000,
        0,
        FieldType::I,
        0,
    ),
    field(
        "MasterNode",
        "_MasterNode",
        "S",
        0x80000000,
        0,
        FieldType::NT,
        0,
    ),
    field(
        "UsedInDistributedInfoBase",
        "_UsedInDistributedInfoBase",
        "L",
        0,
        0,
        FieldType::L,
        0,
    ),
    field("Version", "_Version", "V", 0, 0, FieldType::RV, 0),
];
trait View: Sized {
    fn list(&self) -> Option<&[Self]>;
    fn number(&self) -> Option<i64>;
    fn text_eq(&self, value: &str) -> bool;
}
impl View for Node {
    fn list(&self) -> Option<&[Self]> {
        self.as_list()
    }
    fn number(&self) -> Option<i64> {
        let text = self.as_atom()?;
        let digits = text.strip_prefix('-').unwrap_or(text);
        let canonical = match digits.as_bytes() {
            [b'0'] => digits.len() == text.len(),
            [first, rest @ ..] => {
                (b'1'..=b'9').contains(first) && rest.iter().all(u8::is_ascii_digit)
            }
            [] => false,
        };
        if canonical { text.parse().ok() } else { None }
    }
    fn text_eq(&self, value: &str) -> bool {
        self.as_str() == Some(value)
    }
}
impl View for BinNode {
    fn list(&self) -> Option<&[Self]> {
        if let Self::List(v) = self {
            Some(v)
        } else {
            None
        }
    }
    fn number(&self) -> Option<i64> {
        if let Self::Atom(a) = self {
            i64::try_from(a.as_int()?).ok()
        } else {
            None
        }
    }
    fn text_eq(&self, value: &str) -> bool {
        let Self::Atom(a) = self else { return false };
        match a.value() {
            AtomValue::Wide(units) => units.iter().copied().eq(value.encode_utf16()),
            AtomValue::Narrow(bytes) => bytes.iter().map(|b| char::from(*b)).eq(value.chars()),
            AtomValue::Int(0) if a.tag() == 1 => value.is_empty(),
            AtomValue::Int(n) if a.tag() == 0x0b => value
                .as_bytes()
                .first()
                .is_some_and(|b| value.len() == 1 && i128::from(*b) == *n),
            _ => false,
        }
    }
}
fn bad() -> Error {
    Error::Invalid("logical inventory layout")
}
fn list<T: View>(node: &T) -> Result<&[T]> {
    node.list().ok_or_else(bad)
}
fn counted<T: View>(node: &T) -> Result<&[T]> {
    let (n, rows) = list(node)?.split_first().ok_or_else(bad)?;
    if n.number().and_then(|n| usize::try_from(n).ok()) != Some(rows.len()) {
        return Err(bad());
    }
    Ok(rows)
}
#[derive(Debug, Clone, Copy)]
pub struct Projection {
    pub info_ordinal: usize,
    pub ngs_ordinal: usize,
    pub tables: usize,
}
impl Projection {
    pub fn from_text(root: &Node, control: &Control) -> Result<Self> {
        project(root, control)
    }
    pub fn from_binary(root: &BinNode, control: &Control) -> Result<Self> {
        project(root, control)
    }
}
fn project<T: View>(root: &T, control: &Control) -> Result<Projection> {
    let [zero, tables] = list(root)? else {
        return Err(bad());
    };
    if zero.number() != Some(0) {
        return Err(bad());
    }
    let mut result = Walk {
        info: None,
        ngs: None,
        tables: 0,
        control,
    };
    result.tables(counted(tables)?, 1)?;
    Ok(Projection {
        info_ordinal: result
            .info
            .ok_or(Error::Unsupported("missing ExtensionsInfo layout"))?,
        ngs_ordinal: result
            .ngs
            .ok_or(Error::Unsupported("missing ExtensionsInfoNGS layout"))?,
        tables: result.tables,
    })
}
struct Walk<'a> {
    info: Option<usize>,
    ngs: Option<usize>,
    tables: usize,
    control: &'a Control,
}
impl Walk<'_> {
    fn tables<T: View>(&mut self, tables: &[T], depth: usize) -> Result<()> {
        if depth > 8 {
            return Err(Error::Limit("table depth"));
        }
        for table in tables {
            self.control.check()?;
            let [
                name,
                kind,
                _number,
                parent,
                fields,
                nested,
                _indexes,
                one,
                key,
                _s0,
                _s1,
                tail,
                t0,
                t1,
            ] = list(table)?
            else {
                return Err(bad());
            };
            let ordinal = self.tables;
            self.tables = self.tables.checked_add(1).ok_or_else(bad)?;
            let ngs = name.text_eq("ExtensionsInfoNGS");
            if ngs || name.text_eq("ExtensionsInfo") {
                let selected = if ngs { &mut self.ngs } else { &mut self.info };
                if selected.replace(ordinal).is_some()
                    || !kind.text_eq("N")
                    || !parent.text_eq("")
                    || one.number() != Some(1)
                    || !key.text_eq("R")
                    || !tail.text_eq("")
                    || t0.number() != Some(0)
                    || t1.number() != Some(0)
                {
                    return Err(bad());
                }
                validate_fields(
                    counted(fields)?,
                    if ngs {
                        "ExtensionsInfoNGS"
                    } else {
                        "ExtensionsInfo"
                    },
                )?;
                if !counted(nested)?.is_empty() {
                    return Err(bad());
                }
            }
            let nested = counted(nested)?;
            if !nested.is_empty() {
                self.tables(nested, depth + 1)?;
            }
        }
        Ok(())
    }
}
fn validate_fields<T: View>(fields: &[T], table: &str) -> Result<()> {
    if fields.len() != FIELDS.len() {
        return Err(bad());
    }
    for (node, spec) in fields.iter().zip(&FIELDS) {
        let [name, nullable, types, tail, zero] = list(node)? else {
            return Err(bad());
        };
        let [ty] = counted(types)? else {
            return Err(bad());
        };
        let [code, length, precision, reference, kind] = list(ty)? else {
            return Err(bad());
        };
        let own = spec.code == "R";
        if !name.text_eq(spec.logical)
            || nullable.number() != Some(0)
            || !tail.text_eq("")
            || zero.number() != Some(0)
            || !code.text_eq(spec.code)
            || length.number() != Some(spec.length)
            || precision.number() != Some(spec.precision)
            || !reference.text_eq(if own { table } else { "" })
            || kind.number() != Some(if own { 2 } else { 0 })
        {
            return Err(bad());
        }
    }
    Ok(())
}
pub(super) fn validate_physical(descr: &TableDescr) -> Result<()> {
    if descr.fields.len() != FIELDS.len() {
        return Err(Error::Invalid("physical inventory field count"));
    }
    for (actual, want) in descr.fields.iter().zip(&FIELDS) {
        if !actual.name.eq_ignore_ascii_case(want.physical)
            || actual.ty != want.physical_type
            || actual.length != want.physical_length
            || i64::from(actual.precision) != want.precision
            || actual.nullable
        {
            return Err(Error::Invalid("physical inventory column"));
        }
    }
    Ok(())
}
