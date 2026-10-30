use std::io::{Read, Write};

use brace::binary::{Atom, BinNode, Writer};
use ibschema::columns::{ColRole, Column};
use ibschema::values::{ColumnValue, RefId, datetime_from_ticks, datetime_to_ticks};
use onecd::Numeric;

use crate::{Error, Result};

pub fn decode_row(cols: &[Column], row: &[BinNode], at: u64) -> Result<Vec<ColumnValue>> {
    decode_in("", cols, row, at)
}

pub fn encode_row<W: Write>(
    w: &mut Writer<W>,
    cols: &[Column],
    vals: &[ColumnValue],
    chunk: usize,
) -> Result<()> {
    check_row("", cols, vals)?;
    write_row(w, cols, vals, chunk)
}

struct Place<'a> {
    table: &'a str,
    column: &'a str,
    at: u64,
}

impl Place<'_> {
    fn name(&self) -> String {
        if self.table.is_empty() {
            self.column.to_string()
        } else {
            format!("{}.{}", self.table, self.column)
        }
    }

    fn dt(&self, what: &str) -> Error {
        Error::Dt {
            offset: self.at,
            what: format!("{}: {what}", self.name()),
        }
    }

    fn schema(&self, e: ibschema::Error) -> Error {
        let here = format!("{} (row at {})", self.name(), self.at);
        Error::Schema(match e {
            ibschema::Error::Unsupported(m) => ibschema::Error::Unsupported(format!("{here}: {m}")),
            ibschema::Error::Parse { what, at, why } => ibschema::Error::Parse {
                what,
                at: format!("{here}: {at}"),
                why,
            },
            other => other,
        })
    }
}

pub(crate) fn decode_in(
    table: &str,
    cols: &[Column],
    row: &[BinNode],
    at: u64,
) -> Result<Vec<ColumnValue>> {
    let mut items = row.iter();
    let mut out = Vec::with_capacity(cols.len());
    for c in cols {
        let p = Place {
            table,
            column: &c.name,
            at,
        };
        let flag = items.next().ok_or_else(|| p.dt("row too short"))?;
        match flag {
            BinNode::Atom(a) if a.as_int() == Some(0) => {
                out.push(ColumnValue::Null);
                continue;
            }
            BinNode::Atom(a) if a.as_int() == Some(1) => {}
            _ => return Err(p.dt("NULL flag must be 0 or 1")),
        }
        let v = items
            .next()
            .ok_or_else(|| p.dt("row too short: no value after flag 1"))?;
        out.push(decode_value(c, v, &p)?);
    }
    let extra = items.count();
    if extra != 0 {
        let what = format!("row has {extra} items after {} columns", cols.len());
        return Err(Error::Dt {
            offset: at,
            what: if table.is_empty() {
                what
            } else {
                format!("{table}: {what}")
            },
        });
    }
    Ok(out)
}

fn decode_value(c: &Column, v: &BinNode, p: &Place) -> Result<ColumnValue> {
    let atom = || match v {
        BinNode::Atom(a) => Ok(a),
        BinNode::List(_) => Err(p.dt("expected a value, got a list")),
    };
    Ok(match c.role {
        ColRole::Binary => ColumnValue::Bytes(decode_binary(v, p)?),
        ColRole::Ref => {
            let g = atom()?.as_guid().ok_or_else(|| p.dt("expected a GUID"))?;
            ColumnValue::Bytes(RefId::from_uuid(g).0.to_vec())
        }
        ColRole::TableNo | ColRole::KeyField => {
            let n = atom()?
                .as_int()
                .and_then(|i| u32::try_from(i).ok())
                .ok_or_else(|| p.dt("expected an integer 0..=4294967295"))?;
            ColumnValue::Bytes(n.to_be_bytes().to_vec())
        }
        ColRole::TypeCode => {
            let n = atom()?
                .as_int()
                .and_then(|i| u8::try_from(i).ok())
                .ok_or_else(|| p.dt("expected an integer 0..=255"))?;
            ColumnValue::Bytes(vec![n])
        }
        ColRole::Bool => {
            ColumnValue::Bool(atom()?.as_bool().ok_or_else(|| p.dt("expected 0 or 1"))?)
        }
        ColRole::Number => {
            let a = atom()?;
            let n = match a.as_int() {
                Some(i) => Numeric::parse(&i.to_string()),
                None => a.as_numeric_text().and_then(Numeric::parse),
            };
            ColumnValue::Number(n.ok_or_else(|| p.dt("expected a number"))?)
        }
        ColRole::Str => {
            let u = atom()?
                .as_utf16()
                .ok_or_else(|| p.dt("expected a string"))?;
            let s = String::from_utf16(&u).map_err(|_| {
                p.schema(ibschema::Error::Unsupported(
                    "lone surrogate in a string".into(),
                ))
            })?;
            ColumnValue::Str(s)
        }
        ColRole::DateTime => {
            let t = atom()?
                .as_int()
                .and_then(|i| u64::try_from(i).ok())
                .ok_or_else(|| p.dt("expected ticks (a non-negative integer)"))?;
            ColumnValue::DateTime(datetime_from_ticks(t).map_err(|e| p.schema(e))?)
        }
        ColRole::RowVersion => {
            return Err(Error::Unsupported(format!(
                "{}: row version columns are not in a .dt",
                p.name()
            )));
        }
    })
}

fn decode_binary(v: &BinNode, p: &Place) -> Result<Vec<u8>> {
    let BinNode::List(items) = v else {
        return Err(p.dt("binary value must be a list"));
    };
    match items.split_first() {
        Some((BinNode::Atom(a), chunks)) if a.as_int() == Some(0) => {
            let mut out = Vec::new();
            for c in chunks {
                let b = match c {
                    BinNode::Atom(a) => a.as_bytes(),
                    BinNode::List(_) => None,
                };
                out.extend(b.ok_or_else(|| p.dt("binary chunk must be bytes"))?);
            }
            Ok(out)
        }
        _ => Err(p.dt("binary list must start with 0")),
    }
}

pub(crate) fn check_row(table: &str, cols: &[Column], vals: &[ColumnValue]) -> Result<()> {
    let unfit = |column: &str, why: String| {
        Error::Schema(ibschema::Error::ValueDoesNotFit {
            table: table.to_string(),
            column: column.to_string(),
            why,
        })
    };
    if cols.len() != vals.len() {
        return Err(unfit(
            "",
            format!("row of {} values for {} columns", vals.len(), cols.len()),
        ));
    }
    for (c, v) in cols.iter().zip(vals) {
        c.check(v).map_err(|why| unfit(&c.name, why))?;
    }
    Ok(())
}

pub(crate) fn write_row<W: Write>(
    w: &mut Writer<W>,
    cols: &[Column],
    vals: &[ColumnValue],
    chunk: usize,
) -> Result<()> {
    w.open()?;
    for (c, v) in cols.iter().zip(vals) {
        if matches!(v, ColumnValue::Null) {
            w.int(0)?;
        } else {
            w.int(1)?;
            write_value(w, c, v, chunk)?;
        }
    }
    w.close()?;
    Ok(())
}

pub(crate) fn write_folder_stream<W: Write>(
    w: &mut Writer<W>,
    cols: &[Column],
    metadata: &[ColumnValue],
    chunk: usize,
    length: u64,
    input: &mut impl Read,
) -> Result<()> {
    w.open()?;
    for (c, v) in cols.iter().zip(metadata) {
        if matches!(v, ColumnValue::Null) {
            w.int(0)?;
        } else {
            w.int(1)?;
            write_value(w, c, v, chunk)?;
        }
    }
    w.int(1)?;
    w.open()?;
    w.int(0)?;
    let mut remaining = length;
    let mut buffer = [0_u8; crate::writer::FOLDER_CHUNK];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| Error::Unsupported("folder chunk size".into()))?;
        let piece = buffer
            .get_mut(..take)
            .ok_or_else(|| Error::Unsupported("folder chunk size".into()))?;
        input.read_exact(piece)?;
        w.bytes(piece)?;
        remaining -= take as u64;
    }
    let probe = buffer
        .get_mut(..1)
        .ok_or_else(|| Error::Unsupported("empty folder chunk buffer".into()))?;
    if input.read(probe)? != 0 {
        return Err(Error::Unsupported(
            "folder input exceeds declared size".into(),
        ));
    }
    w.close()?;
    w.close()?;
    Ok(())
}

fn write_value<W: Write>(
    w: &mut Writer<W>,
    c: &Column,
    v: &ColumnValue,
    chunk: usize,
) -> Result<()> {
    let mismatch = || {
        Error::Schema(ibschema::Error::ValueDoesNotFit {
            table: String::new(),
            column: c.name.clone(),
            why: format!("value kind does not match {:?}", c.ty),
        })
    };
    match (c.role, v) {
        (ColRole::Ref, ColumnValue::Bytes(b)) => {
            let id: [u8; 16] = b.as_slice().try_into().map_err(|_| mismatch())?;
            w.guid(&RefId(id).to_uuid())?;
        }
        (ColRole::TableNo | ColRole::KeyField, ColumnValue::Bytes(b)) => {
            let n: [u8; 4] = b.as_slice().try_into().map_err(|_| mismatch())?;
            w.int(i128::from(u32::from_be_bytes(n)))?;
        }
        (ColRole::TypeCode, ColumnValue::Bytes(b)) => {
            let [t] = b.as_slice() else {
                return Err(mismatch());
            };
            w.int(i128::from(*t))?;
        }
        (ColRole::Binary, ColumnValue::Bytes(b)) => {
            w.open()?;
            w.int(0)?;
            if chunk == 0 {
                if !b.is_empty() {
                    w.bytes(b)?;
                }
            } else {
                for part in b.chunks(chunk) {
                    w.bytes(part)?;
                }
            }
            w.close()?;
        }
        (ColRole::Bool, ColumnValue::Bool(x)) => w.bool(*x)?,
        (ColRole::Number, ColumnValue::Number(n)) => {
            let a = Atom::numeric_text(&n.to_string()).ok_or_else(mismatch)?;
            w.atom(&a)?;
        }
        (ColRole::Str, ColumnValue::Str(s)) => w.str(s)?,
        (ColRole::DateTime, ColumnValue::DateTime(d)) => {
            w.int(i128::from(datetime_to_ticks(d)))?;
        }
        _ => return Err(mismatch()),
    }
    Ok(())
}
