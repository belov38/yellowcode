use super::style::{self, item_text, web_text};
use super::{NS_STYLE, NS_UI, NS_WEB, SharedOf, data_text, enum_presentation, enum_val, types};
use crate::bind::{arg_number, msg, value_mismatch};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::serial::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData};
use brace::Node;
use bsl_lib::xdto::QName;
use bsl_prim::{Str, boost_hash};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef, SharedValue,
    TypeId, Value,
};
use platform_catalog::catalog;
use std::any::Any;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ColorKind {
    Abs(u8, u8, u8),
    Web(&'static str),
    Style {
        name: Option<&'static str>,
        id: Option<i32>,
    },
    Item(u32),
    Palette(u8),
    Auto,
}

pub(crate) struct Color {
    pub(crate) kind: ColorKind,
}

const PALETTE_IDS: &[(&str, u8)] = &[
    ("FirstBrand", 0),
    ("SecondBrand", 1),
    ("Red", 2),
    ("Orange", 3),
    ("Yellow", 4),
    ("Green", 5),
    ("LightBlue", 6),
    ("Blue", 7),
    ("Violet", 8),
    ("Purple", 9),
    ("Pink", 10),
    ("Gray", 15),
];

fn palette_id(name: &str) -> Option<u8> {
    PALETTE_IDS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, i)| *i)
}

const STYLE_COLOR_IDS: &[(&str, i32)] = &[
    ("FormTextColor", -3),
    ("ButtonBackColor", -7),
    ("FieldBackColor", -10),
    ("SpecialTextColor", -16),
    ("ToolTipBackColor", -23),
    ("ReportHeaderBackColor", -25),
    ("ReportGroup1BackColor", -26),
    ("ReportGroup2BackColor", -27),
    ("ReportLineColor", -28),
    ("TableHeaderBackColor", -35),
    ("NavigationColor", -42),
];

pub(crate) fn style_color_id(name: &str) -> Option<i32> {
    STYLE_COLOR_IDS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, i)| *i)
}

fn style_color_name(id: i32) -> Option<&'static str> {
    STYLE_COLOR_IDS
        .iter()
        .find(|(_, i)| *i == id)
        .map(|(n, _)| *n)
}

fn names_of(t: platform_catalog::TypeIdx) -> &'static [platform_catalog::EnumValueInfo] {
    &catalog().type_info(t).values
}

fn web_table() -> &'static (Vec<&'static str>, HashMap<&'static str, usize>) {
    static T: OnceLock<(Vec<&'static str>, HashMap<&'static str, usize>)> = OnceLock::new();
    T.get_or_init(|| {
        let mut names: Vec<&'static str> = names_of(types().web_colors)
            .iter()
            .map(|v| v.name_en.as_str())
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        let gray = names.iter().position(|n| *n == "LightGray");
        let green = names.iter().position(|n| *n == "LightGreen");
        if let (Some(a), Some(b)) = (gray, green)
            && b == a + 1
        {
            names.swap(a, b);
        }
        let index = names.iter().enumerate().map(|(i, n)| (*n, i + 1)).collect();
        (names, index)
    })
}

fn web_index(name: &str) -> Option<usize> {
    web_table().1.get(name).copied()
}

fn web_name(index: usize) -> Option<&'static str> {
    web_table().0.get(index.checked_sub(1)?).copied()
}

fn static_name(t: platform_catalog::TypeIdx, name: &str) -> Option<&'static str> {
    names_of(t)
        .iter()
        .map(|v| v.name_en.as_str())
        .find(|n| *n == name)
}

pub(crate) fn new_color(kind: ColorKind) -> Value {
    Value::Object(Rc::new(Color { kind }))
}

pub(crate) fn web_member(t: platform_catalog::TypeIdx, i: usize) -> Option<Value> {
    let name = names_of(t).get(i)?.name_en.as_str();
    Some(new_color(ColorKind::Web(name)))
}

pub(crate) fn style_member(t: platform_catalog::TypeIdx, i: usize) -> Option<Value> {
    if i == 0 {
        return None;
    }
    let name = names_of(t).get(i)?.name_en.as_str();
    Some(new_color(ColorKind::Style {
        name: Some(name),
        id: style_color_id(name),
    }))
}

pub(crate) fn palette_member(t: platform_catalog::TypeIdx, i: usize) -> Option<Value> {
    let name = names_of(t).get(i)?.name_en.as_str();
    palette_id(name).map(|id| new_color(ColorKind::Palette(id)))
}

pub(super) fn item_color(k: u32) -> Value {
    new_color(ColorKind::Item(k))
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    match args.len() {
        0 => Ok(new_color(ColorKind::Auto)),
        3 => {
            let mut c = [0u8; 3];
            for (i, slot) in c.iter_mut().enumerate() {
                let n = arg_number(ex, args.get(i), i + 1)?;
                let v = n.to_i64().filter(|v| (0..=255).contains(v));
                *slot = match v {
                    Some(v) => v as u8,
                    None => return Err(value_mismatch(i + 1).into()),
                };
            }
            Ok(new_color(ColorKind::Abs(c[0], c[1], c[2])))
        }
        _ => Err(ctor_not_implemented(types().color).into()),
    }
}

impl Color {
    fn component(&self, i: usize) -> i64 {
        match self.kind {
            ColorKind::Abs(r, g, b) => i64::from([r, g, b][i]),
            _ => -1,
        }
    }

    fn kind_name(&self) -> &'static str {
        match self.kind {
            ColorKind::Abs(..) => "Absolute",
            ColorKind::Web(_) => "WebColor",
            ColorKind::Style { .. } | ColorKind::Item(_) => "StyleItem",
            ColorKind::Palette(_) => "PaletteColor",
            ColorKind::Auto => "AutoColor",
        }
    }
}

impl HostObject for Color {
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<bool, Exception> {
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().color)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().color;
        match property_name(t, m) {
            Some("Type") => Ok(enum_val(types().color_type, self.kind_name())),
            Some("R") => Ok(Value::from(self.component(0))),
            Some("G") => Ok(Value::from(self.component(1))),
            Some("B") => Ok(Value::from(self.component(2))),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = types().color;
        match property_ru(t, m) {
            "" => Err(not_implemented(t, m).into()),
            ru => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(ru)
                .raw()
                .into()),
        }
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        let t = types().color;
        match (method_name(t, m), &self.kind) {
            (Some("GetAbsolute"), ColorKind::Abs(..)) => Ok(new_color(self.kind.clone())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let text = |s: String| Str::from(s.as_str());
        Ok(match &self.kind {
            ColorKind::Abs(r, g, b) => Some(Str::from(format!("{r}, {g}, {b}").as_str())),
            ColorKind::Web(n) | ColorKind::Style { name: Some(n), .. } => {
                let (t, own) = if matches!(self.kind, ColorKind::Web(_)) {
                    (types().web_colors, web_text(ex, n))
                } else {
                    (types().style_colors, item_text(ex, Some(n), None).map(text))
                };
                own.or_else(|| {
                    names_of(t)
                        .iter()
                        .find(|v| v.name_en == *n)
                        .map(|v| Str::from(v.name_ru.as_str()))
                })
            }
            ColorKind::Palette(id) => types().palette_colors.and_then(|t| {
                let lang = ex.lang();
                let (name, _) = PALETTE_IDS.iter().find(|(_, i)| i == id)?;
                names_of(t)
                    .iter()
                    .find(|v| v.name_en == *name)
                    .and_then(|v| enum_presentation(t, &v.name_ru, &v.name_en, lang))
            }),
            ColorKind::Item(k) => item_text(ex, None, Some(*k)).map(text),
            _ => None,
        })
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(match &self.kind {
            ColorKind::Abs(r, g, b) => {
                boost_hash::ordinal((u32::from(*b) << 16 | u32::from(*g) << 8 | u32::from(*r)) + 1)
            }
            ColorKind::Web(n) => {
                let u: Vec<u16> = n.encode_utf16().collect();
                boost_hash::string(&u)
            }
            ColorKind::Style { id, name } => boost_hash::ordinal(
                id.map_or_else(|| name.map_or(0, |n| n.len() as u32), |i| i.unsigned_abs()),
            ),
            ColorKind::Item(k) => boost_hash::ordinal(k.wrapping_add(1)),
            ColorKind::Palette(id) => boost_hash::ordinal(u32::from(*id)),
            ColorKind::Auto => boost_hash::ordinal(0),
        })
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<Color>()?;
        Some(o.kind == self.kind)
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedOf::new(
            self.kind.clone(),
            |k| new_color(k.clone()),
        ))))
    }
}

pub(super) struct ColorCodec;

impl ColorCodec {
    fn of(v: &ObjRef) -> Option<ColorKind> {
        Some(v.as_any().downcast_ref::<Color>()?.kind.clone())
    }
}

fn unsupported(ex: &mut Execution) -> Exception {
    ex.error(HostError::not_implemented("Цвет", "Сериализация"))
}

impl ValueCodec for ColorCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(kind) = Self::of(v) else {
            return Ok(None);
        };
        let one = |n: i64| vec![Node::int(n)];
        let (k, value, tail) = match kind {
            ColorKind::Abs(r, g, b) => (
                0,
                one((i64::from(b) << 16) | (i64::from(g) << 8) | i64::from(r)),
                0,
            ),
            ColorKind::Web(n) => (2, one(web_index(n).map_or(0, |i| i as i64)), 2),
            ColorKind::Style { id: Some(id), .. } => (3, one(i64::from(id)), 3),
            ColorKind::Style { name, id: None } => match name.and_then(style_color_id) {
                Some(id) => (3, one(i64::from(id)), 3),
                None => return Err(unsupported(ex)),
            },
            ColorKind::Item(item) => match style::item_ref(ex, item) {
                Some(reference) => (3, reference, 3),
                None => return Err(unsupported(ex)),
            },
            ColorKind::Palette(id) => (4, one(i64::from(id)), 5),
            ColorKind::Auto => (4, one(0), 4),
        };
        w.account(ex, 48)?;
        Ok(Some(vec![
            Node::int(2),
            Node::List(vec![
                Node::int(4),
                Node::int(k),
                Node::List(value),
                Node::int(tail),
            ]),
        ]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let Some(body) = items.iter().find_map(Node::as_list) else {
            return Err(r.format_error(ex));
        };
        let kind = body.get(1).and_then(Node::as_i64);
        let inner = body.get(2).and_then(Node::as_list);
        let value = inner.and_then(|l| l.first()).and_then(Node::as_i64);
        let (Some(kind), Some(value)) = (kind, value) else {
            return Err(r.format_error(ex));
        };
        let parsed = match kind {
            0 if (0..=0xFF_FFFF).contains(&value) => Some(ColorKind::Abs(
                (value & 0xFF) as u8,
                ((value >> 8) & 0xFF) as u8,
                ((value >> 16) & 0xFF) as u8,
            )),
            2 => usize::try_from(value)
                .ok()
                .and_then(web_name)
                .map(ColorKind::Web),
            3 if value < 0 && value >= i64::from(i32::MIN) => {
                let id = value as i32;
                Some(ColorKind::Style {
                    name: style_color_name(id),
                    id: Some(id),
                })
            }
            3 if value == 0 => inner
                .and_then(|l| l.get(1))
                .and_then(Node::text)
                .and_then(|uuid| style::item_by_uuid(ex, uuid))
                .map(ColorKind::Item),
            4 if body.get(3).and_then(Node::as_i64) == Some(5) => PALETTE_IDS
                .iter()
                .find(|(_, id)| i64::from(*id) == value)
                .map(|(_, id)| ColorKind::Palette(*id)),
            4 => Some(ColorKind::Auto),
            _ => None,
        };
        match parsed {
            Some(k) => Ok(new_color(k)),
            None => Err(r.format_error(ex)),
        }
    }
}

fn color_type() -> QName {
    QName::new(NS_UI, "Color")
}

impl XdtoCodec for ColorCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(kind) = Self::of(v) else {
            return Err(s.mismatch(ex));
        };
        let text = match kind {
            ColorKind::Abs(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
            ColorKind::Auto => "auto".to_string(),
            ColorKind::Web(n) => QName::new(NS_WEB, n).clark(),
            ColorKind::Style { name, id } => {
                let name = name.or_else(|| id.and_then(style_color_name));
                match name {
                    Some(n) => QName::new(NS_STYLE, n).clark(),
                    None => {
                        return Err(s.fail(HostError::not_implemented("Цвет", "Сериализация")));
                    }
                }
            }
            ColorKind::Item(_) | ColorKind::Palette(_) => {
                return Err(s.fail(HostError::not_implemented("Цвет", "Сериализация")));
            }
        };
        s.account(ex, text.len() as u64)?;
        Ok(data_text(color_type(), &text))
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let XdtoData::Value { lexical, .. } = d else {
            return Err(s.lexical_error(ex, "", &color_type()));
        };
        let t = lexical.trim();
        let kind = if let Some(hex) = t.strip_prefix('#')
            && hex.len() == 6
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            let n = u32::from_str_radix(hex, 16).unwrap_or(0);
            Some(ColorKind::Abs((n >> 16) as u8, (n >> 8) as u8, n as u8))
        } else if t == "auto" {
            Some(ColorKind::Auto)
        } else if let Some((ns, local)) = t.strip_prefix('{').and_then(|r| r.split_once('}')) {
            match ns {
                NS_WEB => static_name(types().web_colors, local).map(ColorKind::Web),
                NS_STYLE => static_name(types().style_colors, local).map(|n| ColorKind::Style {
                    name: Some(n),
                    id: style_color_id(n),
                }),
                _ => None,
            }
        } else {
            None
        };
        match kind {
            Some(k) => Ok(new_color(k)),
            None => Err(s.lexical_error(ex, lexical, &color_type())),
        }
    }

    fn xdto_type(&self) -> QName {
        color_type()
    }
}
