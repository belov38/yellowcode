mod document;
mod reader;
mod writer;

use crate::binary::guard_bytes;
use crate::bind::{arg_str, is_given, lib_error, msg};
use crate::enums::type_by_id;
use crate::fs::{fs, fs_error};
use crate::globals::data::encoding_from_value;
use bsl_lib::codec::{self, CodePages, Encoding, EncodingName};
use bsl_vm::{Arg, CallResult, Cause, Execution, HostError, Registry, TypeImpl, Value};
use platform_catalog::TypeIdx;
use std::sync::{Arc, OnceLock};

pub(crate) struct TextTypes {
    pub(crate) writer: TypeIdx,
    pub(crate) reader: TypeIdx,
    pub(crate) document: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
pub(crate) fn text_types() -> &'static TextTypes {
    static T: OnceLock<TextTypes> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        TextTypes {
            writer: t("TextWriter"),
            reader: t("TextReader"),
            document: t("TextDocument"),
        }
    })
}

pub(crate) fn register(reg: &mut Registry) {
    let t = text_types();
    reg.type_impl(
        t.writer,
        TypeImpl {
            ctor: Some(Arc::new(writer::ctor)),
        },
    );
    reg.type_impl(
        t.reader,
        TypeImpl {
            ctor: Some(Arc::new(reader::ctor)),
        },
    );
    reg.type_impl(
        t.document,
        TypeImpl {
            ctor: Some(Arc::new(document::ctor)),
        },
    );
}

pub(crate) fn not_open() -> HostError {
    HostError::key(msg::FILE_NOT_OPEN)
}

const PAGES: CodePages = CodePages::STAND;

pub(crate) fn encoding_arg(
    ex: &mut Execution,
    v: &Value,
    param: usize,
    default: Default,
) -> CallResult<(Encoding, bool)> {
    match v {
        Value::Undefined => match default {
            Default::Ansi => Ok((page(ex, 0)?, false)),
            Default::System => Ok((page(ex, 2)?, false)),
            Default::Utf8 => encoding_from_value(ex, v, param),
        },
        Value::String(s) => codec::resolve(EncodingName::Named(s.units()), &PAGES).map_err(|e| {
            HostError::key(msg::CHAR_CONVERSION)
                .caused_by(Cause::Host(Box::new(lib_error(e))))
                .into()
        }),
        _ => encoding_from_value(ex, v, param),
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Default {
    Utf8,
    Ansi,
    System,
}

fn page(ex: &Execution, which: usize) -> CallResult<Encoding> {
    let label = ex.env().code_pages()[which];
    let units: Vec<u16> = label.encode_utf16().collect();
    let (e, _) = codec::resolve(EncodingName::Named(&units), &PAGES).map_err(lib_error)?;
    Ok(e)
}

pub(crate) fn text_arg(
    ex: &mut Execution,
    a: Option<&Arg>,
    default: &[u16],
) -> CallResult<Vec<u16>> {
    if !is_given(a) {
        return Ok(default.to_vec());
    }
    Ok(arg_str(ex, a)?.units().to_vec())
}

pub(crate) fn read_file(ex: &mut Execution, path: &str) -> CallResult<Vec<u8>> {
    let f = fs(ex, path)?;
    if let Some(st) = f.get().stat(path).map_err(fs_error)? {
        guard_bytes(ex, st.size)?;
    }
    Ok(f.get().read(path).map_err(fs_error)?)
}

const LF: [u16; 1] = [0x0A];
const CRLF: [u16; 2] = [0x0D, 0x0A];
