use crate::Catalog;
use crate::model::*;
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use std::io::{Read, Write};

const MAGIC: &[u8; 8] = b"BSLCAT\x04\x00";
pub(crate) const MAX_PLATFORM: usize = 64;
pub(crate) const MAX_INFLATED: u64 = 64 << 20;

#[derive(Default)]
struct W(Vec<u8>, Option<u64>, bool);

impl W {
    fn bytes(&mut self, bytes: &[u8]) {
        if self.1.is_none() {
            self.0.extend_from_slice(bytes);
        }
    }
    fn heap(&mut self, bytes: usize) {
        if let Some(total) = self.1 {
            match u64::try_from(bytes)
                .ok()
                .and_then(|bytes| total.checked_add(bytes))
            {
                Some(total) => self.1 = Some(total),
                None => self.2 = true,
            }
        }
    }
    fn u8(&mut self, v: u8) {
        self.bytes(&[v]);
    }
    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }
    fn u16(&mut self, v: u16) {
        self.bytes(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.bytes(&(n as u32).to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.heap(s.len());
        self.len(s.len());
        self.bytes(s.as_bytes());
    }
    fn opt<T>(&mut self, v: &Option<T>, f: impl FnOnce(&mut W, &T)) {
        match v {
            Some(x) => {
                self.u8(1);
                f(self, x);
            }
            None => self.u8(0),
        }
    }
    fn vec<T>(&mut self, v: &[T], f: impl Fn(&mut W, &T)) {
        match v.len().checked_mul(std::mem::size_of::<T>()) {
            Some(bytes) => self.heap(bytes),
            None => self.2 = true,
        }
        self.len(v.len());
        for x in v {
            f(self, x);
        }
    }
}

fn w_avail(w: &mut W, a: &Avail) {
    w.u16(a.0);
}
fn w_names(w: &mut W, ru: &str, en: &str) {
    w.str(ru);
    w.str(en);
}
fn w_param(w: &mut W, p: &ParamInfo) {
    w_names(w, &p.name_ru, &p.name_en);
    w.bool(p.optional);
    w.bool(p.variadic);
}
fn w_result(w: &mut W, t: &ResultType) {
    w.opt(&t.id, |w, s| w.str(s));
    w.str(&t.name_ru);
    w.opt(&t.name_en, |w, s| w.str(s));
}
fn w_sig(w: &mut W, s: &Signature) {
    w.vec(&s.params, w_param);
    w.opt(&s.return_types, |w, ts| w.vec(ts, w_result));
}
fn w_prop(w: &mut W, p: &PropertyInfo) {
    w_names(w, &p.name_ru, &p.name_en);
    w.bool(p.readable);
    w.bool(p.writable);
    w_avail(w, &p.avail);
    w.vec(&p.types, w_result);
}
fn w_method(w: &mut W, m: &MethodInfo) {
    w_names(w, &m.name_ru, &m.name_en);
    w.vec(&m.variants, w_sig);
    w.bool(m.returns_value);
    w_avail(w, &m.avail);
}
fn w_type(w: &mut W, t: &TypeInfo) {
    w.str(&t.id);
    w_names(w, &t.name_ru, &t.name_en);
    w.u8(match t.kind {
        TypeKind::Primitive => 0,
        TypeKind::Class => 1,
        TypeKind::Enum => 2,
    });
    w_avail(w, &t.avail);
    w.bool(t.metadata_template);
    w.opt(&t.collection, |w, c| {
        w.bool(c.for_each);
        w.bool(c.index_access);
    });
    w.bool(t.serializable);
    w.bool(t.server_exchange);
    w.opt(&t.xdto, |w, x| w_names(w, &x.namespace, &x.name));
    w.opt(&t.guid, |w, g| w.bytes(g));
    w.opt(&t.since, |w, s| w.str(s));
    w.opt(&t.compat_mode, |w, s| w.str(s));
    w.opt(&t.presentation_ru, |w, s| w.str(s));
    w.opt(&t.presentation_en, |w, s| w.str(s));
    w.opt(&t.new_no_args, |w, b| w.bool(*b));
    w.opt(&t.type_fn, |w, b| w.bool(*b));
    w.opt(&t.type_fn_en, |w, b| w.bool(*b));
    w.opt(&t.new_error, |w, e| {
        w.u8(match e {
            NewError::CtorNotFound => 0,
            NewError::NotObject => 1,
            NewError::CtorFails => 2,
        })
    });
    w.opt(&t.value_type_presentation_ru, |w, s| w.str(s));
    w.opt(&t.value_type_presentation_en, |w, s| w.str(s));
    w.bool(t.enum_global);
    w.opt(&t.server_compile, |w, s| {
        w.bool(s.type_known);
        w.bool(s.new_no_args_compiles);
        w.bool(s.global_value);
    });
    w.vec(&t.alt_names, |w, s| w.str(s));
    w.vec(&t.properties, w_prop);
    w.vec(&t.methods, w_method);
    w.vec(&t.constructors, w_sig);
    w.vec(&t.values, |w, v| {
        w_names(w, &v.name_ru, &v.name_en);
        w.opt(&v.ordinal, |w, n| w.bytes(&n.to_le_bytes()));
        w.opt(&v.presentation_ru, |w, s| w.str(s));
        w.opt(&v.presentation_en, |w, s| w.str(s));
    });
}
fn w_global(w: &mut W, g: &GlobalMethod) {
    w_method(w, &g.info);
    w.bool(g.server_known);
    w.bool(g.builtin);
    w.bool(g.is_function);
    w.u8(g.min_args);
    w.opt(&g.max_args, |w, v| w.u8(*v));
    w.bool(g.arity_compile);
}
fn w_table(w: &mut W, t: &QueryTableInfo) {
    w.str(&t.id);
    w.opt(&t.group, |w, s| w.str(s));
    w_names(w, &t.name_ru, &t.name_en);
    w.vec(&t.fields, |w, f| {
        w_names(w, &f.name_ru, &f.name_en);
        w.vec(&f.types, |w, ft| {
            w.opt(&ft.id, |w, s| w.str(s));
            w_names(w, &ft.name_ru, &ft.name_en);
        });
    });
    w.vec(&t.params, |w, p| {
        w_names(w, &p.name_ru, &p.name_en);
        w.bool(p.optional);
    });
}

fn w_data(w: &mut W, d: &CatalogData) {
    w.vec(&d.types, w_type);
    w.vec(&d.global_properties, w_prop);
    w.vec(&d.global_methods, w_global);
    w.vec(&d.query_tables, w_table);
    w.vec(&d.query_table_groups, |w, g| {
        w.str(&g.id);
        w_names(w, &g.name_ru, &g.name_en);
    });
}
pub(crate) fn copy_bytes(d: &CatalogData) -> Result<u64, memory_budget::BudgetError> {
    let mut writer = W(Vec::new(), Some(0), false);
    w_data(&mut writer, d);
    if writer.2 {
        return Err(memory_budget::BudgetError::Overflow);
    }
    Ok(writer.1.unwrap_or(0))
}

type R<T> = Result<T, DecodeError>;

fn err<T>(msg: &str) -> R<T> {
    Err(DecodeError(msg.to_string()))
}

struct Rd<'a> {
    input: &'a [u8],
    count: Option<u64>,
    exact: bool,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        if n > self.input.len() {
            return err("unexpected end of data");
        }
        let (a, b) = self.input.split_at(n);
        self.input = b;
        Ok(a)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> R<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => err("bad boolean byte"),
        }
    }
    fn u16(&mut self) -> R<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> R<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn str(&mut self) -> R<String> {
        let n = self.u32()? as usize;
        let b = self.take(n)?;
        match std::str::from_utf8(b) {
            Ok(s) => {
                if let Some(total) = self.count {
                    self.count = Some(
                        total
                            .checked_add(n as u64)
                            .ok_or_else(|| DecodeError("allocation size overflow".into()))?,
                    );
                    Ok(String::new())
                } else {
                    let mut value = String::new();
                    value
                        .try_reserve_exact(n)
                        .map_err(|_| DecodeError("allocation failed".into()))?;
                    value.push_str(s);
                    Ok(value)
                }
            }
            Err(_) => err("invalid UTF-8"),
        }
    }
    fn opt<T>(&mut self, f: impl FnOnce(&mut Self) -> R<T>) -> R<Option<T>> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(f(self)?)),
            _ => err("bad option byte"),
        }
    }
    fn vec<T>(&mut self, f: impl Fn(&mut Self) -> R<T>) -> R<Vec<T>> {
        let n = self.u32()? as usize;
        if n > self.input.len() {
            return err("element count exceeds remaining data");
        }
        let mut v = Vec::new();
        if let Some(total) = self.count {
            let bytes = n
                .checked_mul(std::mem::size_of::<T>())
                .ok_or_else(|| DecodeError("allocation size overflow".into()))?;
            self.count = Some(
                total
                    .checked_add(bytes as u64)
                    .ok_or_else(|| DecodeError("allocation size overflow".into()))?,
            );
            for _ in 0..n {
                drop(f(self)?);
            }
        } else {
            v.try_reserve_exact(if self.exact { n } else { n.min(1024) })
                .map_err(|_| DecodeError("allocation failed".into()))?;
            for _ in 0..n {
                v.push(f(self)?);
            }
        }
        Ok(v)
    }
}

fn r_avail(r: &mut Rd) -> R<Avail> {
    let v = r.u16()?;
    if v & !Avail::ALL.0 != 0 {
        return err("unknown availability bits");
    }
    Ok(Avail(v))
}
fn r_names(r: &mut Rd) -> R<(String, String)> {
    Ok((r.str()?, r.str()?))
}
fn r_param(r: &mut Rd) -> R<ParamInfo> {
    let (name_ru, name_en) = r_names(r)?;
    Ok(ParamInfo {
        name_ru,
        name_en,
        optional: r.bool()?,
        variadic: r.bool()?,
    })
}
fn r_result(r: &mut Rd) -> R<ResultType> {
    Ok(ResultType {
        id: r.opt(Rd::str)?,
        name_ru: r.str()?,
        name_en: r.opt(Rd::str)?,
    })
}
fn r_sig(r: &mut Rd) -> R<Signature> {
    Ok(Signature {
        params: r.vec(r_param)?,
        return_types: r.opt(|r| r.vec(r_result))?,
    })
}
fn r_prop(r: &mut Rd) -> R<PropertyInfo> {
    let (name_ru, name_en) = r_names(r)?;
    Ok(PropertyInfo {
        name_ru,
        name_en,
        readable: r.bool()?,
        writable: r.bool()?,
        avail: r_avail(r)?,
        types: r.vec(r_result)?,
    })
}
fn r_method(r: &mut Rd) -> R<MethodInfo> {
    let (name_ru, name_en) = r_names(r)?;
    Ok(MethodInfo {
        name_ru,
        name_en,
        variants: r.vec(r_sig)?,
        returns_value: r.bool()?,
        avail: r_avail(r)?,
    })
}
fn r_type(r: &mut Rd) -> R<TypeInfo> {
    let id = r.str()?;
    let (name_ru, name_en) = r_names(r)?;
    let kind = match r.u8()? {
        0 => TypeKind::Primitive,
        1 => TypeKind::Class,
        2 => TypeKind::Enum,
        _ => return err("bad type kind"),
    };
    Ok(TypeInfo {
        id,
        name_ru,
        name_en,
        kind,
        avail: r_avail(r)?,
        metadata_template: r.bool()?,
        collection: r.opt(|r| {
            Ok(CollectionFlags {
                for_each: r.bool()?,
                index_access: r.bool()?,
            })
        })?,
        serializable: r.bool()?,
        server_exchange: r.bool()?,
        xdto: r.opt(|r| {
            let (namespace, name) = r_names(r)?;
            Ok(XdtoName { namespace, name })
        })?,
        guid: r.opt(|r| {
            let mut g = [0u8; 16];
            g.copy_from_slice(r.take(16)?);
            Ok(g)
        })?,
        since: r.opt(Rd::str)?,
        compat_mode: r.opt(Rd::str)?,
        presentation_ru: r.opt(Rd::str)?,
        presentation_en: r.opt(Rd::str)?,
        new_no_args: r.opt(Rd::bool)?,
        type_fn: r.opt(Rd::bool)?,
        type_fn_en: r.opt(Rd::bool)?,
        new_error: r.opt(|r| match r.u8()? {
            0 => Ok(NewError::CtorNotFound),
            1 => Ok(NewError::NotObject),
            2 => Ok(NewError::CtorFails),
            _ => err("bad constructor error"),
        })?,
        value_type_presentation_ru: r.opt(Rd::str)?,
        value_type_presentation_en: r.opt(Rd::str)?,
        enum_global: r.bool()?,
        server_compile: r.opt(|r| {
            Ok(ServerCompile {
                type_known: r.bool()?,
                new_no_args_compiles: r.bool()?,
                global_value: r.bool()?,
            })
        })?,
        alt_names: r.vec(Rd::str)?,
        properties: r.vec(r_prop)?,
        methods: r.vec(r_method)?,
        constructors: r.vec(r_sig)?,
        values: r.vec(|r| {
            let (name_ru, name_en) = r_names(r)?;
            Ok(EnumValueInfo {
                name_ru,
                name_en,
                ordinal: r.opt(|r| r.u32().map(|n| i32::from_le_bytes(n.to_le_bytes())))?,
                presentation_ru: r.opt(Rd::str)?,
                presentation_en: r.opt(Rd::str)?,
            })
        })?,
    })
}
fn r_global(r: &mut Rd) -> R<GlobalMethod> {
    Ok(GlobalMethod {
        info: r_method(r)?,
        server_known: r.bool()?,
        builtin: r.bool()?,
        is_function: r.bool()?,
        min_args: r.u8()?,
        max_args: r.opt(Rd::u8)?,
        arity_compile: r.bool()?,
    })
}
fn r_table(r: &mut Rd) -> R<QueryTableInfo> {
    let id = r.str()?;
    let group = r.opt(Rd::str)?;
    let (name_ru, name_en) = r_names(r)?;
    let fields = r.vec(|r| {
        let (name_ru, name_en) = r_names(r)?;
        let types = r.vec(|r| {
            let id = r.opt(Rd::str)?;
            let (name_ru, name_en) = r_names(r)?;
            Ok(FieldType {
                id,
                name_ru,
                name_en,
            })
        })?;
        Ok(QueryFieldInfo {
            name_ru,
            name_en,
            types,
        })
    })?;
    let params = r.vec(|r| {
        let (name_ru, name_en) = r_names(r)?;
        Ok(QueryParamInfo {
            name_ru,
            name_en,
            optional: r.bool()?,
        })
    })?;
    Ok(QueryTableInfo {
        id,
        group,
        name_ru,
        name_en,
        fields,
        params,
    })
}

fn r_data(r: &mut Rd) -> R<CatalogData> {
    if let Some(bytes) = r.input.first_chunk::<4>()
        && u32::from_le_bytes(*bytes) > u32::from(u16::MAX) + 1
    {
        return err("too many types");
    }
    let types = r.vec(r_type)?;
    if types.len() > usize::from(u16::MAX) + 1 {
        return err("too many types");
    }
    Ok(CatalogData {
        types,
        global_properties: r.vec(r_prop)?,
        global_methods: r.vec(r_global)?,
        query_tables: r.vec(r_table)?,
        query_table_groups: r.vec(|r| {
            let id = r.str()?;
            let (name_ru, name_en) = r_names(r)?;
            Ok(QueryTableGroup {
                id,
                name_ru,
                name_en,
            })
        })?,
    })
}

pub(crate) fn split_header(bytes: &[u8]) -> R<(&str, &[u8])> {
    split_header_view(bytes).map_err(|message| DecodeError(message.to_string()))
}

pub(crate) fn split_header_view(bytes: &[u8]) -> Result<(&str, &[u8]), &'static str> {
    let Some(rest) = bytes.strip_prefix(MAGIC.as_slice()) else {
        return Err("bad magic");
    };
    let Some((len, rest)) = rest.split_first_chunk::<2>() else {
        return Err("truncated platform build");
    };
    let len = usize::from(u16::from_le_bytes(*len));
    if len > MAX_PLATFORM {
        return Err("platform build too long");
    }
    let Some((platform, body)) = rest.split_at_checked(len) else {
        return Err("truncated platform build");
    };
    let Ok(platform) = std::str::from_utf8(platform) else {
        return Err("platform build is not UTF-8");
    };
    if !platform.is_empty() && !crate::is_platform_build(platform) {
        return Err("invalid platform build");
    }
    Ok((platform, body))
}

impl Catalog {
    pub fn decode(bytes: &[u8]) -> Result<Catalog, DecodeError> {
        let (platform, body) = split_header(bytes)?;
        let mut raw = Vec::new();
        let n = DeflateDecoder::new(body)
            .take(MAX_INFLATED + 1)
            .read_to_end(&mut raw);
        match n {
            Ok(n) if n as u64 <= MAX_INFLATED => {}
            Ok(_) => return err("inflated data too large"),
            Err(e) => return Err(DecodeError(format!("inflate: {e}"))),
        }
        let mut count = Rd {
            input: &raw,
            count: Some(0),
            exact: true,
        };
        let _ = r_data(&mut count)?;
        let mut r = Rd {
            input: &raw,
            count: None,
            exact: true,
        };
        let data = r_data(&mut r)?;
        if !r.input.is_empty() {
            return err("trailing bytes");
        }
        let c = Catalog::from_data(data);
        if platform.is_empty() {
            return Ok(c);
        }
        c.with_platform(platform)
    }

    pub fn encode(&self) -> Vec<u8> {
        let d = self.data();
        let mut w = W::default();
        w.vec(&d.types, w_type);
        w.vec(&d.global_properties, w_prop);
        w.vec(&d.global_methods, w_global);
        w.vec(&d.query_tables, w_table);
        w.vec(&d.query_table_groups, |w, g| {
            w.str(&g.id);
            w_names(w, &g.name_ru, &g.name_en);
        });
        let platform = self.platform().as_bytes();
        let mut head = MAGIC.to_vec();
        head.extend(u16::try_from(platform.len()).unwrap_or(0).to_le_bytes());
        head.extend(platform);
        let mut enc = DeflateEncoder::new(head, Compression::best());
        let _ = enc.write_all(&w.0);
        enc.finish().unwrap_or_default()
    }
}

const INFLATE_WORKSPACE: u64 = 128 << 10;

impl Catalog {
    pub fn decode_with_memory(
        bytes: &[u8],
        budget: &memory_budget::MemoryBudget,
    ) -> Result<crate::AdmittedCatalog, crate::ManagedDecodeError> {
        use memory_budget::{BudgetError, Category};
        let workspace = budget.try_reserve(Category::ReaderWorking, INFLATE_WORKSPACE)?;
        let result = (|| -> Result<_, DecodeError> {
            let (platform, body) = split_header(bytes)?;
            let mut count = 0usize;
            {
                let mut decoder = DeflateDecoder::new(body);
                let mut chunk = [0u8; 8192];
                loop {
                    let n = decoder
                        .read(&mut chunk)
                        .map_err(|_| DecodeError("inflate failed".into()))?;
                    if n == 0 {
                        break;
                    }
                    count = count
                        .checked_add(n)
                        .ok_or_else(|| DecodeError("inflated data too large".into()))?;
                    if count as u64 > MAX_INFLATED {
                        return err("inflated data too large");
                    }
                }
            }
            Ok((platform, body, count))
        })();
        let (platform, body, count) = match result {
            Ok(parts) => parts,
            Err(error) => return Err(crate::ManagedDecodeError::data(error, workspace)),
        };
        let raw_charge = budget.try_reserve(Category::ReaderWorking, count as u64)?;
        let mut raw = Vec::new();
        raw.try_reserve_exact(count)
            .map_err(|_| BudgetError::AllocationFailed)?;
        raw.resize(count, 0);
        let parsed = (|| -> Result<_, DecodeError> {
            {
                let mut decoder = DeflateDecoder::new(body);
                decoder
                    .read_exact(&mut raw)
                    .map_err(|_| DecodeError("inflate failed".into()))?;
            }
            let mut reader = Rd {
                input: &raw,
                count: Some(0),
                exact: true,
            };
            let _ = r_data(&mut reader)?;
            if !reader.input.is_empty() {
                return err("trailing bytes");
            }
            Ok(reader.count.unwrap_or(0))
        })();
        let typed = match parsed {
            Ok(bytes) => bytes,
            Err(error) => return Err(crate::ManagedDecodeError::data(error, workspace)),
        };
        let charge = budget.try_reserve(
            Category::Metadata,
            typed
                .checked_add(platform.len() as u64)
                .and_then(|n| n.checked_add(std::mem::size_of::<Catalog>() as u64))
                .ok_or(BudgetError::Overflow)?,
        )?;
        let mut reader = Rd {
            input: &raw,
            count: None,
            exact: true,
        };
        let data = match r_data(&mut reader) {
            Ok(data) => data,
            Err(error) => return Err(crate::ManagedDecodeError::data(error, workspace)),
        };
        charge.try_grow(crate::index::Indexes::bytes(&data)?)?;
        let ix = crate::index::Indexes::try_build(&data)?;
        let mut platform_text = String::new();
        platform_text
            .try_reserve_exact(platform.len())
            .map_err(|_| BudgetError::AllocationFailed)?;
        platform_text.push_str(platform);
        let value = Catalog {
            data,
            ix,
            platform: platform_text,
        };
        drop(raw);
        drop(raw_charge);
        drop(workspace);
        Ok(crate::AdmittedCatalog {
            value,
            _charge: charge,
        })
    }
}
