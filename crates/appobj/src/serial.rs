use crate::Application;
use crate::errors::{self, keys};
use crate::model::{ObjId, ObjKind};
use crate::types::Category;
use crate::values::refs::{RefObject, RefValue, ref_value};
use bsl_vm::{
    AppSerializer, AppXdto, Exception, Execution, HostError, HostObject, ObjRef, TypeId, Value,
};
use ibschema::values::RefId;
use platform_catalog::XdtoName;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(crate) struct AppSerializerImpl {
    app: Arc<Application>,
    by_type: OnceLock<HashMap<[u8; 16], ObjId>>,
    by_value: OnceLock<HashMap<[u8; 16], ObjId>>,
}

impl AppSerializerImpl {
    pub(crate) fn new(app: Arc<Application>) -> AppSerializerImpl {
        AppSerializerImpl {
            app,
            by_type: OnceLock::new(),
            by_value: OnceLock::new(),
        }
    }

    fn ref_guids(&self, value: bool) -> HashMap<[u8; 16], ObjId> {
        let mut map = HashMap::new();
        for def in &self.app.model().objects {
            if !def.kind.is_reference() && def.kind != ObjKind::Enum {
                continue;
            }
            for g in def.generated.iter().filter(|g| g.category == "Ref") {
                let id = if value { g.value_id } else { g.type_id };
                map.entry(*id.as_bytes()).or_insert(def.id);
            }
        }
        map
    }

    fn obj_of_type(&self, g: &[u8; 16]) -> Option<ObjId> {
        let map = self.by_type.get_or_init(|| self.ref_guids(false));
        map.get(g).copied()
    }

    fn as_ref_obj(v: &ObjRef) -> Option<&RefObject> {
        v.as_any().downcast_ref::<RefObject>()
    }

    fn value_of(&self, ex: &mut Execution, r: RefValue) -> Result<Value, Exception> {
        ref_value(&self.app, r).map_err(|e| ex.error(e))
    }
}

fn platform_error(key: &str) -> HostError {
    match errors::msg(key) {
        Some(m) => HostError::text(m.ru, m.en),
        None => HostError::text(key, key),
    }
}

fn format_error() -> HostError {
    platform_error(keys::LIST_STREAM_ERROR)
}

fn xml_conversion_error() -> HostError {
    platform_error(keys::XML_DATA_CONVERSION)
}

fn ref_text(table: Option<u32>, id: &RefId) -> String {
    format!("{}:{}", table.unwrap_or(0), hex(&id.0))
}

fn hex(bytes: &[u8; 16]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(s: &str) -> Option<RefId> {
    if s.len() != 32 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(RefId(out))
}

impl AppSerializer for AppSerializerImpl {
    fn xdto_type(&self, t: TypeId) -> Option<XdtoName> {
        self.app.types()?.xdto_name(t)
    }

    fn internal_write(
        &self,
        _ex: &mut Execution,
        v: &ObjRef,
    ) -> Result<Option<Vec<brace::Node>>, Exception> {
        if let Some(o) = Self::as_ref_obj(v) {
            let text = ref_text(o.table_number(), &o.r.id);
            return Ok(Some(vec![brace::Node::Atom(text)]));
        }
        let is_object = self
            .app
            .types()
            .and_then(|t| t.describe(HostObject::type_id(&**v)))
            .is_some_and(|(_, cat)| cat == Category::Object);
        Ok(is_object.then(Vec::new))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        type_guid: &[u8; 16],
        items: &[brace::Node],
    ) -> Result<Option<Value>, Exception> {
        let Some(obj) = self.obj_of_type(type_guid) else {
            return Ok(None);
        };
        let text = match items.first() {
            Some(brace::Node::Atom(s) | brace::Node::Str(s)) => s.as_str(),
            _ => return Err(ex.error(format_error())),
        };
        let id = text
            .split_once(':')
            .and_then(|(_, h)| unhex(h))
            .ok_or_else(|| ex.error(format_error()))?;
        self.value_of(ex, RefValue { obj, id }).map(Some)
    }

    fn value_guid(&self, t: TypeId) -> Option<[u8; 16]> {
        let (obj, cat) = self.app.types()?.describe(t)?;
        if cat != Category::Ref {
            return None;
        }
        let def = self.app.model().obj(obj);
        def.generated
            .iter()
            .find(|g| g.category == "Ref")
            .map(|g| *g.value_id.as_bytes())
    }

    fn type_by_value_guid(&self, g: &[u8; 16]) -> Option<TypeId> {
        let map = self.by_value.get_or_init(|| self.ref_guids(true));
        let obj = *map.get(g)?;
        self.app.types()?.type_of(obj, Category::Ref)
    }

    fn xdto_write(&self, _ex: &mut Execution, v: &ObjRef) -> Result<Option<AppXdto>, Exception> {
        let Some(o) = Self::as_ref_obj(v) else {
            return Ok(None);
        };
        let def = self.app.model().obj(o.r.obj);
        if def.kind == ObjKind::Enum {
            let name = def
                .enum_values
                .iter()
                .find(|e| RefId::from_uuid(e.md) == o.r.id)
                .map_or("", |e| e.name.as_str());
            return Ok(Some(AppXdto::Text(name.to_string())));
        }
        Ok(Some(AppXdto::Text(o.r.id.to_uuid().to_string())))
    }

    fn xdto_read(&self, ex: &mut Execution, t: TypeId, data: AppXdto) -> Result<Value, Exception> {
        let described = self.app.types().and_then(|ty| ty.describe(t));
        let (Some((obj, Category::Ref)), AppXdto::Text(s)) = (described, &data) else {
            return Err(ex.error(HostError::not_implemented(
                "СериализаторXDTO",
                "ПрочитатьXML",
            )));
        };
        let def = self.app.model().obj(obj);
        let id = if s.is_empty() {
            RefId::NIL
        } else if def.kind == ObjKind::Enum {
            def.enum_values
                .iter()
                .find(|e| e.name == *s)
                .map(|e| RefId::from_uuid(e.md))
                .ok_or_else(|| ex.error(xml_conversion_error()))?
        } else {
            brace::Guid::parse(s)
                .map(RefId::from_uuid)
                .ok_or_else(|| ex.error(xml_conversion_error()))?
        };
        self.value_of(ex, RefValue { obj, id })
    }
}
