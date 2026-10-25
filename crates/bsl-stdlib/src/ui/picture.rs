use super::{NS_UI, SharedOf, data_bool, enum_val, prop, prop_text, types};
use crate::bind::{arg_str, arg_value, key_error, msg, type_mismatch};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::fs::{fs, fs_error};
use crate::serial::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::values::{binary_data_arc, binary_data_bytes, new_binary_data_arc};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData};
use brace::Node;
use bsl_lib::xdto::QName;
use bsl_vm::{
    Arg, CallResult, Category, CategorySet, Exception, Execution, HostError, HostObject, MemberId,
    ObjRef, SharedValue, TypeId, Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) struct Picture {
    data: Option<Arc<[u8]>>,
    transparent: bool,
}

pub(super) fn comparison_resources(value: &Value) -> Option<(u64, u64)> {
    let picture = value.as_object()?.as_any().downcast_ref::<Picture>()?;
    Some((
        picture.data.as_ref().map_or(0, |b| b.len()) as u64 / 64 + 1,
        0,
    ))
}

type PictureData = (Option<Arc<[u8]>>, bool);

fn new_picture(data: Option<Arc<[u8]>>, transparent: bool) -> Value {
    Value::Object(Rc::new(Picture { data, transparent }))
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.is_empty() {
        return Ok(new_picture(None, false));
    }
    if args.len() > 2 {
        return Err(ctor_not_implemented(types().picture).into());
    }
    let first = arg_value(ex, args.first())?;
    let data = match &first {
        Value::String(_) => {
            let path = arg_str(ex, args.first())?.to_string_lossy();
            let f = fs(ex, &path)?;
            if let Some(st) = f.get().stat(&path).map_err(fs_error)? {
                crate::binary::guard_bytes(ex, st.size)?;
            }
            Arc::from(f.get().read(&path).map_err(fs_error)?)
        }
        v => match binary_data_arc(v) {
            Some(b) => b,
            None => return Err(type_mismatch(1).into()),
        },
    };
    let transparent = match arg_value(ex, args.get(1))? {
        Value::Undefined => false,
        Value::Bool(b) => b,
        _ => return Err(type_mismatch(2).into()),
    };
    Ok(new_picture(Some(data), transparent))
}

fn format_of(b: &[u8]) -> &'static str {
    let starts = |p: &[u8]| b.starts_with(p);
    if starts(b"\x89PNG\r\n\x1a\n") {
        "PNG"
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        "GIF"
    } else if starts(&[0xFF, 0xD8, 0xFF]) {
        "JPEG"
    } else if starts(b"BM") {
        "BMP"
    } else if starts(b"II*\0") || starts(b"MM\0*") {
        "TIFF"
    } else if starts(&[0, 0, 1, 0]) {
        "Icon"
    } else if starts(&[0xD7, 0xCD, 0xC6, 0x9A]) || starts(&[1, 0, 9, 0]) {
        "WMF"
    } else if b.get(40..44) == Some(b" EMF") {
        "EMF"
    } else if b.windows(4).take(512).any(|w| w == b"<svg") {
        "SVG"
    } else {
        "UnknownFormat"
    }
}

impl Picture {
    fn kind_name(&self) -> &'static str {
        if self.data.is_some() {
            "Absolute"
        } else {
            "Empty"
        }
    }
}

impl HostObject for Picture {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().picture)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().picture;
        match property_name(t, m) {
            Some("Type") => Ok(enum_val(types().picture_type, self.kind_name())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = types().picture;
        match property_ru(t, m) {
            "" => Err(not_implemented(t, m).into()),
            ru => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(ru)
                .raw()
                .into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        let t = types().picture;
        match method_name(t, m) {
            Some("Format") => match &self.data {
                Some(b) => Ok(enum_val(types().picture_format, format_of(b))),
                None => {
                    ex.charge(1)?;
                    let mut e = key_error(msg::PICTURE_TYPE, Vec::new(), String::new());
                    e.categories = Some(CategorySet::of(&[
                        Category::AllErrors,
                        Category::ScriptRuntimeError,
                        Category::ScriptUseError,
                    ]));
                    Err(e.into())
                }
            },
            Some("GetBinaryData") => Ok(match &self.data {
                Some(b) => new_binary_data_arc(b.clone()),
                None => Value::Undefined,
            }),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<Picture>()?;
        Some(self.transparent == o.transparent && self.data == o.data)
    }

    fn boost_hash(&self) -> Option<i32> {
        let h = bsl_prim::boost_hash::bytes(self.data.as_deref().unwrap_or(&[]));
        Some(h ^ i32::from(self.transparent))
    }

    fn to_shared(&self) -> Option<SharedValue> {
        let data: PictureData = (self.data.clone(), self.transparent);
        Some(SharedValue::Object(Arc::new(SharedOf::new(data, |d| {
            new_picture(d.0.clone(), d.1)
        }))))
    }
}

pub(super) struct PictureCodec;

fn picture_of(v: &ObjRef) -> Option<&Picture> {
    v.as_any().downcast_ref::<Picture>()
}

impl ValueCodec for PictureCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(p) = picture_of(v) else {
            return Ok(None);
        };
        let int = Node::int;
        let body = match &p.data {
            None => vec![
                int(4),
                int(0),
                Node::List(vec![int(0)]),
                Node::str(""),
                int(-1),
                int(-1),
                int(0),
                int(0),
                Node::str(""),
            ],
            Some(b) => {
                let data = if w.is_storage() {
                    ex.reserve(b.len() as u64)?;
                    int(w.attach(b.to_vec()).map_or(0, |n| n as i64))
                } else {
                    let text = (b.len() as u64).div_ceil(3).saturating_mul(4);
                    ex.reserve(text.saturating_mul(2))?;
                    w.account(ex, text + text / 32)?;
                    brace::base64_atom(b, brace::Newline::Lf, true)
                };
                vec![
                    int(4),
                    int(3),
                    Node::List(vec![int(0)]),
                    Node::str(""),
                    int(-1),
                    int(-1),
                    int(i64::from(p.transparent)),
                    Node::List(vec![Node::List(vec![data])]),
                    int(0),
                    Node::str(""),
                ]
            }
        };
        Ok(Some(vec![Node::List(body)]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let Some(b) = items.iter().find_map(Node::as_list) else {
            return Err(r.format_error(ex));
        };
        match b.get(1).and_then(Node::as_i64) {
            Some(0) => Ok(new_picture(None, false)),
            Some(3) => {
                let transparent = b.get(6).and_then(Node::as_i64) == Some(1);
                let atom = b
                    .get(7)
                    .and_then(Node::as_list)
                    .and_then(|l| l.first())
                    .and_then(Node::as_list)
                    .and_then(|l| l.first())
                    .and_then(Node::as_atom);
                let Some(atom) = atom else {
                    return Err(r.format_error(ex));
                };
                if atom.starts_with("#base64:") {
                    ex.reserve((atom.len() as u64).saturating_mul(2))?;
                    return match Node::atom(atom).base64() {
                        Some(bytes) => Ok(new_picture(Some(Arc::from(bytes)), transparent)),
                        None => Err(r.format_error(ex)),
                    };
                }
                match atom.parse::<usize>().ok().and_then(|n| r.stream(n)) {
                    Some(bytes) => {
                        ex.reserve(bytes.len() as u64)?;
                        Ok(new_picture(Some(Arc::from(bytes)), transparent))
                    }
                    None if r.is_storage() && atom.parse::<usize>().is_ok() => {
                        Ok(new_picture(Some(Arc::from(Vec::new())), transparent))
                    }
                    None => Err(r.format_error(ex)),
                }
            }
            _ => Err(r.format_error(ex)),
        }
    }
}

fn ui_picture() -> QName {
    QName::new(NS_UI, "Picture")
}

impl XdtoCodec for PictureCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(p) = picture_of(v) else {
            return Err(s.mismatch(ex));
        };
        let mut props = vec![("t".to_string(), vec![data_bool(p.transparent)])];
        if let Some(b) = &p.data {
            let len = (b.len() as u64).div_ceil(3).saturating_mul(4);
            ex.reserve(len.saturating_mul(3))?;
            s.account(ex, len.saturating_mul(2))?;
            props.push((
                "__content".to_string(),
                vec![XdtoData::Value {
                    projection: None,
                    ty: QName::new(crate::xdto::XS, "base64Binary"),
                    lexical: crate::serial::xmlvalue::wrapped_base64(b),
                    value: new_binary_data_arc(b.clone()),
                }],
            ));
        }
        Ok(XdtoData::Object {
            ty: ui_picture(),
            props,
        })
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let XdtoData::Object { props, .. } = d else {
            return Err(s.lexical_error(ex, "", &ui_picture()));
        };
        let transparent = prop_text(props, "t").is_some_and(|t| t == "true" || t == "1");
        let bytes = match prop(props, "__content") {
            Some(XdtoData::Value { lexical, value, .. }) => {
                if let Some(b) = binary_data_bytes(value) {
                    ex.reserve(b.len() as u64)?;
                    b.to_vec()
                } else {
                    ex.reserve((lexical.len() as u64).saturating_mul(2))?;
                    let units: Vec<u16> = lexical.encode_utf16().collect();
                    match bsl_lib::b64::base64_value(&units) {
                        Some(b) => b,
                        None => return Err(s.lexical_error(ex, lexical, &ui_picture())),
                    }
                }
            }
            _ => Vec::new(),
        };
        ex.reserve(bytes.len() as u64)?;
        Ok(if bytes.is_empty() {
            new_picture(None, false)
        } else {
            new_picture(Some(Arc::from(bytes)), transparent)
        })
    }

    fn xdto_type(&self) -> QName {
        ui_picture()
    }
}
