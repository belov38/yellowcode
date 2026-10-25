use crate::Catalog;
use crate::model::*;
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use std::io::{Read, Write};

const MAGIC: &[u8; 8] = b"BSLCAT\x03\x00";
pub(crate) const MAX_PLATFORM: usize = 64;
const MAX_INFLATED: u64 = 64 << 20;

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.0.extend_from_slice(&(n as u32).to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.0.extend_from_slice(s.as_bytes());
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
fn w_sig(w: &mut W, s: &Signature) {
    w.vec(&s.params, w_param);
}
fn w_prop(w: &mut W, p: &PropertyInfo) {
    w_names(w, &p.name_ru, &p.name_en);
    w.bool(p.readable);
    w.bool(p.writable);
    w_avail(w, &p.avail);
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
    w.opt(&t.guid, |w, g| w.0.extend_from_slice(g));
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
        w.opt(&v.ordinal, |w, n| w.0.extend_from_slice(&n.to_le_bytes()));
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

type R<T> = Result<T, DecodeError>;

fn err<T>(msg: &str) -> R<T> {
    Err(DecodeError(msg.to_string()))
}

struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        if n > self.0.len() {
            return err("unexpected end of data");
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
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
            Ok(s) => Ok(s.to_string()),
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
        if n > self.0.len() {
            return err("element count exceeds remaining data");
        }
        let mut v = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            v.push(f(self)?);
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
fn r_sig(r: &mut Rd) -> R<Signature> {
    Ok(Signature {
        params: r.vec(r_param)?,
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
    let Some(rest) = bytes.strip_prefix(MAGIC.as_slice()) else {
        return err("bad magic");
    };
    let Some((len, rest)) = rest.split_first_chunk::<2>() else {
        return err("truncated platform build");
    };
    let len = usize::from(u16::from_le_bytes(*len));
    if len > MAX_PLATFORM {
        return err("platform build too long");
    }
    let Some((platform, body)) = rest.split_at_checked(len) else {
        return err("truncated platform build");
    };
    let Ok(platform) = std::str::from_utf8(platform) else {
        return err("platform build is not UTF-8");
    };
    if !platform.is_empty() && !crate::is_platform_build(platform) {
        return err("invalid platform build");
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
        let mut r = Rd(&raw);
        let data = r_data(&mut r)?;
        if !r.0.is_empty() {
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
