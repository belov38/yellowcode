mod color;
mod font;
mod formatted;
mod frame;
mod picture;

use crate::enums::{enum_value, manager, type_by_id};
use crate::serial::codec::ValueCodec;
use crate::xdto::{XdtoCodec, XdtoData};
use bsl_lib::xdto::{QName, TypeDef, platform_model};
use bsl_prim::{Number, Str};
use bsl_vm::{Registry, TypeImpl, Value};
use platform_catalog::{TypeIdx, catalog, messages::Lang};
use std::sync::{Arc, OnceLock};

pub(crate) const NS_UI: &str = "http://v8.1c.ru/8.1/data/ui";
pub(crate) const NS_STYLE: &str = "http://v8.1c.ru/8.1/data/ui/style";
pub(crate) const NS_WEB: &str = "http://v8.1c.ru/8.1/data/ui/colors/web";

pub(crate) struct Types {
    pub color: TypeIdx,
    pub font: TypeIdx,
    pub picture: TypeIdx,
    pub formatted: TypeIdx,
    pub border: TypeIdx,
    pub line: TypeIdx,
    pub color_type: TypeIdx,
    pub font_type: TypeIdx,
    pub picture_type: TypeIdx,
    pub picture_format: TypeIdx,
    pub border_kind: TypeIdx,
    pub web_colors: TypeIdx,
    pub style_colors: TypeIdx,
    pub style_fonts: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
pub(crate) fn types() -> &'static Types {
    static T: OnceLock<Types> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        Types {
            color: t("Color"),
            font: t("Font"),
            picture: t("Picture"),
            formatted: t("FormattedString"),
            border: t("Border"),
            line: t("Line"),
            color_type: t("ColorType"),
            font_type: t("FontType"),
            picture_type: t("PictureType"),
            picture_format: t("PictureFormat"),
            border_kind: t("BorderType"),
            web_colors: t("WebColors"),
            style_colors: t("StyleColors"),
            style_fonts: t("StyleFonts"),
        }
    })
}

pub(crate) fn register(reg: &mut Registry) {
    let t = types();
    let ctor =
        |f: fn(&mut bsl_vm::Execution, &mut [bsl_vm::Arg]) -> bsl_vm::CallResult<Value>| TypeImpl {
            ctor: Some(Arc::new(f)),
        };
    reg.type_impl(t.color, ctor(color::ctor));
    reg.type_impl(t.font, ctor(font::ctor));
    reg.type_impl(t.picture, ctor(picture::ctor));
    reg.type_impl(t.formatted, ctor(formatted::ctor));
    reg.type_impl(t.border, ctor(frame::border_ctor));
    reg.type_impl(t.line, ctor(frame::line_ctor));
    reg.system_enum(t.web_colors, manager(t.web_colors, color::web_member));
    reg.system_enum(t.style_colors, manager(t.style_colors, color::style_member));
    reg.system_enum(t.style_fonts, manager(t.style_fonts, font::style_member));
}

pub(crate) fn comparison_resources(value: &Value) -> Option<(u64, u64)> {
    font::comparison_resources(value)
        .or_else(|| picture::comparison_resources(value))
        .or_else(|| formatted::presentation_input_len(value).map(|len| (len / 64 + 1, 0)))
}

pub(crate) fn presentation_input_len(value: &Value) -> Option<u64> {
    font::presentation_input_len(value).or_else(|| formatted::presentation_input_len(value))
}

pub(crate) struct SharedOf<T: Send + Sync + 'static> {
    data: T,
    make: fn(&T) -> Value,
}

impl<T: Send + Sync + 'static> SharedOf<T> {
    pub(crate) fn new(data: T, make: fn(&T) -> Value) -> SharedOf<T> {
        SharedOf { data, make }
    }
}

impl<T: Send + Sync + 'static> bsl_vm::SharedObject for SharedOf<T> {
    fn to_value(&self, _ex: &mut bsl_vm::Execution) -> Result<Value, bsl_vm::Exception> {
        Ok((self.make)(&self.data))
    }
}

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    let t = types();
    vec![
        (t.color, Arc::new(color::ColorCodec) as Arc<dyn ValueCodec>),
        (t.font, Arc::new(font::FontCodec)),
        (t.picture, Arc::new(picture::PictureCodec)),
        (t.formatted, Arc::new(formatted::FormattedCodec)),
        (t.border, Arc::new(frame::BorderCodec)),
        (t.line, Arc::new(frame::LineCodec)),
    ]
}

pub(crate) fn xdto_codecs() -> Vec<(TypeIdx, Arc<dyn XdtoCodec>)> {
    let t = types();
    vec![
        (t.color, Arc::new(color::ColorCodec) as Arc<dyn XdtoCodec>),
        (t.font, Arc::new(font::FontCodec)),
        (t.picture, Arc::new(picture::PictureCodec)),
        (t.formatted, Arc::new(formatted::FormattedCodec)),
        (t.border, Arc::new(frame::BorderCodec)),
        (t.line, Arc::new(frame::LineCodec)),
    ]
}

pub(crate) fn enum_val(t: TypeIdx, name_en: &str) -> Value {
    enum_value(t, name_en).unwrap_or(Value::Undefined)
}

pub(crate) fn model_names(q: &QName) -> &'static [String] {
    match platform_model().type_def(q) {
        Some(TypeDef::Value(v)) => &v.facets.enumeration,
        _ => &[],
    }
}

pub(crate) fn model_ordinal(q: &QName, name: &str) -> Option<usize> {
    model_names(q).iter().position(|n| n == name)
}

pub(crate) fn enum_xdto(t: TypeIdx) -> Option<QName> {
    let x = catalog().type_info(t).xdto.as_ref()?;
    Some(QName::new(&x.namespace, &x.name))
}

pub(crate) fn enum_by_xdto(q: &QName) -> Option<TypeIdx> {
    let c = catalog();
    c.types().iter().enumerate().find_map(|(i, info)| {
        let x = info.xdto.as_ref()?;
        (info.kind == platform_catalog::TypeKind::Enum && x.namespace == q.ns && x.name == q.name)
            .then(|| u16::try_from(i).ok().map(TypeIdx))
            .flatten()
    })
}

pub(crate) fn enum_presentation(
    t: TypeIdx,
    name_ru: &str,
    name_en: &str,
    lang: Lang,
) -> Option<Str> {
    let ty = types();
    if ![
        ty.color_type,
        ty.font_type,
        ty.picture_type,
        ty.picture_format,
    ]
    .contains(&t)
    {
        return None;
    }
    let name = if lang == Lang::En { name_en } else { name_ru };
    let mut out = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_uppercase() && prev_lower {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
        prev_lower = c.is_lowercase();
    }
    Some(Str::from(out.as_str()))
}

fn xs(name: &str) -> QName {
    QName::new(crate::xdto::XS, name)
}

pub(crate) fn data_bool(b: bool) -> XdtoData {
    XdtoData::Value {
        projection: None,
        ty: xs("boolean"),
        lexical: b.to_string(),
        value: Value::Bool(b),
    }
}

pub(crate) fn data_decimal(n: Number) -> XdtoData {
    XdtoData::Value {
        projection: None,
        ty: xs("decimal"),
        lexical: n.to_xml_string(),
        value: Value::Number(n),
    }
}

pub(crate) fn data_text(ty: QName, lexical: &str) -> XdtoData {
    XdtoData::Value {
        projection: None,
        ty,
        lexical: lexical.to_string(),
        value: Value::from(lexical),
    }
}

pub(crate) fn prop<'a>(props: &'a [(String, Vec<XdtoData>)], name: &str) -> Option<&'a XdtoData> {
    props
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, v)| v.first())
}

pub(crate) fn prop_text<'a>(props: &'a [(String, Vec<XdtoData>)], name: &str) -> Option<&'a str> {
    match prop(props, name)? {
        XdtoData::Value { lexical, .. } => Some(lexical.as_str()),
        _ => None,
    }
}

pub(crate) fn tenths(t: i64) -> Number {
    if t % 10 == 0 {
        Number::from_i64(t / 10)
    } else {
        let sign = if t < 0 { "-" } else { "" };
        let a = t.abs();
        Number::parse(&format!("{sign}{}.{}", a / 10, a % 10))
            .unwrap_or_else(|| Number::from_i64(0))
    }
}

pub(crate) fn to_tenths(n: &Number) -> i64 {
    (n.to_f64() * 10.0).round() as i64
}
