use super::buffer::BinBuf;
use super::{MAX_BYTES, guard_bytes};
use crate::bind::{
    arg_i64, arg_value, is_given, msg, strict_count, strict_i64, type_mismatch, with_other,
};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::enums::{as_enum_value, type_by_id};
use crate::fs::{fs, fs_error};
use crate::globals::enum_arg;
use crate::values::new_binary_data;
use bsl_vm::{
    AppHost, Arg, CallError, CallResult, Execution, FsError, FsErrorKind, HostError, HostObject,
    MemberId, Registry, TypeId, TypeIdx, TypeImpl, Value,
};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

pub(crate) type SharedStream = Rc<RefCell<dyn ByteStream>>;

const CHUNK: usize = 64 << 10;

pub(crate) enum StreamError {
    Closed,
    CantRead,
    CantWrite,
    CantSeek,
    Overflow,
    Memory,
    Fs(FsError),
}

impl StreamError {
    pub(crate) fn into_call(self, ex: &Execution) -> CallError {
        let key = match self {
            StreamError::Closed => msg::STREAM_CLOSED,
            StreamError::CantRead => msg::CANT_READ,
            StreamError::CantWrite => msg::CANT_WRITE,
            StreamError::CantSeek => msg::CANT_SEEK,
            StreamError::Overflow => msg::BUFFER_OVERFLOW,
            StreamError::Memory => return ex.out_of_memory().into(),
            StreamError::Fs(e) => return fs_error(e).into(),
        };
        with_other(HostError::key(key), false).into()
    }
}

type SResult<T> = Result<T, StreamError>;

pub(crate) trait ByteStream {
    fn read(&mut self, out: &mut [u8]) -> SResult<usize>;
    fn write(&mut self, data: &[u8]) -> SResult<()>;
    fn position(&self) -> u64;
    fn seek(&mut self, pos: u64) -> SResult<()>;
    fn len(&self) -> SResult<u64>;
    fn set_len(&mut self, n: u64) -> SResult<()>;
    fn can_read(&self) -> bool;
    fn can_write(&self) -> bool;
    fn can_seek(&self) -> bool;
    fn check_write(&self, len_after: u64) -> SResult<()>;
    fn flush(&mut self) -> SResult<()>;
    fn close(&mut self) -> SResult<()>;
    fn closed(&self) -> bool;
    fn contents(&self) -> SResult<Arc<[u8]>>;
    fn take(&mut self) -> SResult<Vec<u8>>;
    fn file_name(&self) -> Option<String> {
        None
    }
}

enum Store {
    Own(Vec<u8>),
    Fixed {
        store: Rc<RefCell<Vec<u8>>>,
        off: usize,
        len: usize,
        ro: bool,
    },
    Frozen(Arc<[u8]>),
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

impl Store {
    fn len(&self) -> usize {
        match self {
            Store::Own(v) => v.len(),
            Store::Fixed { len, .. } => *len,
            Store::Frozen(b) => b.len(),
        }
    }

    fn read_at(&self, pos: usize, out: &mut [u8]) -> usize {
        let n = self.len().saturating_sub(pos).min(out.len());
        let mut copy = |src: &[u8]| {
            if let (Some(s), Some(d)) = (src.get(pos..pos + n), out.get_mut(..n)) {
                d.copy_from_slice(s);
            }
        };
        match self {
            Store::Own(v) => copy(v),
            Store::Fixed { store, off, .. } => {
                if let Some(w) = store.borrow().get(*off..*off + self.len()) {
                    copy(w);
                }
            }
            Store::Frozen(b) => copy(b),
        }
        n
    }

    fn to_vec(&self) -> Vec<u8> {
        let mut v = vec![0u8; self.len()];
        self.read_at(0, &mut v);
        v
    }

    fn write_at(&mut self, pos: usize, data: &[u8]) -> SResult<()> {
        let end = pos.checked_add(data.len()).ok_or(StreamError::Memory)?;
        match self {
            Store::Own(v) => {
                if end as u64 > MAX_BYTES {
                    return Err(StreamError::Memory);
                }
                if end > v.len() {
                    v.try_reserve(end - v.len())
                        .map_err(|_| StreamError::Memory)?;
                    v.resize(end, 0);
                }
                if let Some(d) = v.get_mut(pos..end) {
                    d.copy_from_slice(data);
                }
                Ok(())
            }
            Store::Fixed {
                store, off, len, ..
            } => {
                if end > *len {
                    return Err(StreamError::Overflow);
                }
                if let Some(d) = store.borrow_mut().get_mut(*off + pos..*off + end) {
                    d.copy_from_slice(data);
                }
                Ok(())
            }
            Store::Frozen(_) => Err(StreamError::CantWrite),
        }
    }

    fn resize(&mut self, n: usize) -> SResult<()> {
        match self {
            Store::Own(v) => {
                if n as u64 > MAX_BYTES {
                    return Err(StreamError::Memory);
                }
                if n > v.len() {
                    v.try_reserve(n - v.len())
                        .map_err(|_| StreamError::Memory)?;
                }
                v.resize(n, 0);
                Ok(())
            }
            Store::Fixed { len, .. } if n == *len => Ok(()),
            Store::Fixed { .. } => Err(StreamError::Overflow),
            Store::Frozen(_) => Err(StreamError::CantWrite),
        }
    }
}

struct MemStream {
    store: Store,
    pos: u64,
    closed: bool,
}

impl MemStream {
    fn open(&self) -> SResult<()> {
        if self.closed {
            Err(StreamError::Closed)
        } else {
            Ok(())
        }
    }

    fn writable(&self) -> bool {
        match &self.store {
            Store::Own(_) => true,
            Store::Fixed { ro, .. } => !ro,
            Store::Frozen(_) => false,
        }
    }
}

impl ByteStream for MemStream {
    fn read(&mut self, out: &mut [u8]) -> SResult<usize> {
        self.open()?;
        let n = self.store.read_at(to_usize(self.pos), out);
        self.pos += n as u64;
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> SResult<()> {
        self.open()?;
        if !self.writable() {
            return Err(StreamError::CantWrite);
        }
        if data.is_empty() {
            return Ok(());
        }
        self.store.write_at(to_usize(self.pos), data)?;
        self.pos += data.len() as u64;
        Ok(())
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn seek(&mut self, pos: u64) -> SResult<()> {
        self.open()?;
        self.pos = pos;
        Ok(())
    }

    fn len(&self) -> SResult<u64> {
        self.open()?;
        Ok(self.store.len() as u64)
    }

    fn set_len(&mut self, n: u64) -> SResult<()> {
        self.open()?;
        if !self.writable() {
            return Err(StreamError::CantWrite);
        }
        if n > MAX_BYTES {
            return Err(StreamError::Memory);
        }
        self.store.resize(to_usize(n))
    }

    fn can_read(&self) -> bool {
        true
    }

    fn can_write(&self) -> bool {
        self.writable()
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn check_write(&self, len_after: u64) -> SResult<()> {
        self.open()?;
        if !self.writable() {
            return Err(StreamError::CantWrite);
        }
        match &self.store {
            Store::Fixed { len, .. } if len_after > *len as u64 => Err(StreamError::Overflow),
            _ => Ok(()),
        }
    }

    fn flush(&mut self) -> SResult<()> {
        self.open()
    }

    fn close(&mut self) -> SResult<()> {
        self.closed = true;
        Ok(())
    }

    fn closed(&self) -> bool {
        self.closed
    }

    fn contents(&self) -> SResult<Arc<[u8]>> {
        self.open()?;
        Ok(match &self.store {
            Store::Frozen(b) => b.clone(),
            s => Arc::from(s.to_vec()),
        })
    }

    fn take(&mut self) -> SResult<Vec<u8>> {
        self.open()?;
        self.closed = true;
        Ok(match &mut self.store {
            Store::Own(v) => std::mem::take(v),
            s => s.to_vec(),
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FileAccessMode {
    pub read: bool,
    pub write: bool,
}

struct FileStream {
    mem: MemStream,
    path: String,
    app: Arc<dyn AppHost>,
    access: FileAccessMode,
    dirty: bool,
}

impl FileStream {
    fn write_out(&mut self) -> SResult<()> {
        if !self.dirty {
            return Ok(());
        }
        let Some(files) = self.app.file_system() else {
            return Err(StreamError::Fs(denied(&self.path)));
        };
        let data = self.mem.store.to_vec();
        files
            .write(&self.path, &data, false)
            .map_err(StreamError::Fs)?;
        self.dirty = false;
        Ok(())
    }
}

impl Drop for FileStream {
    fn drop(&mut self) {
        if !self.mem.closed {
            let _ = self.write_out();
        }
    }
}

fn denied(path: &str) -> FsError {
    FsError {
        kind: FsErrorKind::AccessDenied,
        path: path.to_owned(),
    }
}

impl ByteStream for FileStream {
    fn read(&mut self, out: &mut [u8]) -> SResult<usize> {
        self.mem.open()?;
        if !self.access.read {
            return Err(StreamError::CantRead);
        }
        self.mem.read(out)
    }

    fn write(&mut self, data: &[u8]) -> SResult<()> {
        self.mem.open()?;
        if !self.access.write {
            return Err(StreamError::CantWrite);
        }
        self.mem.write(data)?;
        self.dirty |= !data.is_empty();
        Ok(())
    }

    fn position(&self) -> u64 {
        self.mem.position()
    }

    fn seek(&mut self, pos: u64) -> SResult<()> {
        self.mem.seek(pos)
    }

    fn len(&self) -> SResult<u64> {
        self.mem.len()
    }

    fn set_len(&mut self, n: u64) -> SResult<()> {
        self.mem.open()?;
        if !self.access.write {
            return Err(StreamError::CantWrite);
        }
        self.mem.set_len(n)?;
        self.dirty = true;
        Ok(())
    }

    fn can_read(&self) -> bool {
        self.access.read
    }

    fn can_write(&self) -> bool {
        self.access.write
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn check_write(&self, len_after: u64) -> SResult<()> {
        self.mem.open()?;
        if !self.access.write {
            return Err(StreamError::CantWrite);
        }
        self.mem.check_write(len_after)
    }

    fn flush(&mut self) -> SResult<()> {
        self.mem.open()?;
        self.write_out()
    }

    fn close(&mut self) -> SResult<()> {
        if self.mem.closed {
            return Ok(());
        }
        let r = self.write_out();
        self.mem.closed = true;
        r
    }

    fn closed(&self) -> bool {
        self.mem.closed
    }

    fn contents(&self) -> SResult<Arc<[u8]>> {
        self.mem.contents()
    }

    fn take(&mut self) -> SResult<Vec<u8>> {
        let r = self.write_out();
        let v = self.mem.take();
        r?;
        v
    }

    fn file_name(&self) -> Option<String> {
        Some(self.path.clone())
    }
}

pub(crate) fn stream_of(v: &Value) -> Option<SharedStream> {
    Some(
        v.as_object()?
            .as_any()
            .downcast_ref::<StreamObj>()?
            .s
            .clone(),
    )
}

pub fn stream_bytes(v: &Value) -> Option<Vec<u8>> {
    let s = stream_of(v)?;
    let c = s.borrow().contents().ok()?;
    Some(c.to_vec())
}

pub(crate) fn new_read_only_stream(bytes: Arc<[u8]>) -> Value {
    new_stream(frozen(bytes), stream_types().stream)
}

fn frozen(bytes: Arc<[u8]>) -> SharedStream {
    Rc::new(RefCell::new(MemStream {
        store: Store::Frozen(bytes),
        pos: 0,
        closed: false,
    }))
}

fn new_stream(s: SharedStream, ty: TypeIdx) -> Value {
    Value::Object(Rc::new(StreamObj { s, ty }))
}

pub(crate) fn write_stream(
    ex: &mut Execution,
    s: &RefCell<dyn ByteStream>,
    data: &[u8],
) -> CallResult<()> {
    let (end, len) = {
        let b = s.borrow();
        let len = b.len().map_err(|e| e.into_call(ex))?;
        let end = b.position().saturating_add(data.len() as u64);
        b.check_write(end).map_err(|e| e.into_call(ex))?;
        (end, len)
    };
    if end > len {
        if end > MAX_BYTES {
            return Err(ex.out_of_memory().into());
        }
        ex.reserve(end)?;
    }
    ex.charge(data.len() as u64 / 64)?;
    s.borrow_mut().write(data).map_err(|e| e.into_call(ex))
}

pub(crate) fn read_stream(
    ex: &mut Execution,
    s: &RefCell<dyn ByteStream>,
    max: u64,
) -> CallResult<Vec<u8>> {
    let n = available(ex, s)?.min(max);
    ex.charge(n / 64)?;
    ex.reserve(n)?;
    let mut out = Vec::new();
    out.try_reserve_exact(to_usize(n))
        .map_err(|_| ex.out_of_memory())?;
    out.resize(to_usize(n), 0);
    let mut b = s.borrow_mut();
    if !b.can_read() {
        return Err(StreamError::CantRead.into_call(ex));
    }
    let got = b.read(&mut out).map_err(|e| e.into_call(ex))?;
    out.truncate(got);
    Ok(out)
}

pub(crate) fn available(ex: &Execution, s: &RefCell<dyn ByteStream>) -> CallResult<u64> {
    let b = s.borrow();
    let len = b.len().map_err(|e| e.into_call(ex))?;
    Ok(len.saturating_sub(b.position()))
}

pub(crate) fn seek_to(ex: &Execution, s: &RefCell<dyn ByteStream>, pos: u64) -> CallResult<()> {
    s.borrow_mut().seek(pos).map_err(|e| e.into_call(ex))
}

pub(crate) fn copy_stream(
    ex: &mut Execution,
    src: &RefCell<dyn ByteStream>,
    dst: &RefCell<dyn ByteStream>,
    count: Option<u64>,
) -> CallResult<u64> {
    let total = available(ex, src)?.min(count.unwrap_or(u64::MAX));
    let mut done = 0u64;
    while done < total {
        let chunk = read_stream(ex, src, (total - done).min(CHUNK as u64))?;
        if chunk.is_empty() {
            break;
        }
        write_stream(ex, dst, &chunk)?;
        done += chunk.len() as u64;
    }
    Ok(done)
}

fn range() -> HostError {
    HostError::key(msg::RANGE)
}

pub(crate) struct StreamTypes {
    memory: TypeIdx,
    file: TypeIdx,
    stream: TypeIdx,
    pub(crate) writer: TypeIdx,
    pub(crate) reader: TypeIdx,
    pub(crate) result: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
pub(crate) fn stream_types() -> &'static StreamTypes {
    static T: OnceLock<StreamTypes> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        StreamTypes {
            memory: t("MemoryStream"),
            file: t("FileStream"),
            stream: t("Stream"),
            writer: t("DataWriter"),
            reader: t("DataReader"),
            result: t("ReadDataResult"),
        }
    })
}

pub(crate) fn register(reg: &mut Registry) {
    let t = stream_types();
    reg.type_impl(
        t.memory,
        TypeImpl {
            ctor: Some(Arc::new(memory_ctor)),
        },
    );
    reg.type_impl(
        t.file,
        TypeImpl {
            ctor: Some(Arc::new(file_ctor)),
        },
    );
}

fn memory_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let store = match args.len() {
        0 => Store::Own(Vec::new()),
        1 => {
            let v = arg_value(ex, args.first())?;
            if let Some(b) = BinBuf::of(&v) {
                let (store, off, len, ro) = b.window();
                Store::Fixed {
                    store,
                    off,
                    len,
                    ro,
                }
            } else if let Value::Number(n) = &v {
                let Ok(cap) = u64::try_from(n.trunc_to_i64_saturating()) else {
                    return Err(HostError::key(msg::NEGATIVE_SIZE).into());
                };
                guard_bytes(ex, cap)?;
                Store::Own(Vec::new())
            } else {
                return Err(type_mismatch(1).into());
            }
        }
        _ => return Err(ctor_not_implemented(stream_types().memory).into()),
    };
    let s: SharedStream = Rc::new(RefCell::new(MemStream {
        store,
        pos: 0,
        closed: false,
    }));
    Ok(new_stream(s, stream_types().memory))
}

fn file_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.len() < 2 || args.len() > 4 {
        return Err(ctor_not_implemented(stream_types().file).into());
    }
    let path = crate::bind::arg_str(ex, args.first())?.to_string_lossy();
    let Some(mode) = enum_arg(ex, args.get(1), "РежимОткрытияФайла", 2)? else {
        return Err(type_mismatch(2).into());
    };
    let third = arg_value(ex, args.get(2))?;
    let access = if as_enum_value(&third).is_some() {
        enum_arg(ex, args.get(2), "ДоступКФайлу", 3)?
    } else {
        if args.len() == 4 {
            return Err(type_mismatch(3).into());
        }
        if is_given(args.get(2)) {
            arg_i64(ex, args.get(2), 3)?;
        }
        None
    };
    if args.len() == 4 && is_given(args.get(3)) {
        arg_i64(ex, args.get(3), 4)?;
    }
    open_file(ex, &path, mode, access)
}

pub(crate) fn open_file(
    ex: &mut Execution,
    path: &str,
    mode: &str,
    access: Option<&str>,
) -> CallResult<Value> {
    let f = fs(ex, path)?;
    let app = ex.runtime().app().clone();
    let files = f.get();
    let exists = files
        .stat(path)
        .map_err(fs_error)?
        .is_some_and(|s| !s.is_dir);
    let missing_or = |e: FsError| {
        if e.kind == FsErrorKind::NotFound {
            crate::fs::stream_not_found(path)
        } else {
            fs_error(e)
        }
    };
    let create = matches!(mode, "Создать" | "СоздатьНовый" | "Обрезать");
    if mode == "СоздатьНовый" && exists {
        return Err(fs_error(denied(path)).into());
    }
    let data = if create || (!exists && matches!(mode, "ОткрытьИлиСоздать" | "Дописать"))
    {
        if mode == "Обрезать" && !exists {
            files.read(path).map_err(missing_or)?;
        }
        files.write(path, &[], false).map_err(fs_error)?;
        Vec::new()
    } else {
        if let Some(st) = files.stat(path).map_err(fs_error)? {
            guard_bytes(ex, st.size)?;
        }
        files.read(path).map_err(missing_or)?
    };
    let access = match access {
        Some("Чтение") => FileAccessMode {
            read: true,
            write: false,
        },
        Some("Запись") => FileAccessMode {
            read: false,
            write: true,
        },
        Some(_) => FileAccessMode {
            read: true,
            write: true,
        },
        None => FileAccessMode {
            read: mode != "Дописать",
            write: true,
        },
    };
    let pos = if mode == "Дописать" {
        data.len() as u64
    } else {
        0
    };
    let s: SharedStream = Rc::new(RefCell::new(FileStream {
        mem: MemStream {
            store: Store::Own(data),
            pos,
            closed: false,
        },
        path: path.to_owned(),
        app,
        access,
        dirty: false,
    }));
    Ok(new_stream(s, stream_types().file))
}

struct StreamObj {
    s: SharedStream,
    ty: TypeIdx,
}

impl StreamObj {
    fn open(&self, ex: &Execution) -> CallResult<()> {
        if self.s.borrow().closed() {
            return Err(StreamError::Closed.into_call(ex));
        }
        Ok(())
    }

    fn number(n: u64) -> Value {
        Value::from(i64::try_from(n).unwrap_or(i64::MAX))
    }

    fn seek(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let off = strict_i64(ex, args.first(), 1, true)?;
        let from = enum_arg(ex, args.get(1), "ПозицияВПотоке", 2)?.unwrap_or("Начало");
        let (pos, len) = {
            let b = self.s.borrow();
            let len = b.len().map_err(|e| e.into_call(ex))?;
            if !b.can_seek() {
                return Err(StreamError::CantSeek.into_call(ex));
            }
            (b.position(), len)
        };
        let base = match from {
            "Конец" => i64::try_from(len).ok(),
            "Текущая" => i64::try_from(pos).ok(),
            _ => Some(0),
        };
        if let Some(t) = base
            .and_then(|b| b.checked_add(off))
            .and_then(|t| u64::try_from(t).ok())
        {
            seek_to(ex, &self.s, t)?;
            return Ok(Self::number(t));
        }
        Ok(Self::number(pos))
    }

    fn read(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let Some(buf) = BinBuf::of(&v) else {
            return Err(with_other(type_mismatch(1), true).into());
        };
        let pos = strict_i64(ex, args.get(1), 2, true)?;
        let count = strict_count(ex, args.get(2), 3, true)?;
        self.open(ex)?;
        buf.stream_span(pos, count, true)?;
        let data = read_stream(ex, &self.s, count)?;
        buf.put_range(pos, &data)?;
        Ok(Self::number(data.len() as u64))
    }

    fn write(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let Some(buf) = BinBuf::of(&v) else {
            return Err(with_other(type_mismatch(1), true).into());
        };
        let pos = strict_i64(ex, args.get(1), 2, true)?;
        let count = strict_count(ex, args.get(2), 3, true)?;
        self.open(ex)?;
        buf.stream_span(pos, count, false)?;
        let data = buf.get_range(pos, count as i64)?;
        write_stream(ex, &self.s, &data)?;
        Ok(Value::Undefined)
    }

    fn set_size(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let n = strict_i64(ex, args.first(), 1, true)?;
        self.open(ex)?;
        let Ok(n) = u64::try_from(n) else {
            return Err(range().into());
        };
        {
            let b = self.s.borrow();
            b.check_write(n).map_err(|e| e.into_call(ex))?;
        }
        guard_bytes(ex, n)?;
        self.s
            .borrow_mut()
            .set_len(n)
            .map_err(|e| e.into_call(ex))?;
        Ok(Value::Undefined)
    }

    fn copy_to(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        let Some(dst) = stream_of(&v) else {
            return Err(type_mismatch(1).into());
        };
        let count = if is_given(args.get(1)) {
            let n = strict_i64(ex, args.get(1), 2, true)?;
            Some(u64::try_from(n).map_err(|_| range())?)
        } else {
            None
        };
        self.open(ex)?;
        let n = copy_stream(ex, &self.s, &dst, count)?;
        Ok(Self::number(n))
    }

    fn readonly(&self, ex: &mut Execution) -> CallResult<Value> {
        self.open(ex)?;
        let n = self.s.borrow().len().map_err(|e| e.into_call(ex))?;
        guard_bytes(ex, n)?;
        let c = self.s.borrow().contents().map_err(|e| e.into_call(ex))?;
        Ok(new_read_only_stream(c))
    }
}

impl HostObject for StreamObj {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.open(ex)?;
        let b = self.s.borrow();
        match property_name(self.ty, m) {
            Some("CanWrite") => Ok(Value::Bool(b.can_write())),
            Some("CanRead") => Ok(Value::Bool(b.can_read())),
            Some("CanSeek") => Ok(Value::Bool(b.can_seek())),
            Some("FileName") => Ok(Value::from(b.file_name().unwrap_or_default().as_str())),
            _ => Err(not_implemented(self.ty, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        match property_name(self.ty, m) {
            Some(_) => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(property_ru(self.ty, m))
                .raw()
                .into()),
            None => Err(not_implemented(self.ty, m).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let args: &[Arg] = args;
        match method_name(self.ty, m) {
            Some("Close") => {
                self.s.borrow_mut().close().map_err(|e| e.into_call(ex))?;
                Ok(Value::Undefined)
            }
            Some("Size") => {
                let n = self.s.borrow().len().map_err(|e| e.into_call(ex))?;
                Ok(Self::number(n))
            }
            Some("CurrentPosition") => {
                self.open(ex)?;
                let n = self.s.borrow().position();
                Ok(Self::number(n))
            }
            Some("Seek") => self.seek(ex, args),
            Some("Read") => self.read(ex, args),
            Some("Write") => self.write(ex, args),
            Some("SetSize") => self.set_size(ex, args),
            Some("CopyTo") => self.copy_to(ex, args),
            Some("GetReadonlyStream") => self.readonly(ex),
            Some("Flush") => {
                self.s.borrow_mut().flush().map_err(|e| e.into_call(ex))?;
                Ok(Value::Undefined)
            }
            Some("CloseAndGetBinaryData") if self.ty == stream_types().memory => {
                self.open(ex)?;
                let n = self.s.borrow().len().map_err(|e| e.into_call(ex))?;
                guard_bytes(ex, n)?;
                let v = self.s.borrow_mut().take().map_err(|e| e.into_call(ex))?;
                Ok(new_binary_data(v))
            }
            _ => Err(not_implemented(self.ty, m).into()),
        }
    }
}
