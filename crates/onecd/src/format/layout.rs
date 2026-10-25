use crate::error::{Error, Result};
use crate::format::brace::bad_descr;
use crate::format::descr::{FieldDescr, FieldType, TableDescr};
use crate::format::record::FLAG_FREE;

const MIN_RECORD_LEN: u64 = 5;
const MAX_RECORD_LEN: u64 = 1 << 24;

const HEADER_ROWVERSION: u32 = 17;
const HEADER_HIDDEN_VERSION: u32 = 9;
const HEADER_PLAIN: u32 = 1;

pub fn value_size(f: &FieldDescr) -> u32 {
    let n = u64::from(f.length);
    let size = match f.ty {
        FieldType::N => (n + 1).div_ceil(2),
        FieldType::C | FieldType::B => n,
        FieldType::NC => n * 2,
        FieldType::VC | FieldType::VB => n + 2,
        FieldType::NVC => n * 2 + 2,
        FieldType::T | FieldType::NT | FieldType::I => 8,
        FieldType::L => 1,
        FieldType::D => 4,
        FieldType::DT => 7,
        FieldType::RV => 16,
    };
    u32::try_from(size).unwrap_or(u32::MAX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSlot {
    pub offset: u32,
    pub value_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordLayout {
    pub record_len: u32,
    pub header_len: u32,
    pub has_rowversion: bool,
    pub has_hidden_version: bool,
    pub slots: Vec<FieldSlot>,
}

impl RecordLayout {
    pub fn new(d: &TableDescr) -> Result<RecordLayout> {
        if let Some(f) = d
            .fields
            .iter()
            .find(|f| f.ty == FieldType::RV && f.nullable)
        {
            return Err(bad_descr(format!(
                "table {}: row version field {} cannot be nullable",
                d.name, f.name
            )));
        }
        let has_rowversion = d.fields.iter().any(|f| f.ty == FieldType::RV);
        let has_hidden_version = d.recordlock && !has_rowversion;
        let header_len = if has_rowversion {
            HEADER_ROWVERSION
        } else if has_hidden_version {
            HEADER_HIDDEN_VERSION
        } else {
            HEADER_PLAIN
        };

        let mut end = u64::from(header_len);
        let mut slots = Vec::with_capacity(d.fields.len());
        for f in &d.fields {
            let size = value_size(f);
            if f.ty == FieldType::RV {
                slots.push(FieldSlot {
                    offset: 1,
                    value_size: size,
                });
                continue;
            }
            let offset = u32::try_from(end).map_err(|_| too_long(d))?;
            slots.push(FieldSlot {
                offset,
                value_size: size,
            });
            end += u64::from(size) + u64::from(f.nullable);
            if end > MAX_RECORD_LEN {
                return Err(too_long(d));
            }
        }
        let record_len = u32::try_from(end.max(MIN_RECORD_LEN)).map_err(|_| too_long(d))?;
        Ok(RecordLayout {
            record_len,
            header_len,
            has_rowversion,
            has_hidden_version,
            slots,
        })
    }

    pub fn empty_template(&self, d: &TableDescr) -> Vec<u8> {
        let mut buf = vec![0u8; self.record_len as usize];
        if let Some(flag) = buf.first_mut() {
            *flag = FLAG_FREE;
        }
        for (slot, f) in self.slots.iter().zip(&d.fields) {
            if f.nullable || f.ty == FieldType::RV {
                continue;
            }
            let start = slot.offset as usize;
            let dst = start
                .checked_add(slot.value_size as usize)
                .and_then(|end| buf.get_mut(start..end));
            if let Some(dst) = dst {
                fill_empty_value(f.ty, dst);
            }
        }
        buf
    }
}

fn too_long(d: &TableDescr) -> Error {
    bad_descr(format!(
        "table {}: record is longer than {MAX_RECORD_LEN} bytes",
        d.name
    ))
}

fn fill_empty_value(ty: FieldType, dst: &mut [u8]) {
    match ty {
        FieldType::N => {
            if let Some(first) = dst.first_mut() {
                *first = 0x10;
            }
        }
        FieldType::C => dst.fill(b' '),
        FieldType::NC => fill_utf16_spaces(dst),
        FieldType::VC => {
            if let Some(tail) = dst.get_mut(2..) {
                tail.fill(b' ');
            }
        }
        FieldType::NVC => {
            if let Some(tail) = dst.get_mut(2..) {
                fill_utf16_spaces(tail);
            }
        }
        _ => {}
    }
}

fn fill_utf16_spaces(dst: &mut [u8]) {
    for unit in dst.as_chunks_mut::<2>().0.iter_mut() {
        unit.copy_from_slice(&[b' ', 0]);
    }
}
