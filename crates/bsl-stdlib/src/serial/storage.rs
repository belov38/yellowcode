use super::codec::{InternalReader, InternalWriter, ValueCodec};
use super::internal::read_text;
use crate::bind::{arg_number, arg_value, type_mismatch, value_mismatch};
use crate::collections::{method_name, not_implemented};
use crate::enums::type_by_id;
use brace::{Newline, Node, to_text_with};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallError, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef,
    TypeId, TypeImpl, Value,
};
use platform_catalog::TypeIdx;
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

const SIGNATURE: [u8; 16] = [
    0x53, 0x4B, 0x6F, 0xF4, 0x88, 0x8D, 0xC1, 0x4E, 0xA0, 0xD5, 0xEB, 0xB6, 0xBD, 0xA0, 0xA7, 0x0D,
];
const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const DEFAULT_LEVEL: u32 = 6;
const MAX_LEVEL: u32 = 9;
const STREAM_HEADER: usize = 12;

struct Types {
    storage: TypeIdx,
    deflation: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
fn types() -> &'static Types {
    static T: OnceLock<Types> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        Types {
            storage: t("ValueStorage"),
            deflation: t("Deflation"),
        }
    })
}

pub fn storable_without_data() -> &'static [&'static str] {
    super::kind::STORABLE_WITHOUT_DATA
}

struct ValueStorage {
    bytes: Arc<[u8]>,
}

impl ValueStorage {
    fn of(v: &Value) -> Option<&ValueStorage> {
        v.as_object()?.as_any().downcast_ref::<ValueStorage>()
    }
}

struct Deflation {
    level: u32,
    names: RefCell<Vec<String>>,
}

impl Deflation {
    fn of(v: &Value) -> Option<&Deflation> {
        v.as_object()?.as_any().downcast_ref::<Deflation>()
    }

    fn not_object(&self, ex: &mut Execution, m: MemberId) -> CallError {
        let name = self
            .names
            .borrow()
            .get(m.0 as usize)
            .cloned()
            .unwrap_or_default();
        CallError::Exc(ex.error(HostError::key("bsl:IDS_RTE_NOTOBJECT").with_arg(&name)))
    }
}

impl HostObject for Deflation {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().deflation)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(self.not_object(ex, m))
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(self.not_object(ex, m))
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(self.not_object(ex, m))
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let mut names = self.names.borrow_mut();
        let i = match names.iter().position(|n| n == name) {
            Some(i) => i,
            None => {
                names.push(name.to_string());
                names.len() - 1
            }
        };
        u32::try_from(i).ok().map(MemberId)
    }

    fn presentation(&self, _ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from("СжатиеДанных")))
    }
}

impl HostObject for ValueStorage {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().storage)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(types().storage, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().storage, m).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(types().storage, m) {
            Some("Get") if args.is_empty() => match read_form(ex, &self.bytes) {
                Ok(v) => Ok(v),
                Err(FormError::Format) => Err(CallError::Host(stream_format_error())),
                Err(FormError::FileOperation) => Err(crate::bind::with_other(
                    HostError::key(crate::bind::msg::FILE_OPERATION),
                    false,
                )
                .into()),
                Err(FormError::Exc(e)) => Err(CallError::Exc(e)),
            },
            _ => Err(not_implemented(types().storage, m).into()),
        }
    }

    fn presentation(&self, _ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from("ХранилищеЗначения")))
    }
}

fn stream_format_error() -> HostError {
    HostError::key("core85:IDS_LISTSTREAMERROR")
}

fn unserializable() -> HostError {
    HostError::key("core85:IDS_VS_UNSERIALIZABLE_VALUE")
}

enum BuildError {
    Unstorable,
    Exc(Exception),
}

impl From<Exception> for BuildError {
    fn from(e: Exception) -> BuildError {
        BuildError::Exc(e)
    }
}

pub fn new_value_storage(
    ex: &mut Execution,
    v: &Value,
    level: Option<u32>,
) -> Result<Value, Exception> {
    match build(ex, v, level) {
        Ok(v) => Ok(v),
        Err(BuildError::Unstorable) => Err(ex.error(unserializable())),
        Err(BuildError::Exc(e)) => Err(e),
    }
}

pub fn value_storage_bytes(v: &Value) -> Option<Arc<[u8]>> {
    ValueStorage::of(v).map(|s| s.bytes.clone())
}

pub fn value_storage_from_bytes(bytes: Arc<[u8]>) -> Value {
    Value::Object(Rc::new(ValueStorage { bytes }))
}

pub(crate) fn storage_type() -> TypeIdx {
    types().storage
}

pub(crate) fn is_storage_type(t: TypeId) -> bool {
    t == TypeId::of_catalog(types().storage)
}

fn build(ex: &mut Execution, v: &Value, level: Option<u32>) -> Result<Value, BuildError> {
    if level.is_some_and(|l| l > MAX_LEVEL) {
        return Err(BuildError::Exc(ex.error(value_mismatch(1))));
    }
    let mut w = InternalWriter::storage();
    let node = w.write_value(ex, v)?;
    if w.unstorable() {
        return Err(BuildError::Unstorable);
    }
    ex.reserve(w.estimate().saturating_mul(2))?;
    let text = to_text_with(&node, Newline::Lf);
    drop(node);
    let streams = w.into_streams();
    let bytes = form(ex, &text, &streams, level)?;
    Ok(value_storage_from_bytes(Arc::from(bytes)))
}

fn form(
    ex: &mut Execution,
    text: &str,
    streams: &[Vec<u8>],
    level: Option<u32>,
) -> Result<Vec<u8>, Exception> {
    let lfs = text.bytes().filter(|&b| b == b'\n').count();
    let text_len = (BOM.len() + text.len() + lfs) as u64;
    let streams_len = streams
        .iter()
        .fold(0u64, |a, s| a + STREAM_HEADER as u64 + s.len() as u64);
    let total = 2 + 8 + text_len + streams_len;
    ex.reserve(total)?;
    ex.charge(total / 64)?;
    let mut out = Vec::new();
    out.try_reserve_exact(total as usize)
        .map_err(|_| ex.out_of_memory())?;
    out.extend_from_slice(&[1, 1]);
    out.extend_from_slice(&text_len.to_le_bytes());
    out.extend_from_slice(&BOM);
    for b in text.bytes() {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    for (n, s) in streams.iter().enumerate() {
        out.extend_from_slice(&(n as u32).to_le_bytes());
        out.extend_from_slice(&(s.len() as u64).to_le_bytes());
        out.extend_from_slice(s);
    }
    let Some(level) = level else {
        return Ok(out);
    };
    ex.reserve(total.saturating_add(total / 8).saturating_add(64))?;
    let body = out.get(2..).unwrap_or_default();
    let packed = zraw::deflate(body, level).map_err(|_| ex.out_of_memory())?;
    let mut z = Vec::with_capacity(2 + SIGNATURE.len() + packed.len());
    z.extend_from_slice(&[2, 1]);
    z.extend_from_slice(&SIGNATURE);
    z.extend_from_slice(&packed);
    Ok(z)
}

#[derive(Debug)]
enum FormError {
    Format,
    FileOperation,
    Exc(Exception),
}

impl From<Exception> for FormError {
    fn from(e: Exception) -> FormError {
        FormError::Exc(e)
    }
}

fn read_form(ex: &mut Execution, bytes: &[u8]) -> Result<Value, FormError> {
    let Some(&kind) = bytes.first() else {
        return Ok(Value::Undefined);
    };
    if bytes.get(1) != Some(&1) {
        return Err(FormError::Format);
    }
    match kind {
        1 => read_body(ex, bytes.get(2..).unwrap_or_default()),
        2 => {
            let rest = bytes.get(2..).unwrap_or_default();
            let Some(packed) = rest.strip_prefix(&SIGNATURE) else {
                return Err(FormError::Format);
            };
            ex.charge(packed.len() as u64 / 64)?;
            let limit = ex.options().memory_limit.map_or(zraw::DEFAULT_LIMIT, |l| {
                l.saturating_sub(bytes.len() as u64)
            });
            match zraw::inflate_prefix(packed, limit) {
                Ok(body) => {
                    ex.charge(body.len() as u64 / 64)?;
                    read_body(ex, &body)
                }
                Err(zraw::Error::TooLarge(_) | zraw::Error::Zlib(_)) => {
                    Err(FormError::Exc(ex.out_of_memory()))
                }
                Err(zraw::Error::Corrupt(message))
                    if message.contains("invalid stored block lengths") =>
                {
                    Err(FormError::FileOperation)
                }
                Err(_) => Err(FormError::Format),
            }
        }
        _ => Err(FormError::Format),
    }
}

fn read_body(ex: &mut Execution, body: &[u8]) -> Result<Value, FormError> {
    let Some(head) = body.get(..8) else {
        return Err(FormError::Format);
    };
    let n = u64::from_le_bytes(head.try_into().map_err(|_| FormError::Format)?);
    if n > i64::MAX as u64 {
        return Err(FormError::Format);
    }
    let rest = body.get(8..).unwrap_or_default();
    let take = usize::try_from(n).map_or(rest.len(), |n| n.min(rest.len()));
    let (text, tail) = rest.split_at(take);
    let streams = read_streams(ex, tail)?;
    let text = text.strip_prefix(&BOM).unwrap_or(text);
    let text = match std::str::from_utf8(text) {
        Ok(text) => text,
        Err(error) => {
            std::str::from_utf8(&text[..error.valid_up_to()]).map_err(|_| FormError::Format)?
        }
    };
    ex.reserve((text.len() as u64).saturating_mul(3))?;
    ex.charge(text.len() as u64 / 64)?;
    let units: Vec<u16> = text.replace("\r\n", "\n").encode_utf16().collect();
    let mut r = InternalReader::with_streams(streams);
    match read_text(ex, &units, &mut r) {
        Ok(v) => Ok(v),
        Err(_) if r.failed() => Err(FormError::Format),
        Err(e) => Err(FormError::Exc(e)),
    }
}

fn read_streams(ex: &mut Execution, mut tail: &[u8]) -> Result<Vec<Vec<u8>>, FormError> {
    let mut streams: Vec<Vec<u8>> = Vec::new();
    while tail.len() >= STREAM_HEADER {
        let (head, rest) = tail.split_at(STREAM_HEADER);
        let (num, len) = head.split_at(4);
        let num = u32::from_le_bytes(num.try_into().map_err(|_| FormError::Format)?);
        let len = u64::from_le_bytes(len.try_into().map_err(|_| FormError::Format)?);
        if len > i64::MAX as u64 {
            return Err(FormError::Format);
        }
        if (num as u64) < streams.len() as u64 {
            break;
        }
        if num as u64 != streams.len() as u64 {
            if len > rest.len() as u64 {
                break;
            }
            return Err(FormError::Format);
        }
        let take = usize::try_from(len).unwrap_or(usize::MAX).min(rest.len());
        let (data, after) = rest.split_at(take);
        ex.reserve(take as u64)?;
        ex.charge(1 + take as u64 / 64)?;
        streams.push(data.to_vec());
        tail = after;
    }
    Ok(streams)
}

struct StorageCodec;

impl ValueCodec for StorageCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(s) = v.as_any().downcast_ref::<ValueStorage>() else {
            return Ok(None);
        };
        let data = if w.is_storage() {
            ex.reserve(s.bytes.len() as u64)?;
            let n = w.attach(s.bytes.to_vec());
            Node::int(n.map_or(0, |n| n as i64))
        } else {
            let text = (s.bytes.len() as u64).div_ceil(3).saturating_mul(4);
            ex.reserve(text.saturating_mul(2))?;
            w.account(ex, text + text / 32)?;
            brace::base64_atom(&s.bytes, Newline::Lf, true)
        };
        Ok(Some(vec![Node::List(vec![data])]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let atom = items
            .first()
            .and_then(Node::as_list)
            .and_then(|l| l.first())
            .and_then(Node::as_atom);
        let Some(atom) = atom else {
            return Err(r.format_error(ex));
        };
        if atom.starts_with("#base64:") {
            ex.reserve((atom.len() as u64).saturating_mul(2))?;
            return match Node::atom(atom).base64() {
                Some(b) => Ok(value_storage_from_bytes(Arc::from(b))),
                None => Err(r.format_error(ex)),
            };
        }
        match atom.parse::<usize>() {
            Ok(n) => match r.stream(n) {
                Some(b) => {
                    ex.reserve(b.len() as u64)?;
                    Ok(value_storage_from_bytes(Arc::from(b)))
                }
                None if r.is_storage() => Ok(value_storage_from_bytes(Arc::from(Vec::new()))),
                None => Err(r.format_error(ex)),
            },
            Err(_) => Err(r.format_error(ex)),
        }
    }
}

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    vec![(
        types().storage,
        Arc::new(StorageCodec) as Arc<dyn ValueCodec>,
    )]
}

fn storage_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.is_empty() {
        return Err(CallError::Exc(
            ex.error(HostError::key("bsl:IDS_RTE_CTORNOTFOUND")),
        ));
    }
    let v = arg_value(ex, args.first())?;
    let level = match arg_value(ex, args.get(1))? {
        Value::Undefined => None,
        other => match Deflation::of(&other) {
            Some(d) => Some(d.level),
            None => return Err(type_mismatch(2).into()),
        },
    };
    match build(ex, &v, level) {
        Ok(v) => Ok(v),
        Err(BuildError::Unstorable) => Err(unserializable().into()),
        Err(BuildError::Exc(e)) => Err(CallError::Exc(e)),
    }
}

fn deflation_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let level = match args.first() {
        None | Some(Arg::Omitted) => DEFAULT_LEVEL,
        Some(a) => {
            let n = arg_number(ex, Some(a), 1)?.trunc_to_i64_saturating();
            match u32::try_from(n) {
                Ok(l) if l <= MAX_LEVEL => l,
                _ => return Err(value_mismatch(1).into()),
            }
        }
    };
    Ok(Value::Object(Rc::new(Deflation {
        level,
        names: RefCell::new(Vec::new()),
    })))
}

pub(crate) fn register(reg: &mut bsl_vm::Registry) {
    reg.type_impl(
        types().storage,
        TypeImpl {
            ctor: Some(Arc::new(storage_ctor)),
        },
    );
    reg.type_impl(
        types().deflation,
        TypeImpl {
            ctor: Some(Arc::new(deflation_ctor)),
        },
    );
}
