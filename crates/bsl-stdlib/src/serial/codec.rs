use super::cycle_fatal;
use super::depth::DepthGuard;
use super::internal::text::{decode_str, encode_str};
use super::kind::{
    guid_text, is_manager, parse_guid, platform_type, storable_without_data, type_by_guid,
    type_guid,
};
use brace::Node;
use bsl_prim::{BslDate, Number, Str};
use bsl_vm::{Exception, Execution, HostObject, ObjRef, TypeId, Value};
use platform_catalog::{TypeIdx, catalog};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

pub trait ValueCodec: Send + Sync {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception>;

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception>;

    fn storable(&self) -> bool {
        true
    }
}

type Table = HashMap<u16, Arc<dyn ValueCodec>>;

fn table() -> &'static RwLock<Table> {
    static T: OnceLock<RwLock<Table>> = OnceLock::new();
    T.get_or_init(|| RwLock::new(HashMap::new()))
}

fn builtins() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let mut t = table().write().unwrap_or_else(|e| e.into_inner());
        for (idx, c) in crate::collections::codecs()
            .into_iter()
            .chain(crate::values::codecs())
            .chain(crate::ui::codecs())
            .chain(super::errinfo::codecs())
            .chain(super::storage::codecs())
            .chain(super::s4_codecs::codecs())
            .chain(super::typedesc::codecs())
        {
            t.entry(idx.0).or_insert(c);
        }
    });
}

pub fn register_codec(t: TypeIdx, c: Arc<dyn ValueCodec>) {
    builtins();
    table()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(t.0, c);
}

pub fn codec_of(t: TypeIdx) -> Option<Arc<dyn ValueCodec>> {
    builtins();
    table()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&t.0)
        .cloned()
}

const NODE_BYTES: u64 = 64;

pub struct InternalWriter {
    guard: DepthGuard,
    streams: Option<Vec<Vec<u8>>>,
    bytes: u64,
    live: u64,
    unstorable: bool,
}

impl Default for InternalWriter {
    fn default() -> Self {
        InternalWriter::new()
    }
}

impl InternalWriter {
    pub fn new() -> InternalWriter {
        InternalWriter {
            guard: DepthGuard::new(),
            streams: None,
            bytes: 0,
            live: 0,
            unstorable: false,
        }
    }

    pub fn storage() -> InternalWriter {
        InternalWriter {
            streams: Some(Vec::new()),
            ..InternalWriter::new()
        }
    }

    pub fn is_storage(&self) -> bool {
        self.streams.is_some()
    }

    pub fn unstorable(&self) -> bool {
        self.unstorable
    }

    fn reject(&mut self) {
        if self.streams.is_some() {
            self.unstorable = true;
        }
    }

    pub fn attach(&mut self, bytes: Vec<u8>) -> Option<usize> {
        let s = self.streams.as_mut()?;
        s.push(bytes);
        Some(s.len() - 1)
    }

    pub fn into_streams(self) -> Vec<Vec<u8>> {
        self.streams.unwrap_or_default()
    }

    pub fn estimate(&self) -> u64 {
        self.bytes
    }

    pub fn account(&mut self, ex: &mut Execution, text_bytes: u64) -> Result<(), Exception> {
        self.bytes = self
            .bytes
            .saturating_add(NODE_BYTES)
            .saturating_add(text_bytes);
        ex.charge(1 + text_bytes / 64)?;
        ex.reserve(self.bytes)
    }

    pub fn hold(&mut self, ex: &mut Execution, n: usize) -> Result<(), Exception> {
        self.live = self.live.saturating_add(n as u64);
        crate::collections::reserve_values(ex, self.live)
    }

    pub fn release(&mut self, n: usize) {
        self.live = self.live.saturating_sub(n as u64);
    }

    pub fn write_value(&mut self, ex: &mut Execution, v: &Value) -> Result<Node, Exception> {
        match v {
            Value::Undefined => self.tag(ex, "U"),
            Value::Null => self.tag(ex, "L"),
            Value::Bool(b) => self.pair(ex, "B", Node::int(i64::from(*b))),
            Value::Number(n) => self.number(ex, n),
            Value::String(s) => {
                self.account(ex, s.len() as u64 * 3)?;
                Ok(typed("S", encode_str(s.units())))
            }
            Value::Date(d) => self.date(ex, *d),
            Value::Type(t) => self.type_value(ex, *t),
            Value::Object(o) => self.object(ex, o),
        }
    }

    fn tag(&mut self, ex: &mut Execution, tag: &str) -> Result<Node, Exception> {
        self.account(ex, 4)?;
        Ok(Node::List(vec![Node::str(tag)]))
    }

    fn pair(&mut self, ex: &mut Execution, tag: &str, n: Node) -> Result<Node, Exception> {
        self.account(ex, 8)?;
        Ok(typed(tag, n))
    }

    fn number(&mut self, ex: &mut Execution, n: &Number) -> Result<Node, Exception> {
        let text = n.to_xml_string();
        self.account(ex, text.len() as u64 + 8)?;
        Ok(typed("N", Node::Atom(text)))
    }

    fn date(&mut self, ex: &mut Execution, d: BslDate) -> Result<Node, Exception> {
        let p = d.parts();
        let text = format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}",
            p.year, p.month, p.day, p.hour, p.minute, p.second
        );
        self.account(ex, 20)?;
        Ok(typed("D", Node::Atom(text)))
    }

    fn type_value(&mut self, ex: &mut Execution, t: TypeId) -> Result<Node, Exception> {
        match type_guid(ex, t) {
            Some(g) => {
                self.account(ex, 40)?;
                Ok(typed("T", Node::Atom(guid_text(&g))))
            }
            None => self.tag(ex, "U"),
        }
    }

    fn object(&mut self, ex: &mut Execution, o: &ObjRef) -> Result<Node, Exception> {
        let t = HostObject::type_id(&**o);
        let Some(guid) = type_guid(ex, t) else {
            self.reject();
            return self.tag(ex, "U");
        };
        if self.guard.enter().is_err() {
            return Err(cycle_fatal(ex));
        }
        let items = self.object_items(ex, o, t);
        self.guard.leave();
        let items = items?;
        self.account(ex, 48)?;
        let mut list = vec![Node::str("#"), Node::Atom(guid_text(&guid))];
        list.extend(items);
        Ok(Node::List(list))
    }

    fn object_items(
        &mut self,
        ex: &mut Execution,
        o: &ObjRef,
        t: TypeId,
    ) -> Result<Vec<Node>, Exception> {
        let Some(idx) = platform_type(ex, t) else {
            let app = ex.runtime().app().clone();
            let items = match app.serializer() {
                Some(s) => s.internal_write(ex, o)?,
                None => None,
            };
            if items.is_none() {
                self.reject();
            }
            return Ok(items.unwrap_or_default());
        };
        if let Some(c) = codec_of(idx) {
            if !c.storable() {
                self.reject();
            }
            let items = c.internal_write(ex, o, self)?;
            if items.is_none() && !storable_without_data(idx) {
                self.reject();
            }
            return Ok(items.unwrap_or_default());
        }
        Ok(if is_manager(idx) {
            vec![Node::List(vec![Node::int(0)])]
        } else {
            if !storable_without_data(idx) {
                self.reject();
            }
            Vec::new()
        })
    }
}

fn typed(tag: &str, v: Node) -> Node {
    Node::List(vec![Node::str(tag), v])
}

pub struct InternalReader {
    streams: Vec<Vec<u8>>,
    storage: bool,
    failed: bool,
    guard: DepthGuard,
}

impl Default for InternalReader {
    fn default() -> Self {
        InternalReader::new()
    }
}

impl InternalReader {
    pub fn new() -> InternalReader {
        InternalReader {
            streams: Vec::new(),
            storage: false,
            failed: false,
            guard: DepthGuard::new(),
        }
    }

    pub fn with_streams(streams: Vec<Vec<u8>>) -> InternalReader {
        InternalReader {
            streams,
            storage: true,
            failed: false,
            guard: DepthGuard::new(),
        }
    }

    pub fn is_storage(&self) -> bool {
        self.storage
    }

    pub fn stream(&self, n: usize) -> Option<&[u8]> {
        self.streams.get(n).map(Vec::as_slice)
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn format_error(&mut self, ex: &mut Execution) -> Exception {
        self.failed = true;
        ex.error(super::internal::format_host_error())
    }

    pub fn read_value(&mut self, ex: &mut Execution, n: &Node) -> Result<Value, Exception> {
        ex.charge(1)?;
        let Some(items) = n.as_list() else {
            return Err(self.format_error(ex));
        };
        let Some(tag) = items.first().and_then(Node::as_str) else {
            return Err(self.format_error(ex));
        };
        match tag {
            "U" => Ok(Value::Undefined),
            "L" => Ok(Value::Null),
            "B" => match items.get(1).and_then(Node::as_atom) {
                Some("1") => Ok(Value::Bool(true)),
                Some("0") => Ok(Value::Bool(false)),
                _ => Err(self.format_error(ex)),
            },
            "N" => match items.get(1).and_then(Node::as_atom).and_then(Number::parse) {
                Some(n) => Ok(Value::Number(n)),
                None => Err(self.format_error(ex)),
            },
            "S" => match items.get(1).and_then(decode_str) {
                Some(u) => {
                    ex.reserve(u.len() as u64 * 2)?;
                    Ok(Value::String(Str::from_vec(u)))
                }
                None => Err(self.format_error(ex)),
            },
            "D" => match items.get(1).and_then(Node::as_atom).and_then(read_date) {
                Some(d) => Ok(Value::Date(d)),
                None => Err(self.format_error(ex)),
            },
            "T" => match items.get(1).and_then(Node::as_atom).and_then(parse_guid) {
                Some(g) => Ok(Value::Type(
                    type_by_guid(ex, &g).unwrap_or(TypeId::UNDEFINED),
                )),
                None => Err(self.format_error(ex)),
            },
            "#" => self.object(ex, items),
            _ => Ok(Value::Undefined),
        }
    }

    fn object(&mut self, ex: &mut Execution, items: &[Node]) -> Result<Value, Exception> {
        let Some(g) = items.get(1).and_then(Node::as_atom).and_then(parse_guid) else {
            return Err(self.format_error(ex));
        };
        if self.guard.enter().is_err() {
            return Err(self.format_error(ex));
        }
        let r = self.object_data(ex, &g, items.get(2..).unwrap_or_default());
        self.guard.leave();
        r
    }

    fn object_data(
        &mut self,
        ex: &mut Execution,
        g: &[u8; 16],
        data: &[Node],
    ) -> Result<Value, Exception> {
        let g = *g;
        if let Some(idx) = catalog().find_type_by_guid(&g) {
            return match codec_of(idx) {
                Some(c) => c.internal_read(ex, data, self),
                None => Ok(Value::Undefined),
            };
        }
        let app = ex.runtime().app().clone();
        if let Some(s) = app.serializer()
            && let Some(v) = s.internal_read(ex, &g, data)?
        {
            return Ok(v);
        }
        Ok(Value::Undefined)
    }
}

fn read_date(a: &str) -> Option<BslDate> {
    if !matches!(a.len(), 8 | 14) || !a.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if a.bytes().all(|b| b == b'0') {
        return Some(BslDate::EMPTY);
    }
    let n = |r: std::ops::Range<usize>| a.get(r)?.parse::<u32>().ok();
    let (h, mi, s) = if a.len() == 14 {
        (n(8..10)?, n(10..12)?, n(12..14)?)
    } else {
        (0, 0, 0)
    };
    BslDate::from_parts(n(0..4)?, n(4..6)?, n(6..8)?, h, mi, s)
}
