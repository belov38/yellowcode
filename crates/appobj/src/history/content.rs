pub(crate) mod tags {
    pub(crate) const NULL: u64 = 11;
    pub(crate) const BOOL: u64 = 12;
    pub(crate) const INT: u64 = 13;
    pub(crate) const STR: u64 = 14;
    pub(crate) const DATE: u64 = 15;
    pub(crate) const UUID: u64 = 16;
    pub(crate) const DECIMAL: u64 = 17;
    pub(crate) const REF: u64 = 18;
    pub(crate) const STORAGE: u64 = 19;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    New = 0,
    Changed = 1,
    Deleted = 2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Std(i64),
    Item([u8; 16]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Val {
    Bool(bool),
    Int(u64),
    Str(String),
    Date(u64),
    Uuid([u8; 16]),
    Decimal(String),
    Ref { id: [u8; 16], table: u32 },
    EmptyRef { table: u32 },
    Storage(Vec<u8>),
    Null,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tab {
    pub md: [u8; 16],
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug)]
pub(crate) struct Header {
    pub version: [u8; 16],
    pub key: Vec<u8>,
    pub user: [u8; 16],
    pub user_name: String,
    pub full_name: String,
    pub utc_ticks: u64,
    pub txn: [u8; 16],
    pub changed_by_extension: bool,
}

fn varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push(u8::try_from(n & 0x7f).unwrap_or(0) | 0x80);
        n >>= 7;
    }
    out.push(u8::try_from(n).unwrap_or(0));
}

fn tag(out: &mut Vec<u8>, field: u64, wire: u64) {
    varint(out, (field << 3) | wire);
}

fn bytes(out: &mut Vec<u8>, field: u64, b: &[u8]) {
    tag(out, field, 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn number(out: &mut Vec<u8>, field: u64, n: u64) {
    tag(out, field, 0);
    varint(out, n);
}

fn header(h: &Header) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    bytes(&mut out, 1, &h.version);
    bytes(&mut out, 2, &h.key);
    bytes(&mut out, 3, &h.user);
    bytes(&mut out, 4, h.user_name.as_bytes());
    bytes(&mut out, 5, h.full_name.as_bytes());
    number(&mut out, 6, h.utc_ticks);
    bytes(&mut out, 7, &h.txn);
    let mut node = Vec::with_capacity(20);
    bytes(&mut node, 1, &[0; 16]);
    number(&mut node, 2, 0);
    bytes(&mut out, 8, &node);
    if h.changed_by_extension {
        number(&mut out, 11, 1);
    }
    number(&mut out, 14, 0);
    out
}

fn value(v: &Val) -> Vec<u8> {
    let mut out = Vec::new();
    match v {
        Val::Bool(b) => number(&mut out, tags::BOOL, u64::from(*b)),
        Val::Int(n) => number(&mut out, tags::INT, *n),
        Val::Str(s) => bytes(&mut out, tags::STR, s.as_bytes()),
        Val::Date(t) => number(&mut out, tags::DATE, *t),
        Val::Uuid(u) => bytes(&mut out, tags::UUID, u),
        Val::Decimal(s) => bytes(&mut out, tags::DECIMAL, s.as_bytes()),
        Val::Ref { id, table } => {
            let mut r = Vec::with_capacity(24);
            bytes(&mut r, 1, id);
            number(&mut r, 2, u64::from(*table));
            bytes(&mut out, tags::REF, &r);
        }
        Val::EmptyRef { table } => {
            let mut r = Vec::with_capacity(4);
            number(&mut r, 2, u64::from(*table));
            bytes(&mut out, tags::REF, &r);
        }
        Val::Storage(b) => bytes(&mut out, tags::STORAGE, b),
        Val::Null => number(&mut out, tags::NULL, 1),
    }
    out
}

fn item(f: &Field) -> Vec<u8> {
    let mut item = Vec::with_capacity(18);
    match f {
        Field::Std(n) => number(&mut item, 2, u64::from_ne_bytes(n.to_ne_bytes())),
        Field::Item(u) => bytes(&mut item, 1, u),
    }
    item
}

pub(crate) struct Body<'a> {
    pub fields: &'a [Field],
    pub tabs: &'a [Tab],
    pub values: &'a [Val],
    pub lines: &'a [Vec<Vec<Val>>],
}

pub(crate) fn encode(h: &Header, md: &[u8; 16], change: Change, body: &Body<'_>) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    bytes(&mut out, 1, &header(h));
    let mut desc = Vec::with_capacity(18 + body.fields.len() * 20);
    bytes(&mut desc, 1, md);
    if change != Change::Deleted {
        for f in body.fields {
            bytes(&mut desc, 2, &item(f));
        }
        for t in body.tabs {
            let mut tab = Vec::with_capacity(18 + t.fields.len() * 20);
            bytes(&mut tab, 1, &t.md);
            for f in &t.fields {
                bytes(&mut tab, 2, &item(f));
            }
            bytes(&mut desc, 3, &tab);
        }
    }
    bytes(&mut out, 2, &desc);
    number(&mut out, 3, change as u64);
    number(&mut out, 4, 1);
    if change != Change::Deleted {
        for v in body.values {
            bytes(&mut out, 5, &value(v));
        }
        for lines in body.lines {
            let mut tab = Vec::new();
            for line in lines {
                let mut row = Vec::new();
                for v in line {
                    bytes(&mut row, 2, &value(v));
                }
                bytes(&mut tab, 1, &row);
            }
            bytes(&mut out, 6, &tab);
        }
    }
    out
}

pub(crate) enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

fn read_varint(b: &[u8], at: &mut usize) -> Option<u64> {
    let mut n = 0u64;
    for shift in (0..64).step_by(7) {
        let c = *b.get(*at)?;
        *at += 1;
        n |= u64::from(c & 0x7f) << shift;
        if c < 0x80 {
            return Some(n);
        }
    }
    None
}

pub(crate) fn fields_of(b: &[u8]) -> Option<Vec<(u64, Wire<'_>)>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let key = read_varint(b, &mut at)?;
        let wire = match key & 7 {
            0 => Wire::Varint(read_varint(b, &mut at)?),
            2 => {
                let n = usize::try_from(read_varint(b, &mut at)?).ok()?;
                let end = at.checked_add(n)?;
                let v = b.get(at..end)?;
                at = end;
                Wire::Bytes(v)
            }
            _ => return None,
        };
        out.push((key >> 3, wire));
    }
    Some(out)
}

fn described_item(item: &[u8]) -> Option<Field> {
    let mut field = None;
    for (k, v) in fields_of(item)? {
        match (k, v) {
            (1, Wire::Bytes(u)) if field.is_none() => field = Some(Field::Item(u.try_into().ok()?)),
            (2, Wire::Varint(x)) if field.is_none() => {
                field = Some(Field::Std(i64::from_ne_bytes(x.to_ne_bytes())));
            }
            _ => {}
        }
    }
    field
}

pub(crate) fn described(content: &[u8]) -> Option<(Vec<Field>, Vec<Tab>)> {
    let mut fields = Vec::new();
    let mut tabs = Vec::new();
    for (n, w) in fields_of(content)? {
        match (n, w) {
            (2, Wire::Bytes(item)) => fields.push(described_item(item)?),
            (3, Wire::Bytes(tab)) => {
                let mut md = None;
                let mut columns = Vec::new();
                for (k, v) in fields_of(tab)? {
                    match (k, v) {
                        (1, Wire::Bytes(u)) if md.is_none() => md = Some(u.try_into().ok()?),
                        (2, Wire::Bytes(item)) => columns.push(described_item(item)?),
                        _ => {}
                    }
                }
                tabs.push(Tab {
                    md: md?,
                    fields: columns,
                });
            }
            _ => {}
        }
    }
    Some((fields, tabs))
}
