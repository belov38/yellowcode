use super::data::{FactoryRef, XdtoData, data_to_value, factory_data_copy};
use super::{XS, builtin_root, headed, text_for};
use crate::bind::{key_error, msg};
use crate::serial::cycle_fatal;
use crate::serial::depth::DepthGuard;
use crate::serial::kind::platform_type;
use bsl_lib::xdto::{Model, QName};
use bsl_prim::Str;
use bsl_vm::{
    AppXdto, CallError, Cause, Exception, Execution, FatalError, FatalKind, HostError, HostObject,
    ObjRef, TypeId, Value,
};
use platform_catalog::{TypeIdx, catalog};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock, RwLock};

#[path = "applied_map.rs"]
mod applied_map;

pub(crate) const NS_CORE: &str = "http://v8.1c.ru/8.1/data/core";
const NS_OLD: &str = "http://v8.1c.ru/data";
const NS_TYPES: &str = "http://v8.1c.ru/8.2/data/types";

#[allow(clippy::wrong_self_convention)]
pub trait XdtoCodec: Send + Sync {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception>;

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception>;

    fn xdto_type(&self) -> QName;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservedXdtoName(pub QName);

impl std::fmt::Display for ReservedXdtoName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "XDTO type {} is reserved", self.0.clark())
    }
}

impl std::error::Error for ReservedXdtoName {}

#[derive(Default)]
struct Table {
    by_type: HashMap<u16, Arc<dyn XdtoCodec>>,
    by_name: HashMap<QName, u16>,
    reserved_types: std::collections::HashSet<u16>,
    reserved_names: std::collections::HashSet<QName>,
}

impl Table {
    fn is_reserved(&self, q: &QName) -> bool {
        q.ns == XS
            || (q.ns == NS_CORE && (q.name == "Null" || q.name == "Type"))
            || self.reserved_names.contains(q)
    }

    fn add_builtin(&mut self, t: TypeIdx, c: Arc<dyn XdtoCodec>) {
        let name = c.xdto_type();
        self.reserved_types.insert(t.0);
        self.reserved_names.insert(name.clone());
        self.by_type.entry(t.0).or_insert(c);
        self.by_name.entry(name).or_insert(t.0);
    }

    fn register(&mut self, t: TypeIdx, c: Arc<dyn XdtoCodec>) -> Result<(), ReservedXdtoName> {
        let name = c.xdto_type();
        let taken = self.by_name.get(&name).is_some_and(|owner| *owner != t.0);
        if self.reserved_types.contains(&t.0) || self.is_reserved(&name) || taken {
            return Err(ReservedXdtoName(name));
        }
        if let Some(old) = self.by_type.get(&t.0) {
            let old = old.xdto_type();
            if self.by_name.get(&old) == Some(&t.0) {
                self.by_name.remove(&old);
            }
        }
        self.by_name.insert(name, t.0);
        self.by_type.insert(t.0, c);
        Ok(())
    }
}

fn table() -> &'static RwLock<Table> {
    static T: OnceLock<RwLock<Table>> = OnceLock::new();
    T.get_or_init(|| RwLock::new(Table::default()))
}

fn builtins() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let mut t = table().write().unwrap_or_else(|e| e.into_inner());
        let ct = crate::collections::types();
        let own: Vec<(TypeIdx, Arc<dyn XdtoCodec>)> = vec![
            (ct.uuid, Arc::new(TextCodec::new(ct.uuid, NS_CORE, "UUID"))),
            (
                ct.binary,
                Arc::new(TextCodec::new(ct.binary, XS, "base64Binary")),
            ),
            (
                crate::serial::storage::storage_type(),
                Arc::new(TextCodec::new(
                    crate::serial::storage::storage_type(),
                    NS_CORE,
                    "ValueStorage",
                )),
            ),
        ];
        for (idx, c) in own
            .into_iter()
            .chain(crate::collections::xdto_codecs())
            .chain(crate::ui::xdto_codecs())
            .chain(crate::values::period::xdto_codecs())
        {
            t.add_builtin(idx, c);
        }
    });
}

pub fn register_xdto_codec(t: TypeIdx, c: Arc<dyn XdtoCodec>) -> Result<(), ReservedXdtoName> {
    builtins();
    table()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .register(t, c)
}

fn codec_of_type(t: TypeIdx) -> Option<Arc<dyn XdtoCodec>> {
    builtins();
    table()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .by_type
        .get(&t.0)
        .cloned()
}

pub(crate) fn has_codec(q: &QName) -> bool {
    codec_of_name(q).is_some()
}

fn codec_of_name(q: &QName) -> Option<Arc<dyn XdtoCodec>> {
    builtins();
    let tab = table().read().unwrap_or_else(|e| e.into_inner());
    let idx = tab.by_name.get(q)?;
    tab.by_type.get(idx).cloned()
}

pub(crate) fn core(name: &str) -> QName {
    QName::new(NS_CORE, name)
}

fn xs(name: &str) -> QName {
    QName::new(XS, name)
}

pub struct XdtoCtx {
    factory: FactoryRef,
    guard: DepthGuard,
    bytes: u64,
    live: u64,
    param: usize,
    pending: Option<HostError>,
    marker: Rc<FatalError>,
    wire_read: bool,
}

const NODE_BYTES: u64 = std::mem::size_of::<XdtoData>() as u64 + 64;

impl XdtoCtx {
    pub(crate) fn new(factory: FactoryRef, param: usize) -> XdtoCtx {
        XdtoCtx {
            factory,
            guard: DepthGuard::new(),
            bytes: 0,
            live: 0,
            param,
            pending: None,
            wire_read: false,
            marker: Rc::new(FatalError {
                kind: FatalKind::StackOverflow,
                text: String::new(),
                cause: None,
            }),
        }
    }

    pub(crate) fn wire_read(&mut self) {
        self.wire_read = true;
    }

    pub fn model(&self) -> &Model {
        &self.factory.model
    }

    pub fn fail(&mut self, e: HostError) -> Exception {
        self.pending = Some(e);
        Exception::Fatal(self.marker.clone())
    }

    pub(crate) fn exc(&mut self, e: CallError) -> Exception {
        match e {
            CallError::Host(h) => self.fail(h),
            CallError::Exc(x) => x,
        }
    }

    pub(crate) fn call_error(&mut self, e: Exception) -> CallError {
        if let Exception::Fatal(f) = &e
            && Rc::ptr_eq(f, &self.marker)
            && let Some(h) = self.pending.take()
        {
            return CallError::Host(h);
        }
        CallError::Exc(e)
    }

    pub fn account(&mut self, ex: &mut Execution, add: u64) -> Result<(), Exception> {
        self.bytes = self.bytes.saturating_add(add).saturating_add(NODE_BYTES);
        ex.charge(1 + add / 64)?;
        ex.reserve(self.bytes.saturating_mul(2))
    }

    pub fn hold(&mut self, ex: &mut Execution, n: usize) -> Result<(), Exception> {
        self.live = self.live.saturating_add(n as u64);
        crate::collections::reserve_values(ex, self.live)
    }

    pub fn release(&mut self, n: usize) {
        self.live = self.live.saturating_sub(n as u64);
    }

    pub fn enter(&mut self, ex: &Execution, reading: bool) -> Result<(), Exception> {
        if self.guard.enter().is_err() {
            return Err(if reading {
                self.fail(crate::xml::xml_error(msg::XML_INVALID_DATA))
            } else {
                cycle_fatal(ex)
            });
        }
        Ok(())
    }

    pub fn leave(&mut self) {
        self.guard.leave();
    }

    pub fn mismatch(&mut self, ex: &Execution) -> Exception {
        let suffix = format!(" ({})", text_for(ex, msg::XDTO_TYPE_MAPPING, &[]));
        self.fail(key_error(
            msg::TYPE_MISMATCH,
            vec![self.param.to_string()],
            suffix,
        ))
    }

    pub fn conversion_error(&mut self, ex: &Execution, text: &str, ty: &QName) -> Exception {
        let e = mapping_error(ex, &self.factory.model, text, ty);
        self.fail(e)
    }

    pub fn lexical_error(&mut self, ex: &Execution, text: &str, ty: &QName) -> Exception {
        let context = text_for(
            ex,
            msg::XDTO_VALUE_CONTEXT,
            &[
                text_for(ex, msg::XDTO_NODE_VALUE, &[]),
                text.to_string(),
                ty.clark(),
            ],
        );
        let line = text_for(
            ex,
            msg::XDTO_TYPE_MAPPING_LEXICAL,
            &[text.to_string(), ty.name.clone()],
        );
        let e = headed(msg::XDTO_SERIALIZATION, &context)
            .caused_by(Cause::Host(Box::new(headed(msg::XDTO_TYPE_MAPPING, &line))));
        self.fail(e)
    }

    pub fn to_data(&mut self, ex: &mut Execution, v: &Value) -> Result<XdtoData, Exception> {
        let (ty, text) = match v {
            Value::Undefined => {
                self.account(ex, 0)?;
                return Ok(XdtoData::Nil);
            }
            Value::Null => (core("Null"), String::new()),
            Value::Bool(_) => (xs("boolean"), self.text(ex, v)?),
            Value::Number(_) => (xs("decimal"), self.text(ex, v)?),
            Value::String(_) => (xs("string"), self.text(ex, v)?),
            Value::Date(_) => (xs("dateTime"), self.text(ex, v)?),
            Value::Type(t) => (core("Type"), type_qname(ex, *t).clark()),
            Value::Object(o) => return self.object_data(ex, o),
        };
        self.account(ex, text.len() as u64)?;
        Ok(XdtoData::Value {
            projection: None,
            ty,
            lexical: text,
            value: v.clone(),
        })
    }

    fn text(&mut self, ex: &mut Execution, v: &Value) -> Result<String, Exception> {
        match crate::convert::xml_text(ex, v) {
            Ok(t) => Ok(t),
            Err(e) => Err(self.exc(e)),
        }
    }

    fn object_data(&mut self, ex: &mut Execution, o: &ObjRef) -> Result<XdtoData, Exception> {
        let t = HostObject::type_id(&**o);
        if ex.runtime().types().app_type(t).is_some() {
            self.enter(ex, false)?;
            let result = self.applied_data(ex, o, t);
            self.leave();
            return result;
        }
        let Some(codec) = platform_type(ex, t).and_then(codec_of_type) else {
            return Err(self.mismatch(ex));
        };
        self.enter(ex, false)?;
        let r = codec.to_xdto(ex, o, self);
        self.leave();
        r
    }

    fn applied_data(
        &mut self,
        ex: &mut Execution,
        o: &ObjRef,
        t: TypeId,
    ) -> Result<XdtoData, Exception> {
        let Some(name) = ex
            .runtime()
            .types()
            .app_type(t)
            .and_then(|a| a.xdto.as_ref())
        else {
            return Err(self.mismatch(ex));
        };
        let name_bytes = name.namespace.len().saturating_add(name.name.len()) as u64;
        self.account(ex, name_bytes)?;
        let Some(name) = ex
            .runtime()
            .types()
            .app_type(t)
            .and_then(|a| a.xdto.as_ref())
        else {
            return Err(self.mismatch(ex));
        };
        let ty = QName::new(&name.namespace, &name.name);
        let app = ex.runtime().app().clone();
        let Some(serializer) = app.serializer() else {
            return Err(self.mismatch(ex));
        };
        match serializer.xdto_write(ex, o)? {
            Some(AppXdto::Text(lexical)) => {
                self.account(ex, lexical.capacity() as u64)?;
                Ok(XdtoData::Value {
                    projection: None,
                    ty,
                    lexical,
                    value: Value::Object(o.clone()),
                })
            }
            Some(AppXdto::Object(props)) => self.applied_object_data(ex, ty, &props),
            None => Err(self.mismatch(ex)),
        }
    }

    fn applied_value(
        &mut self,
        ex: &mut Execution,
        t: TypeId,
        d: &XdtoData,
    ) -> Result<Value, Exception> {
        let app = ex.runtime().app().clone();
        let Some(serializer) = app.serializer() else {
            return Err(self.mismatch(ex));
        };
        let projected = match d {
            XdtoData::Value { lexical, .. } => {
                self.account(ex, lexical.len() as u64)?;
                AppXdto::Text(lexical.clone())
            }
            XdtoData::Object { .. } if self.wire_read => self.applied_object_value(ex, d)?,
            _ => return Err(self.mismatch(ex)),
        };
        serializer.xdto_read(ex, t, projected)
    }

    pub fn from_data(&mut self, ex: &mut Execution, d: &XdtoData) -> Result<Value, Exception> {
        ex.charge(1)?;
        let ty = match d {
            XdtoData::Nil => return Ok(Value::Undefined),
            XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. } => ty,
        };
        if let Some(c) = codec_of_name(ty) {
            self.enter(ex, true)?;
            let r = c.from_xdto(ex, d, self);
            self.leave();
            return r;
        }
        if let XdtoData::Value { lexical, value, .. } = d {
            if ty.ns == XS {
                return Ok(value.clone());
            }
            if *ty == core("Null") {
                return Ok(Value::Null);
            }
            if *ty == core("Type") {
                return match type_from_qname(ex, lexical) {
                    Some(t) => Ok(Value::Type(t)),
                    None => Err(self.conversion_error(ex, lexical, ty)),
                };
            }
        }
        let reserved =
            ty.ns == XS || (ty.ns == NS_CORE && matches!(ty.name.as_str(), "Null" | "Type"));
        let applied = (!reserved)
            .then(|| applied_type_from_qname(ex, &ty.ns, &ty.name))
            .flatten();
        if let Some(t) = applied {
            self.enter(ex, true)?;
            let result = self.applied_value(ex, t, d);
            self.leave();
            return result;
        }
        match d {
            XdtoData::Value { value, .. } => Ok(value.clone()),
            _ => {
                let f = self.factory.clone();
                let canonical = factory_data_copy(ex, d)?;
                data_to_value(ex, &f, canonical)
            }
        }
    }
}

pub(crate) fn type_qname(ex: &Execution, t: TypeId) -> QName {
    match t {
        TypeId::NUMBER => return xs("decimal"),
        TypeId::STRING => return xs("string"),
        TypeId::DATE => return xs("dateTime"),
        TypeId::BOOL => return xs("boolean"),
        TypeId::NULL => return core("Null"),
        TypeId::TYPE => return core("Type"),
        _ => {}
    }
    let types = ex.runtime().types();
    if let Some(app) = types.app_type(t) {
        if let Some(x) = &app.xdto {
            return QName::new(&x.namespace, &x.name);
        }
        return QName::new(NS_TYPES, &app.name_en);
    }
    let Some(idx) = types.catalog_type(t) else {
        return QName::new(NS_TYPES, "Undefined");
    };
    if idx == crate::collections::types().binary {
        return xs("base64Binary");
    }
    let info = catalog().type_info(idx);
    match &info.xdto {
        Some(x) if !x.name.contains('<') => {
            let ns = if x.namespace == NS_OLD {
                NS_CORE
            } else {
                x.namespace.as_str()
            };
            QName::new(ns, &x.name)
        }
        _ => QName::new(NS_TYPES, &info.name_en),
    }
}

fn by_xdto_name() -> &'static HashMap<QName, TypeIdx> {
    static M: OnceLock<HashMap<QName, TypeIdx>> = OnceLock::new();
    M.get_or_init(|| {
        let mut m = HashMap::new();
        for (i, info) in catalog().types().iter().enumerate() {
            let Ok(i) = u16::try_from(i) else { break };
            let idx = TypeIdx(i);
            if let Some(x) = &info.xdto
                && !x.name.contains('<')
            {
                let ns = if x.namespace == NS_OLD {
                    NS_CORE
                } else {
                    x.namespace.as_str()
                };
                m.entry(QName::new(ns, &x.name)).or_insert(idx);
            }
            m.entry(QName::new(NS_TYPES, &info.name_en)).or_insert(idx);
        }
        m
    })
}

pub(crate) fn type_from_qname(ex: &Execution, clark: &str) -> Option<TypeId> {
    let (ns, local) = clark
        .strip_prefix('{')?
        .split_once('}')
        .unwrap_or(("", clark));
    let q = QName::new(ns, local);
    if ns == XS {
        return match local {
            "decimal" => Some(TypeId::NUMBER),
            "string" => Some(TypeId::STRING),
            "dateTime" => Some(TypeId::DATE),
            "boolean" => Some(TypeId::BOOL),
            "base64Binary" => Some(TypeId::of_catalog(crate::collections::types().binary)),
            _ => None,
        };
    }
    if ns == NS_CORE && local == "Null" {
        return Some(TypeId::NULL);
    }
    if ns == NS_CORE && local == "Type" {
        return Some(TypeId::TYPE);
    }
    if let Some(idx) = by_xdto_name().get(&q) {
        return Some(TypeId::of_catalog(*idx));
    }
    applied_type_from_qname(ex, ns, local)
}

fn applied_type_from_qname(ex: &Execution, ns: &str, local: &str) -> Option<TypeId> {
    let types = ex.runtime().types();
    (0..)
        .map(|n| TypeId(100_000 + n))
        .map_while(|t| types.app_type(t).map(|a| (t, a.xdto.as_ref())))
        .find_map(|(t, name)| {
            name.is_some_and(|name| name.namespace == ns && name.name == local)
                .then_some(t)
        })
}

fn language_type_name(ex: &Execution, m: &Model, ty: &QName) -> String {
    let t = type_from_qname(ex, &ty.clark()).or_else(|| match builtin_root(m, ty).as_deref() {
        Some(
            "decimal" | "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
            | "positiveInteger" | "nonPositiveInteger" | "negativeInteger" | "unsignedLong"
            | "unsignedInt" | "unsignedShort" | "unsignedByte" | "float" | "double",
        ) => Some(TypeId::NUMBER),
        Some("boolean") => Some(TypeId::BOOL),
        Some("dateTime" | "date") => Some(TypeId::DATE),
        Some("base64Binary") => Some(TypeId::of_catalog(crate::collections::types().binary)),
        _ => Some(TypeId::STRING),
    });
    let lang = ex.lang();
    t.map(|t| ex.runtime().types().name(t, lang).to_string())
        .unwrap_or_default()
}

pub(crate) fn mapping_error(ex: &Execution, m: &Model, text: &str, ty: &QName) -> HostError {
    let context = text_for(
        ex,
        msg::XDTO_VALUE_CONTEXT,
        &[
            text_for(ex, msg::XDTO_NODE_VALUE, &[]),
            text.to_string(),
            ty.clark(),
        ],
    );
    let line = text_for(
        ex,
        msg::XDTO_TYPE_MAPPING_FORMAT,
        &[ty.clark(), language_type_name(ex, m, ty)],
    );
    headed(msg::XDTO_SERIALIZATION, &context)
        .caused_by(Cause::Host(Box::new(headed(msg::XDTO_TYPE_MAPPING, &line))))
}

struct TextCodec {
    idx: TypeIdx,
    ty: QName,
}

impl TextCodec {
    fn new(idx: TypeIdx, ns: &str, name: &str) -> TextCodec {
        TextCodec {
            idx,
            ty: QName::new(ns, name),
        }
    }
}

impl XdtoCodec for TextCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let value = Value::Object(v.clone());
        let Some(text) = crate::serial::xmlvalue::xml_string(ex, &value)? else {
            return Err(s.mismatch(ex));
        };
        s.account(ex, text.len() as u64)?;
        Ok(XdtoData::Value {
            projection: None,
            ty: self.ty.clone(),
            lexical: text,
            value,
        })
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let XdtoData::Value { lexical, .. } = d else {
            return Err(s.conversion_error(ex, "", &self.ty));
        };
        ex.reserve((lexical.len() as u64).saturating_mul(2))?;
        match crate::convert::xml_from_text(
            ex,
            TypeId::of_catalog(self.idx),
            &Str::from(lexical.as_str()),
        ) {
            Ok(v) => Ok(v),
            Err(CallError::Host(_)) => Err(s.conversion_error(ex, lexical, &self.ty)),
            Err(CallError::Exc(e)) => Err(e),
        }
    }

    fn xdto_type(&self) -> QName {
        self.ty.clone()
    }
}
