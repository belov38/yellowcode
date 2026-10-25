use super::tz::{session_offset_of_local, utc_to_session};
use super::{enum_arg, enum_of, int_value, reserve_units, str_value};
use crate::bind::{
    arg_bool, arg_date, arg_i64, arg_str, arg_value, global, is_given, lib_error, type_mismatch,
    with_other,
};
use crate::collections::array_items;
use crate::values::{binary_data_bytes, new_binary_data};
use bsl_lib::codec::{self, Encoding, EncodingName};
use bsl_lib::json::{DateFormat, DateVariant, read_date, write_date};
use bsl_lib::url::{self, UrlMode};
use bsl_lib::{b64, hex, xml};
use bsl_prim::Number;
use bsl_vm::{Arg, CallResult, Execution, Registry, Value};

type Args<'a> = &'a mut [Arg];

pub(super) fn register(reg: &mut Registry) {
    for (ru, en) in [
        ("Base64Строка", "Base64String"),
        (
            "ПолучитьBase64СтрокуИзДвоичныхДанных",
            "GetBase64StringFromBinaryData",
        ),
    ] {
        global(reg, ru, en, |ex, a| {
            let v = arg_value(ex, a.first())?;
            let Some(b) = binary_data_bytes(&v).map(<[u8]>::to_vec) else {
                return Ok(Value::Undefined);
            };
            reserve_units(ex, b.len(), 3)?;
            Ok(Value::from(
                crate::serial::xmlvalue::wrapped_base64(&b).as_str(),
            ))
        });
    }
    for (ru, en) in [
        ("Base64Значение", "Base64Value"),
        (
            "ПолучитьДвоичныеДанныеИзBase64Строки",
            "GetBinaryDataFromBase64String",
        ),
    ] {
        global(reg, ru, en, |ex, a| {
            let s = arg_str(ex, a.first())?;
            Ok(match b64::base64_value(s.units()) {
                Some(b) => new_binary_data(b),
                None => Value::Undefined,
            })
        });
    }
    global(
        reg,
        "ПолучитьДвоичныеДанныеИзСтроки",
        "GetBinaryDataFromString",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            let enc = encoding_arg(ex, a.get(1), 2)?.unwrap_or(Encoding::Utf8);
            let bom = if is_given(a.get(2)) {
                arg_bool(ex, a.get(2))?
            } else {
                false
            };
            reserve_units(ex, s.len(), 2)?;
            Ok(new_binary_data(codec::string_to_bytes(s.units(), enc, bom)))
        },
    );
    global(
        reg,
        "ПолучитьСтрокуИзДвоичныхДанных",
        "GetStringFromBinaryData",
        |ex, a| {
            let b = binary_arg(ex, a.first(), 1)?;
            let enc = encoding_arg(ex, a.get(1), 2)?.unwrap_or(Encoding::Utf8);
            reserve_units(ex, b.len(), 1)?;
            Ok(str_value(codec::bytes_to_string(&b, enc)))
        },
    );
    global(
        reg,
        "ПолучитьHexСтрокуИзДвоичныхДанных",
        "GetHexStringFromBinaryData",
        |ex, a| {
            let b = binary_arg(ex, a.first(), 1)?;
            reserve_units(ex, b.len(), 2)?;
            Ok(Value::from(hex::hex_string(&b).as_str()))
        },
    );
    global(
        reg,
        "ПолучитьДвоичныеДанныеИзHexСтроки",
        "GetBinaryDataFromHexString",
        |ex, a| Ok(new_binary_data(hex_arg(ex, a.first())?)),
    );
    global(
        reg,
        "СоединитьДвоичныеДанные",
        "ConcatBinaryData",
        concat_binary,
    );
    global(reg, "КодироватьСтроку", "EncodeString", encode_string);
    global(reg, "РаскодироватьСтроку", "DecodeString", decode_string);
    global(
        reg,
        "НайтиНедопустимыеСимволыXML",
        "FindDisallowedXMLCharacters",
        find_disallowed,
    );
    global(
        reg,
        "УдалитьНедопустимыеСимволыXML",
        "DeleteDisallowedXMLCharacters",
        |ex, a| {
            let s = arg_str(ex, a.first())?;
            let v11 = xml_version(ex, a.get(1))?;
            Ok(str_value(xml::xml_remove_invalid(s.units(), v11)))
        },
    );
    global(reg, "ЗаписатьДатуJSON", "WriteJSONDate", write_json_date);
    global(reg, "ПрочитатьДатуJSON", "ReadJSONDate", read_json_date);
}

pub(crate) fn hex_arg(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Vec<u8>> {
    let Value::String(s) = arg_value(ex, a)? else {
        return Err(with_other(type_mismatch(1), true).into());
    };
    let is_digit = |u: &u16| char::from_u32(u32::from(*u)).is_some_and(|c| c.is_ascii_hexdigit());
    let n = s.units().iter().take_while(|u| is_digit(u)).count();
    let digits = &s.units()[..n];
    hex::hex_bytes(digits).map_err(|e| with_other(lib_error(e), false).into())
}

pub(super) fn binary_arg(ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<Vec<u8>> {
    let v = arg_value(ex, a)?;
    match binary_data_bytes(&v) {
        Some(b) => Ok(b.to_vec()),
        None => Err(with_other(type_mismatch(param), true).into()),
    }
}

pub(super) fn encoding_arg(
    ex: &mut Execution,
    a: Option<&Arg>,
    param: usize,
) -> CallResult<Option<Encoding>> {
    let v = arg_value(ex, a)?;
    Ok(encoding_value(ex, &v, param)?.map(|(e, _)| e))
}

fn encoding_value(ex: &Execution, v: &Value, param: usize) -> CallResult<Option<(Encoding, bool)>> {
    if let Value::String(s) = v {
        let r = codec::resolve(EncodingName::Named(s.units()), &PAGES).map_err(lib_error)?;
        return Ok(Some(r));
    }
    let Some(name) = enum_of(v, "КодировкаТекста", param)? else {
        return Ok(None);
    };
    let r = match name {
        "UTF8" => codec::resolve(EncodingName::Utf8, &PAGES),
        "UTF16" => codec::resolve(EncodingName::Utf16, &PAGES),
        _ => {
            let pages = ex.env().code_pages();
            let label = match name {
                "ANSI" => pages[0],
                "OEM" => pages[1],
                _ => pages[2],
            };
            let units: Vec<u16> = label.encode_utf16().collect();
            codec::resolve(EncodingName::Named(&units), &PAGES)
        }
    }
    .map_err(lib_error)?;
    Ok(Some(r))
}

pub fn encoding_from_value(
    ex: &mut Execution,
    v: &Value,
    param: usize,
) -> CallResult<(Encoding, bool)> {
    match encoding_value(ex, v, param)? {
        Some(r) => Ok(r),
        None => codec::resolve(EncodingName::Utf8, &PAGES)
            .map_err(lib_error)
            .map_err(Into::into),
    }
}

const PAGES: codec::CodePages = codec::CodePages::STAND;

fn concat_binary(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let v = arg_value(ex, a.first())?;
    let Some(items) = array_items(&v) else {
        return Err(type_mismatch(1).into());
    };
    let mut parts: Vec<&[u8]> = Vec::with_capacity(items.len());
    let mut total = 0usize;
    for it in &items {
        let Some(b) = binary_data_bytes(it) else {
            return Err(type_mismatch(1).into());
        };
        total = total.saturating_add(b.len());
        parts.push(b);
    }
    reserve_units(ex, total, 1)?;
    Ok(new_binary_data(parts.concat()))
}

fn url_mode(ex: &mut Execution, a: Option<&Arg>) -> CallResult<UrlMode> {
    match enum_arg(ex, a, "СпособКодированияСтроки", 2)? {
        Some("URLВКодировкеURL") => Ok(UrlMode::UrlInUrl),
        Some(_) => Ok(UrlMode::Url),
        None => Err(type_mismatch(2).into()),
    }
}

fn encode_string(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    let mode = url_mode(ex, a.get(1))?;
    encoding_arg(ex, a.get(2), 3)?;
    reserve_units(ex, s.len(), 9)?;
    Ok(Value::from(url::url_encode(s.units(), mode).as_str()))
}

fn decode_string(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    let mode = url_mode(ex, a.get(1))?;
    encoding_arg(ex, a.get(2), 3)?;
    Ok(str_value(url::url_decode(s.units(), mode)))
}

fn xml_version(ex: &mut Execution, a: Option<&Arg>) -> CallResult<bool> {
    if !is_given(a) {
        return Ok(false);
    }
    Ok(arg_str(ex, a)?.to_string_lossy().trim() == "1.1")
}

fn find_disallowed(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    let start = if is_given(a.get(1)) {
        arg_i64(ex, a.get(1), 2)?.max(1)
    } else {
        1
    };
    let v11 = xml_version(ex, a.get(2))?;
    let skip = usize::try_from(start - 1).unwrap_or(usize::MAX);
    let Some(tail) = s.units().get(skip..) else {
        return Ok(int_value(0));
    };
    let p = xml::xml_invalid_pos(tail, v11);
    Ok(int_value(if p > 0 { p + (start - 1) } else { p }))
}

fn date_format(ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<DateFormat> {
    match enum_arg(ex, a, "ФорматДатыJSON", param)? {
        Some("Microsoft") => Ok(DateFormat::Microsoft),
        Some("JavaScript") => Ok(DateFormat::JavaScript),
        Some(_) => Ok(DateFormat::Iso),
        None => Err(type_mismatch(param).into()),
    }
}

fn write_json_date(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let d = arg_date(ex, a.first())?;
    let f = date_format(ex, a.get(1), 2)?;
    let v = match enum_arg(ex, a.get(2), "ВариантЗаписиДатыJSON", 3)? {
        Some("ЛокальнаяДатаСоСмещением") => DateVariant::LocalWithOffset,
        Some("УниверсальнаяДата") => DateVariant::Universal,
        _ => DateVariant::Local,
    };
    let t = write_date(d, session_offset_of_local(ex, d), f, v).map_err(lib_error)?;
    Ok(Value::from(t.as_str()))
}

fn read_json_date(ex: &mut Execution, a: Args) -> CallResult<Value> {
    let s = arg_str(ex, a.first())?;
    let f = date_format(ex, a.get(1), 2)?;
    let x = read_date(s.units(), f).map_err(lib_error)?;
    Ok(Value::Date(match x.offset {
        None => x.date,
        Some(o) => utc_to_session(ex, x.date.add_seconds(&Number::from_i64(-i64::from(o)))),
    }))
}
