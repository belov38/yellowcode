use super::{
    NS_STYLE, NS_UI, SharedOf, data_bool, data_decimal, data_text, enum_val, prop, prop_text,
    tenths, to_tenths, types,
};
use crate::bind::{arg_value, msg, type_mismatch};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::serial::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::serial::internal::text::{decode_str, encode_str};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData};
use brace::Node;
use bsl_lib::xdto::QName;
use bsl_prim::{Number, Str, boost_hash};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef, SharedValue,
    TypeId, Value,
};
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontKind {
    Absolute,
    Style,
    Auto,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StyleRef {
    name: Option<&'static str>,
    id: Option<i32>,
}

const STYLE_FONT_IDS: &[(&str, i32)] = &[("NormalTextFont", -31)];

fn style_font_id(name: &str) -> Option<i32> {
    STYLE_FONT_IDS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, i)| *i)
}

fn style_font_name(id: i32) -> Option<&'static str> {
    STYLE_FONT_IDS
        .iter()
        .find(|(_, i)| *i == id)
        .map(|(n, _)| *n)
}

fn style_of(name: &'static str) -> StyleRef {
    StyleRef {
        name: Some(name),
        id: style_font_id(name),
    }
}

fn normal_text_font() -> StyleRef {
    style_of("NormalTextFont")
}

const BIT_NAME: u8 = 1;
const BIT_SIZE: u8 = 2;
const BIT_BOLD: u8 = 4;
const BIT_ITALIC: u8 = 8;
const BIT_UNDERLINE: u8 = 16;
const BIT_STRIKE: u8 = 32;

#[derive(Clone, Debug)]
pub(crate) struct FontData {
    kind: FontKind,
    style: Option<StyleRef>,
    face: Option<String>,
    size: Option<i64>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    scale: i64,
}

#[derive(PartialEq)]
struct Effective {
    kind: FontKind,
    style: Option<StyleRef>,
    face: String,
    size: Option<i64>,
    flags: [Option<bool>; 4],
    scale: i64,
}

impl FontData {
    fn auto() -> FontData {
        FontData {
            kind: FontKind::Auto,
            style: None,
            face: None,
            size: None,
            bold: None,
            italic: None,
            underline: None,
            strike: None,
            scale: 100,
        }
    }

    fn effective(&self) -> Effective {
        let flags = [self.bold, self.italic, self.underline, self.strike];
        if self.kind == FontKind::Absolute {
            Effective {
                kind: self.kind,
                style: None,
                face: self.face.clone().unwrap_or_default(),
                size: Some(self.size.unwrap_or(0)),
                flags: flags.map(|f| Some(f.unwrap_or(false))),
                scale: self.scale,
            }
        } else {
            Effective {
                kind: self.kind,
                style: self.style.clone(),
                face: self.face.clone().unwrap_or_default(),
                size: self.size,
                flags,
                scale: self.scale,
            }
        }
    }

    fn mask(&self) -> u8 {
        let bit = |c: bool, b: u8| if c { b } else { 0 };
        bit(self.face.is_some(), BIT_NAME)
            | bit(self.size.is_some(), BIT_SIZE)
            | bit(self.bold.is_some(), BIT_BOLD)
            | bit(self.italic.is_some(), BIT_ITALIC)
            | bit(self.underline.is_some(), BIT_UNDERLINE)
            | bit(self.strike.is_some(), BIT_STRIKE)
    }

    fn kind_name(&self) -> &'static str {
        match self.kind {
            FontKind::Absolute => "Absolute",
            FontKind::Style => "StyleItem",
            FontKind::Auto => "AutoFont",
        }
    }
}

pub(crate) struct Font {
    pub(crate) data: FontData,
}

pub(crate) fn new_font(data: FontData) -> Value {
    Value::Object(Rc::new(Font { data }))
}

fn font_of(v: &Value) -> Option<&Font> {
    v.as_object()?.as_any().downcast_ref::<Font>()
}

pub(super) fn comparison_resources(value: &Value) -> Option<(u64, u64)> {
    let data = font_data(value)?;
    let len = data.face.as_ref().map_or(0, String::len) as u64;
    Some((len / 64 + 1, len.saturating_mul(2).saturating_add(128)))
}

pub(super) fn presentation_input_len(value: &Value) -> Option<u64> {
    let data = font_data(value)?;
    Some(if data.kind == FontKind::Absolute {
        (data.face.as_ref().map_or(0, String::len) as u64).saturating_add(512)
    } else {
        0
    })
}

pub(crate) fn font_data(v: &Value) -> Option<&FontData> {
    font_of(v).map(|f| &f.data)
}

impl FontData {
    pub(crate) fn normal_bold_only(&self) -> Option<i32> {
        let ok = self.kind == FontKind::Style
            && self.face.is_none()
            && self.size.is_none()
            && self.bold == Some(true)
            && self.italic.is_none()
            && self.underline.is_none()
            && self.strike.is_none()
            && self.scale == 100;
        let st = self.style.as_ref()?;
        if ok && st.name == Some("NormalTextFont") {
            st.id
        } else {
            None
        }
    }
}

pub(crate) fn style_member(t: TypeIdx, i: usize) -> Option<Value> {
    if i == 0 {
        return None;
    }
    let name = catalog().type_info(t).values.get(i)?.name_en.as_str();
    let mut d = FontData::auto();
    d.kind = FontKind::Style;
    d.style = Some(style_of(name));
    Some(new_font(d))
}

fn opt_bool(ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<Option<bool>> {
    match arg_value(ex, a)? {
        Value::Undefined => Ok(None),
        Value::Bool(b) => Ok(Some(b)),
        _ => Err(type_mismatch(param).into()),
    }
}

fn opt_number(ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<Option<Number>> {
    match arg_value(ex, a)? {
        Value::Undefined => Ok(None),
        Value::Number(n) => Ok(Some(n)),
        _ => Err(type_mismatch(param).into()),
    }
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.is_empty() {
        return Ok(new_font(FontData::auto()));
    }
    let first = arg_value(ex, args.first())?;
    let (base, off) = match font_of(&first) {
        Some(f) => (Some(f.data.clone()), 1),
        None => (None, 0),
    };
    if args.len() - off > 7 {
        return Err(ctor_not_implemented(types().font).into());
    }
    let name = match arg_value(ex, args.get(off))? {
        Value::Undefined => None,
        Value::String(s) => Some(s.to_string_lossy()),
        _ => return Err(type_mismatch(off + 1).into()),
    };
    let size = opt_number(ex, args.get(off + 1), off + 2)?.map(|n| to_tenths(&n));
    let bold = opt_bool(ex, args.get(off + 2), off + 3)?;
    let italic = opt_bool(ex, args.get(off + 3), off + 4)?;
    let underline = opt_bool(ex, args.get(off + 4), off + 5)?;
    let strike = opt_bool(ex, args.get(off + 5), off + 6)?;
    let scale = opt_number(ex, args.get(off + 6), off + 7)?.map(|n| n.trunc_to_i64_saturating());
    let given = name.is_some()
        || size.is_some()
        || bold.is_some()
        || italic.is_some()
        || underline.is_some()
        || strike.is_some()
        || scale.is_some();
    let mut d = match base {
        Some(b) if b.kind == FontKind::Auto => FontData {
            kind: FontKind::Style,
            style: Some(normal_text_font()),
            ..b
        },
        Some(b) => b,
        None if name.is_some() => FontData {
            kind: FontKind::Absolute,
            ..FontData::auto()
        },
        None if given => FontData {
            kind: FontKind::Style,
            style: Some(normal_text_font()),
            ..FontData::auto()
        },
        None => FontData::auto(),
    };
    d.face = name.or(d.face);
    d.size = size.or(d.size);
    d.bold = bold.or(d.bold);
    d.italic = italic.or(d.italic);
    d.underline = underline.or(d.underline);
    d.strike = strike.or(d.strike);
    d.scale = scale.unwrap_or(d.scale);
    Ok(new_font(d))
}

const BOLD_ALIAS: MemberId = MemberId(10_000);

impl Font {
    fn flag(&self, f: Option<bool>) -> Value {
        match (f, self.data.kind) {
            (Some(b), _) => Value::Bool(b),
            (None, FontKind::Absolute) => Value::Bool(false),
            (None, _) => Value::Undefined,
        }
    }

    fn bold(&self) -> Value {
        self.flag(self.data.bold)
    }
}

impl HostObject for Font {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().font)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().font;
        if m == BOLD_ALIAS {
            return Ok(self.bold());
        }
        let d = &self.data;
        match property_name(t, m) {
            Some("Type") => Ok(enum_val(types().font_type, d.kind_name())),
            Some("Name") => Ok(Value::from(d.face.as_deref().unwrap_or(""))),
            Some("Size") => Ok(match (d.size, d.kind) {
                (Some(s), _) => Value::Number(tenths(s)),
                (None, FontKind::Absolute) => Value::from(0i64),
                (None, _) => Value::from(-1i64),
            }),
            Some("Bold") => Ok(self.bold()),
            Some("Italic") => Ok(self.flag(d.italic)),
            Some("Underline") => Ok(self.flag(d.underline)),
            Some("Strikeout") => Ok(self.flag(d.strike)),
            Some("Scale") => Ok(Value::from(d.scale)),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = types().font;
        let ru = if m == BOLD_ALIAS {
            "Жирный"
        } else {
            property_ru(t, m)
        };
        match ru {
            "" => Err(not_implemented(t, m).into()),
            ru => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(ru)
                .raw()
                .into()),
        }
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        let t = types().font;
        match (method_name(t, m), self.data.kind) {
            (Some("GetAbsolute"), FontKind::Absolute) => Ok(new_font(self.data.clone())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let n = name.to_lowercase();
        (n == "жирный" || n == "bold").then_some(BOLD_ALIAS)
    }

    fn member_kind(&self, m: MemberId) -> Option<bsl_vm::MemberKind> {
        (m == BOLD_ALIAS).then_some(bsl_vm::MemberKind::Property {
            readable: true,
            writable: false,
        })
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let d = &self.data;
        if d.kind != FontKind::Absolute {
            return Ok(None);
        }
        let lang = ex.lang();
        let mut parts: Vec<String> = Vec::new();
        if let Some(f) = d.face.as_deref().filter(|f| !f.is_empty()) {
            parts.push(f.to_string());
        }
        if let Some(s) = d.size {
            parts.push(tenths(s).to_xml_string());
        }
        for (flag, key) in [
            (d.bold, msg::FONT_BOLD),
            (d.italic, msg::FONT_ITALIC),
            (d.underline, msg::FONT_UNDERLINE),
            (d.strike, msg::FONT_STRIKEOUT),
        ] {
            if flag == Some(true) {
                parts.push(
                    platform_catalog::messages::render_key(key, &[], "", lang).unwrap_or_default(),
                );
            }
        }
        Ok(Some(Str::from(parts.join(", ").as_str())))
    }

    fn boost_hash(&self) -> Option<i32> {
        let e = self.data.effective();
        let u: Vec<u16> = e.face.encode_utf16().collect();
        Some(boost_hash::string(&u) ^ boost_hash::ordinal(e.scale.unsigned_abs() as u32))
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<Font>()?;
        Some(o.data.effective() == self.data.effective())
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedOf::new(
            self.data.clone(),
            |d| new_font(d.clone()),
        ))))
    }
}

pub(super) struct FontCodec;

fn unsupported(ex: &mut Execution) -> Exception {
    ex.error(HostError::not_implemented("Шрифт", "Сериализация"))
}

fn font_node_str(s: &str) -> Node {
    let u: Vec<u16> = s.encode_utf16().collect();
    encode_str(&u)
}

impl ValueCodec for FontCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(f) = v.as_any().downcast_ref::<Font>() else {
            return Ok(None);
        };
        let d = &f.data;
        let int = |n: i64| Node::int(n);
        let flag = |b: Option<bool>| Node::int(i64::from(b == Some(true)));
        let weight = |b: Option<bool>| Node::int(if b == Some(true) { 700 } else { 400 });
        let body = match d.kind {
            FontKind::Absolute => vec![
                int(8),
                int(0),
                int(i64::from(d.mask())),
                int(d.size.unwrap_or(0)),
                int(0),
                int(0),
                int(0),
                weight(d.bold),
                flag(d.italic),
                flag(d.underline),
                flag(d.strike),
                int(0),
                int(0),
                int(0),
                int(0),
                int(0),
                font_node_str(d.face.as_deref().unwrap_or("")),
                int(1),
                int(d.scale),
                int(0),
            ],
            FontKind::Auto => vec![int(8), int(3), int(0), int(1), int(d.scale)],
            FontKind::Style => {
                let id = d
                    .style
                    .as_ref()
                    .and_then(|s| s.id.or_else(|| s.name.and_then(style_font_id)));
                let Some(id) = id else {
                    return Err(unsupported(ex));
                };
                if d.face.is_some()
                    || d.italic.is_some()
                    || d.underline.is_some()
                    || d.strike.is_some()
                {
                    return Err(unsupported(ex));
                }
                let mut b = vec![
                    int(8),
                    int(2),
                    int(i64::from(d.mask())),
                    Node::List(vec![int(i64::from(id))]),
                ];
                if let Some(s) = d.size {
                    b.push(int(s));
                }
                if d.bold.is_some() {
                    b.push(weight(d.bold));
                }
                b.push(int(1));
                b.push(int(d.scale));
                b
            }
        };
        w.account(ex, 160)?;
        Ok(Some(vec![int(1), Node::List(body), int(0)]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        match read_internal(items) {
            Some(d) => Ok(new_font(d)),
            None => Err(r.format_error(ex)),
        }
    }
}

fn read_internal(items: &[Node]) -> Option<FontData> {
    let b = items.iter().find_map(Node::as_list)?;
    let int = |i: usize| b.get(i).and_then(Node::as_i64);
    if int(0)? != 8 {
        return None;
    }
    let kind = int(1)?;
    let mask = u8::try_from(int(2)?).ok()?;
    let has = |bit: u8| mask & bit != 0;
    let mut d = FontData::auto();
    match kind {
        0 => {
            if b.len() < 19 {
                return None;
            }
            d.kind = FontKind::Absolute;
            let flag = |i: usize, bit: u8, on: i64| has(bit).then(|| int(i).map(|v| v == on));
            d.size = has(BIT_SIZE).then(|| int(3)).flatten();
            d.bold = flag(7, BIT_BOLD, 700).flatten();
            d.italic = flag(8, BIT_ITALIC, 1).flatten();
            d.underline = flag(9, BIT_UNDERLINE, 1).flatten();
            d.strike = flag(10, BIT_STRIKE, 1).flatten();
            d.face = has(BIT_NAME)
                .then(|| b.get(16).and_then(decode_str))
                .flatten()
                .map(|u| String::from_utf16_lossy(&u));
            d.scale = int(18)?;
        }
        2 => {
            d.kind = FontKind::Style;
            let id = i32::try_from(
                b.get(3)
                    .and_then(Node::as_list)
                    .and_then(|l| l.first())
                    .and_then(Node::as_i64)?,
            )
            .ok()?;
            d.style = Some(StyleRef {
                name: style_font_name(id),
                id: Some(id),
            });
            if mask & !(BIT_SIZE | BIT_BOLD) != 0 {
                return None;
            }
            let mut at = 4;
            if has(BIT_SIZE) {
                d.size = Some(int(at)?);
                at += 1;
            }
            if has(BIT_BOLD) {
                d.bold = Some(int(at)? >= 700);
                at += 1;
            }
            d.scale = int(at + 1)?;
        }
        3 => {
            d.scale = int(4)?;
        }
        _ => return None,
    }
    Some(d)
}

fn ui(name: &str) -> QName {
    QName::new(NS_UI, name)
}

impl XdtoCodec for FontCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(f) = v.as_any().downcast_ref::<Font>() else {
            return Err(s.mismatch(ex));
        };
        let d = &f.data;
        let mut props: Vec<(String, Vec<XdtoData>)> = Vec::new();
        let mut put = |name: &str, data: XdtoData| props.push((name.to_string(), vec![data]));
        let abs = d.kind == FontKind::Absolute;
        if d.kind == FontKind::Style {
            let name = d
                .style
                .as_ref()
                .and_then(|s| s.name.or_else(|| s.id.and_then(style_font_name)));
            let Some(name) = name else {
                return Err(s.fail(HostError::not_implemented("Шрифт", "Сериализация")));
            };
            put(
                "ref",
                data_text(ui("StyleRef"), &QName::new(NS_STYLE, name).clark()),
            );
        }
        if abs || d.face.is_some() {
            put(
                "faceName",
                data_text(
                    QName::new(crate::xdto::XS, "string"),
                    d.face.as_deref().unwrap_or(""),
                ),
            );
        }
        if abs || d.size.is_some() {
            put("height", data_decimal(tenths(d.size.unwrap_or(0))));
        }
        for (name, f) in [
            ("bold", d.bold),
            ("italic", d.italic),
            ("underline", d.underline),
            ("strikeout", d.strike),
        ] {
            if abs || f.is_some() {
                put(name, data_bool(f == Some(true)));
            }
        }
        put("kind", data_text(ui("FontType"), d.kind_name()));
        if abs || d.scale != 100 {
            put("scale", data_decimal(Number::from_i64(d.scale)));
        }
        s.account(ex, 256)?;
        Ok(XdtoData::Object {
            ty: ui("Font"),
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
            return Err(s.lexical_error(ex, "", &ui("Font")));
        };
        let text = |n: &str| prop_text(props, n);
        let flag = |n: &str| text(n).map(|t| t == "true" || t == "1");
        let kind = text("kind").unwrap_or(if prop(props, "ref").is_some() {
            "StyleItem"
        } else {
            "Absolute"
        });
        let mut out = FontData::auto();
        out.kind = match kind {
            "Absolute" => FontKind::Absolute,
            "StyleItem" => FontKind::Style,
            "AutoFont" => FontKind::Auto,
            other => return Err(s.lexical_error(ex, other, &ui("FontType"))),
        };
        if out.kind == FontKind::Style {
            let r = text("ref").unwrap_or("");
            let name = r
                .strip_prefix('{')
                .and_then(|x| x.split_once('}'))
                .filter(|(ns, _)| *ns == NS_STYLE)
                .and_then(|(_, local)| {
                    catalog()
                        .type_info(types().style_fonts)
                        .values
                        .iter()
                        .map(|v| v.name_en.as_str())
                        .find(|n| *n == local)
                });
            match name {
                Some(n) => out.style = Some(style_of(n)),
                None => return Err(s.lexical_error(ex, r, &ui("StyleRef"))),
            }
        }
        out.face = text("faceName").map(str::to_string);
        for (slot, name) in [
            (&mut out.bold, "bold"),
            (&mut out.italic, "italic"),
            (&mut out.underline, "underline"),
            (&mut out.strike, "strikeout"),
        ] {
            *slot = flag(name);
        }
        if let Some(h) = text("height") {
            match Number::parse(h) {
                Some(n) => out.size = Some(to_tenths(&n)),
                None => return Err(s.lexical_error(ex, h, &ui("Font"))),
            }
        }
        if let Some(sc) = text("scale") {
            match Number::parse(sc) {
                Some(n) => out.scale = n.trunc_to_i64_saturating(),
                None => return Err(s.lexical_error(ex, sc, &ui("Font"))),
            }
        }
        Ok(new_font(out))
    }

    fn xdto_type(&self) -> QName {
        ui("Font")
    }
}
