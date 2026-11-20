use crate::model::Uuid16;
use ibschema::values::RefId;
use sha1::{Digest, Sha1};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyValue {
    Date(u64),
    Ref {
        type_id: Uuid16,
        table: u32,
        id: RefId,
    },
    Str(String),
    Int(u64),
}

struct ListWriter {
    out: Vec<u8>,
    pending: u8,
    need: bool,
    sep: bool,
}

impl ListWriter {
    fn new() -> ListWriter {
        ListWriter {
            out: Vec::new(),
            pending: 0,
            need: false,
            sep: true,
        }
    }

    fn flush_pending(&mut self, mask: u8) {
        if self.pending & mask != 0 {
            self.out.push(self.pending);
            self.pending = 0;
        }
    }

    fn separator(&mut self) {
        if self.need && self.sep {
            self.flush_pending(0xdf);
            self.pending |= 0x80;
            self.need = false;
        }
    }

    fn typed(&mut self, ty: u8, data: &[u8]) {
        self.flush_pending(0x1f);
        self.pending |= ty;
        self.out.push(self.pending);
        self.pending = 0;
        self.out.extend_from_slice(data);
    }

    fn begin(&mut self) {
        self.flush_pending(0x5f);
        if self.need && self.sep {
            self.flush_pending(0xdf);
            self.need = false;
            self.pending |= 0x80;
        }
        self.pending |= 0x40;
    }

    fn end(&mut self) {
        if self.pending != 0 {
            self.out.push(self.pending);
        }
        self.need = true;
        self.pending = 0x20;
    }

    fn uint(&mut self, n: u64) {
        self.separator();
        match n {
            0..=9 => self.typed(u8::try_from(n + 1).unwrap_or(1), &[]),
            10..=0xff => self.typed(0x0b, &[u8::try_from(n).unwrap_or(0)]),
            0x100..=0xffff => self.typed(0x0d, &u16::try_from(n).unwrap_or(0).to_le_bytes()),
            0x1_0000..=0xffff_ffff => {
                self.typed(0x0f, &u32::try_from(n).unwrap_or(0).to_le_bytes())
            }
            _ => self.typed(0x11, &n.to_le_bytes()),
        }
        self.need = true;
    }

    fn string(&mut self, s: &str) {
        self.separator();
        let chars: Vec<char> = s.chars().collect();
        match chars.as_slice() {
            [] => self.typed(1, &[]),
            [c] if u32::from(*c) < 0x100 => {
                self.typed(0x0b, &[u8::try_from(u32::from(*c)).unwrap_or(0)])
            }
            _ if chars.iter().all(|c| u32::from(*c) < 0x100) => {
                let bytes: Vec<u8> = chars
                    .iter()
                    .map(|c| u8::try_from(u32::from(*c)).unwrap_or(0))
                    .collect();
                self.counted(0x1a, &bytes, bytes.len());
            }
            _ => {
                let units: Vec<u16> = s.encode_utf16().collect();
                let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
                self.counted(0x17, &bytes, units.len());
            }
        }
        self.need = true;
    }

    fn counted(&mut self, base: u8, bytes: &[u8], count: usize) {
        let mut data = Vec::with_capacity(bytes.len() + 8);
        if let Ok(n) = u8::try_from(count) {
            data.push(n);
            data.extend_from_slice(bytes);
            self.typed(base, &data);
        } else if let Ok(n) = u16::try_from(count) {
            data.extend_from_slice(&n.to_le_bytes());
            data.extend_from_slice(bytes);
            self.typed(base + 1, &data);
        } else {
            data.extend_from_slice(&(count as u64).to_le_bytes());
            data.extend_from_slice(bytes);
            self.typed(base + 2, &data);
        }
    }

    fn guid(&mut self, g: [u8; 16]) {
        self.separator();
        if g == [0; 16] {
            self.typed(1, &[]);
        } else {
            self.typed(0x15, &g);
        }
        self.need = true;
    }

    fn object(&mut self, items: impl FnOnce(&mut ListWriter)) {
        self.separator();
        self.sep = false;
        items(self);
        self.need = true;
        self.sep = true;
    }

    fn close(mut self) -> Vec<u8> {
        if self.pending != 0 {
            self.out.push(self.pending);
        }
        self.out
    }

    fn value(&mut self, v: &KeyValue) {
        self.begin();
        match v {
            KeyValue::Date(ticks) => {
                self.string("D");
                self.object(|w| w.uint(*ticks));
            }
            KeyValue::Ref { type_id, table, id } => {
                self.string("#");
                self.guid(type_id.to_bytes_le());
                self.object(|w| {
                    w.uint(u64::from(*table));
                    w.guid(id.to_uuid().to_bytes_le());
                });
            }
            KeyValue::Str(s) => {
                self.string("S");
                self.object(|w| w.string(s));
            }
            KeyValue::Int(n) => {
                self.string("N");
                self.object(|w| w.uint(*n));
            }
        }
        self.end();
    }
}

pub(crate) fn key_stream(values: &[KeyValue]) -> Vec<u8> {
    let mut w = ListWriter::new();
    for v in values {
        w.value(v);
    }
    w.close()
}

pub(crate) fn record_id(values: &[KeyValue]) -> [u8; 20] {
    if values.is_empty() {
        return [0; 20];
    }
    Sha1::digest(key_stream(values)).into()
}

pub(crate) fn object_id(id: RefId) -> [u8; 20] {
    let mut out = [0u8; 20];
    for (o, b) in out.iter_mut().zip(id.0) {
        *o = b;
    }
    out
}
