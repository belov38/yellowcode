use super::{
    NS_UI, SharedOf, data_bool, data_decimal, data_text, enum_by_xdto, enum_val, enum_xdto,
    model_names, model_ordinal, prop_text, types,
};
use crate::bind::{arg_value, msg, type_mismatch, value_mismatch};
use crate::collections::{ctor_not_implemented, not_implemented, property_name, property_ru};
use crate::enums::as_enum_value;
use crate::serial::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::serial::kind::{guid_text, parse_guid};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData};
use brace::Node;
use bsl_lib::xdto::QName;
use bsl_prim::Number;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef, SharedValue,
    TypeId, Value,
};
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const ENUM_GUIDS: &[(&str, &str)] = &[
    ("ControlBorderType", "48312c09-257f-4b29-b280-284dd89efc1e"),
    (
        "SpreadsheetDocumentCellLineType",
        "f527dc88-1d39-40b3-bcbb-d98b690ead68",
    ),
    ("ChartLineType", "e5cabe59-d992-4d31-8086-3116931aff81"),
];

const LINE_ENUMS: &[&str] = &[
    "SpreadsheetDocumentCellLineType",
    "ChartLineType",
    "SpreadsheetDocumentDrawingLineType",
    "GeographicalSchemaLineType",
];

fn enum_id(t: TypeIdx) -> &'static str {
    catalog().type_info(t).id.as_str()
}

fn guid_of(t: TypeIdx) -> Option<&'static str> {
    ENUM_GUIDS
        .iter()
        .find(|(id, _)| *id == enum_id(t))
        .map(|(_, g)| *g)
}

fn enum_of_guid(text: &str) -> Option<TypeIdx> {
    let want = parse_guid(text)?;
    ENUM_GUIDS
        .iter()
        .find(|(_, g)| parse_guid(g) == Some(want))
        .and_then(|(id, _)| crate::enums::type_by_id(id))
}

fn static_value(t: TypeIdx, name: &str) -> Option<&'static str> {
    catalog()
        .type_info(t)
        .values
        .iter()
        .map(|v| v.name_en.as_str())
        .find(|n| *n == name)
}

fn width_arg(ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<u32> {
    match arg_value(ex, a)? {
        Value::Undefined => Ok(1),
        Value::Number(n) => n
            .to_i64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| value_mismatch(param).into()),
        _ => Err(type_mismatch(param).into()),
    }
}

#[derive(Clone)]
pub(crate) struct BorderData {
    style: &'static str,
    width: u32,
}

pub(crate) struct Border {
    d: BorderData,
}

fn new_border(d: BorderData) -> Value {
    Value::Object(Rc::new(Border { d }))
}

fn border_type() -> TypeIdx {
    crate::enums::type_by_id("ControlBorderType").unwrap_or(types().border_kind)
}

pub(super) fn border_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(ctor_not_implemented(types().border).into());
    }
    let v = arg_value(ex, args.first())?;
    let Some(e) = as_enum_value(&v).filter(|e| e.type_idx() == border_type()) else {
        return Err(type_mismatch(1).into());
    };
    let width = width_arg(ex, args.get(1), 2)?;
    Ok(new_border(BorderData {
        style: e.name_en(),
        width,
    }))
}

impl HostObject for Border {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().border)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().border;
        match property_name(t, m) {
            Some("Type") => Ok(enum_val(types().border_kind, "Absolute")),
            Some("BorderType") => Ok(enum_val(border_type(), self.d.style)),
            Some("Width") => Ok(Value::from(i64::from(self.d.width))),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = types().border;
        match property_ru(t, m) {
            "" => Err(not_implemented(t, m).into()),
            ru => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(ru)
                .raw()
                .into()),
        }
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().border, m).into())
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<Border>()?;
        Some(o.d.style == self.d.style && o.d.width == self.d.width)
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(bsl_prim::boost_hash::ordinal(self.d.width))
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedOf::new(
            self.d.clone(),
            |d| new_border(d.clone()),
        ))))
    }
}

#[derive(Clone)]
pub(crate) struct LineData {
    ty: TypeIdx,
    style: &'static str,
    width: u32,
    gap: bool,
}

pub(crate) struct Line {
    d: LineData,
}

fn new_line(d: LineData) -> Value {
    Value::Object(Rc::new(Line { d }))
}

pub(super) fn line_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.is_empty() || args.len() > 3 {
        return Err(ctor_not_implemented(types().line).into());
    }
    let v = arg_value(ex, args.first())?;
    let Some(e) = as_enum_value(&v).filter(|e| LINE_ENUMS.contains(&enum_id(e.type_idx()))) else {
        return Err(type_mismatch(1).into());
    };
    let width = width_arg(ex, args.get(1), 2)?;
    let gap = match arg_value(ex, args.get(2))? {
        Value::Undefined => false,
        Value::Bool(b) => b,
        _ => return Err(type_mismatch(3).into()),
    };
    Ok(new_line(LineData {
        ty: e.type_idx(),
        style: e.name_en(),
        width,
        gap,
    }))
}

impl HostObject for Line {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().line)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().line;
        match property_name(t, m) {
            Some("LineType") => Ok(enum_val(self.d.ty, self.d.style)),
            Some("Width") => Ok(Value::from(i64::from(self.d.width))),
            Some("Gap") => Ok(Value::Bool(self.d.gap)),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = types().line;
        match property_ru(t, m) {
            "" => Err(not_implemented(t, m).into()),
            ru => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(ru)
                .raw()
                .into()),
        }
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().line, m).into())
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<Line>()?;
        Some(
            o.d.ty == self.d.ty
                && o.d.style == self.d.style
                && o.d.width == self.d.width
                && o.d.gap == self.d.gap,
        )
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(bsl_prim::boost_hash::ordinal(self.d.width))
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedOf::new(
            self.d.clone(),
            |d| new_line(d.clone()),
        ))))
    }
}

pub(super) struct BorderCodec;
pub(super) struct LineCodec;

fn unsupported(ex: &mut Execution, what: &str) -> Exception {
    ex.error(HostError::not_implemented(what, "Сериализация"))
}

fn ordinal_and_guid(t: TypeIdx, style: &str) -> Option<(usize, &'static str)> {
    let q = enum_xdto(t)?;
    Some((model_ordinal(&q, style)?, guid_of(t)?))
}

fn value_at(t: TypeIdx, n: usize) -> Option<&'static str> {
    let q = enum_xdto(t)?;
    static_value(t, model_names(&q).get(n)?)
}

impl ValueCodec for BorderCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(b) = v.as_any().downcast_ref::<Border>() else {
            return Ok(None);
        };
        let Some((n, guid)) = ordinal_and_guid(border_type(), b.d.style) else {
            return Err(unsupported(ex, "Рамка"));
        };
        w.account(ex, 96)?;
        Ok(Some(vec![
            Node::int(1),
            Node::List(vec![
                Node::int(3),
                Node::int(0),
                Node::List(vec![Node::int(0)]),
                Node::int(n as i64),
                Node::int(i64::from(b.d.width)),
                Node::int(3),
                Node::atom(guid_text(&parse_guid(guid).unwrap_or_default())),
            ]),
            Node::int(0),
        ]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let read = || -> Option<BorderData> {
            let b = items.iter().find_map(Node::as_list)?;
            let n = usize::try_from(b.get(3)?.as_i64()?).ok()?;
            let width = u32::try_from(b.get(4)?.as_i64()?).ok()?;
            let t = enum_of_guid(b.get(6)?.as_atom()?)?;
            Some(BorderData {
                style: value_at(t, n).filter(|_| t == border_type())?,
                width,
            })
        };
        match read() {
            Some(d) => Ok(new_border(d)),
            None => Err(r.format_error(ex)),
        }
    }
}

impl ValueCodec for LineCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(l) = v.as_any().downcast_ref::<Line>() else {
            return Ok(None);
        };
        let Some((n, guid)) = ordinal_and_guid(l.d.ty, l.d.style) else {
            return Err(unsupported(ex, "Линия"));
        };
        w.account(ex, 96)?;
        Ok(Some(vec![
            Node::int(1),
            Node::List(vec![
                Node::int(4),
                Node::int(0),
                Node::List(vec![Node::int(0)]),
                Node::int(n as i64),
                Node::int(i64::from(l.d.width)),
                Node::int(7),
                Node::atom(guid_text(&parse_guid(guid).unwrap_or_default())),
                Node::int(i64::from(l.d.gap)),
            ]),
            Node::int(0),
        ]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let read = || -> Option<LineData> {
            let b = items.iter().find_map(Node::as_list)?;
            let n = usize::try_from(b.get(3)?.as_i64()?).ok()?;
            let width = u32::try_from(b.get(4)?.as_i64()?).ok()?;
            let t = enum_of_guid(b.get(6)?.as_atom()?)?;
            if !LINE_ENUMS.contains(&enum_id(t)) {
                return None;
            }
            Some(LineData {
                ty: t,
                style: value_at(t, n)?,
                width,
                gap: b.get(7)?.as_i64()? == 1,
            })
        };
        match read() {
            Some(d) => Ok(new_line(d)),
            None => Err(r.format_error(ex)),
        }
    }
}

fn ui(name: &str) -> QName {
    QName::new(NS_UI, name)
}

fn style_data(s: &mut XdtoCtx, t: TypeIdx, style: &str) -> Result<XdtoData, Exception> {
    match enum_xdto(t) {
        Some(q) if model_ordinal(&q, style).is_some() => Ok(data_text(q, style)),
        _ => Err(s.fail(HostError::not_implemented("Рамка", "Сериализация"))),
    }
}

fn style_of(d: Option<&XdtoData>) -> Option<(TypeIdx, &'static str)> {
    let XdtoData::Value { ty, lexical, .. } = d? else {
        return None;
    };
    let t = enum_by_xdto(ty)?;
    Some((t, static_value(t, lexical)?))
}

impl XdtoCodec for BorderCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(b) = v.as_any().downcast_ref::<Border>() else {
            return Err(s.mismatch(ex));
        };
        let style = style_data(s, border_type(), b.d.style)?;
        s.account(ex, 128)?;
        Ok(XdtoData::Object {
            ty: ui("Border"),
            props: vec![
                (
                    "width".into(),
                    vec![data_decimal(Number::from_i64(i64::from(b.d.width)))],
                ),
                ("style".into(), vec![style]),
            ],
        })
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let XdtoData::Object { props, .. } = d else {
            return Err(s.lexical_error(ex, "", &ui("Border")));
        };
        let style = super::prop(props, "style");
        let width = prop_text(props, "width").and_then(|w| w.parse::<u32>().ok());
        match (style_of(style), width) {
            (Some((t, name)), Some(width)) if t == border_type() => {
                Ok(new_border(BorderData { style: name, width }))
            }
            _ => Err(s.lexical_error(ex, prop_text(props, "style").unwrap_or(""), &ui("Border"))),
        }
    }

    fn xdto_type(&self) -> QName {
        ui("Border")
    }
}

impl XdtoCodec for LineCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(l) = v.as_any().downcast_ref::<Line>() else {
            return Err(s.mismatch(ex));
        };
        let style = style_data(s, l.d.ty, l.d.style)?;
        s.account(ex, 128)?;
        Ok(XdtoData::Object {
            ty: ui("Line"),
            props: vec![
                (
                    "width".into(),
                    vec![data_decimal(Number::from_i64(i64::from(l.d.width)))],
                ),
                ("gap".into(), vec![data_bool(l.d.gap)]),
                ("style".into(), vec![style]),
            ],
        })
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let XdtoData::Object { props, .. } = d else {
            return Err(s.lexical_error(ex, "", &ui("Line")));
        };
        let width = prop_text(props, "width").and_then(|w| w.parse::<u32>().ok());
        let gap = prop_text(props, "gap").is_some_and(|g| g == "true" || g == "1");
        match (style_of(super::prop(props, "style")), width) {
            (Some((t, name)), Some(width)) if LINE_ENUMS.contains(&enum_id(t)) => {
                Ok(new_line(LineData {
                    ty: t,
                    style: name,
                    width,
                    gap,
                }))
            }
            _ => Err(s.lexical_error(ex, prop_text(props, "style").unwrap_or(""), &ui("Line"))),
        }
    }

    fn xdto_type(&self) -> QName {
        ui("Line")
    }
}
