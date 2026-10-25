use bsl_intl::locale::{Locale, locale};
use bsl_prim::{BslDate, Number, Str};
use bsl_vm::{
    Arg, CallResult, Category, CategorySet, Execution, HostError, MsgKey, MsgRef, Registry, Value,
    ops,
};
use std::sync::Arc;

pub mod msg {
    pub const CONVERT_2NUMERIC: &str = "bsl:IDS_RTE_CONVERT_2NUMERIC";
    pub const CONVERT_2DATE: &str = "bsl:IDS_RTE_CONVERT_2DATE";
    pub const CONVERT_2BOOL: &str = "bsl:IDS_RTE_CONVERT_2BOOL";
    pub const CMPTYPES: &str = "bsl:IDS_RTE_CMPTYPES";
    pub const XML_CONVERSION: &str = "xml2:IDS_XMLERR_DATACONVERSION";
    pub const TYPE_MISMATCH: &str = "core85:IDS_PARAMETER_N_TYPE_MISMATCH";
    pub const PROPERTY_TYPE_MISMATCH: &str = "core85:IDS_PROPERTY_TYPE_MISMATCH";
    pub const PROP_NOT_FOUND: &str = "bsl:IDS_RTE_VALPROP_NOTFOUND";
    pub const PROP_NOT_WRITABLE: &str = "bsl:IDS_RTE_PROP_NONWRITABLE";
    pub const METHOD_NOT_FOUND: &str = "bsl:IDS_RTE_VALMETHOD_NOTFOUND";
    pub const VALUE_MISMATCH: &str = "core85:IDS_PARAMETER_N_VALUE_MISMATCH";
    pub const INDEX_OFFBOUND: &str = "bsl:IDS_RTE_INDEX_OFFBOUND";
    pub const RANGE: &str = "core85:IDS_INDEX_OFFBOUND";
    pub const BAD_KEY: &str = "core85:IDS_VS_BADPROPNAME_ERROR";
    pub const INDEXED_READONLY: &str = "bsl:IDS_RTE_INDEXED_READONLY";
    pub const BUFFER_READONLY: &str = "ext:IDS_ERROR_BUFFER_READONLY";
    pub const BYTE_RANGE: &str = "ext:IDS_ERROR_BYTE_RANGE_MISMATCH";
    pub const NEGATIVE_SIZE: &str = "ext:IDS_ERROR_COUNT_CANNOT_BE_NEGATIVE";
    pub const INTEGER_RANGE: &str = "ext:IDS_ERROR_INTEGER_RANGE_MISMATCH";
    pub const STREAM_CLOSED: &str = "ext:IDS_STREAM_CLOSED_EXCEPTION";
    pub const FILE_NOT_OPEN: &str = "ext:IDS_SEQTEXT_ERR_NOTOPEN";
    pub const CHAR_CONVERSION: &str = "core85:IDS_IO_ERR_CHARCONVERSION";
    pub const CANT_READ: &str = "ext:IDS_CANT_READ_STREAM_EXCEPTION";
    pub const CANT_WRITE: &str = "ext:IDS_CANT_WRITE_STREAM_EXCEPTION";
    pub const CANT_SEEK: &str = "ext:IDS_CANT_SEEK_STREAM_EXCEPTION";
    pub const BUFFER_OVERFLOW: &str = "core85:IDS_EXCP_BUFFER_OVERFLOW";
    pub const PROPERTY_VALUE_MISMATCH: &str = "core85:IDS_PROPERTY_VALUE_MISMATCH";
    pub const FILE_OPERATION: &str = "core85:IDS_FERROR_FAILURE";
    pub const FILE_NOT_FOUND: &str = "core85:IDS_FERROR_FILENOTFOUND";
    pub const FILE_SHARING: &str = "core85:IDS_FERROR_SHARING";
    pub const FILE_ACCESS: &str = "core85:IDS_FERROR_ACCESS";
    pub const FILE_BAD_PATH: &str = "core85:IDS_FERROR_BADPATH";
    pub const FILE_IS_DIR: &str = "core85:IDS_FERR_ISADIR";
    pub const FILE_COPY: &str = "core85:IDS_FERROR_COPY";
    pub const FILE_COPY_MOVE: &str = "core85:IDS_FERROR_COPY_MOVE";
    pub const STREAM_FILE_NOT_FOUND: &str = "ext:IDS_FILE_NOT_FOUND_EXCEPTION";
    pub const COPY_FILES: &str = "ext:IDS_RTE_COPYFILE_ERROR";
    pub const MOVE_FILES: &str = "ext:IDS_RTE_MOVEFILE_ERROR";
    pub const DELETE_FILES: &str = "ext:IDS_RTE_DELETEFILES_ERROR";
    pub const HASH_CRC32: &str = "entext:IDS_HASH_FUNC_CRC32";
    pub const HASH_MD5: &str = "entext:IDS_HASH_FUNC_MD5";
    pub const HASH_SHA1: &str = "entext:IDS_HASH_FUNC_SHA1";
    pub const HASH_SHA256: &str = "entext:IDS_HASH_FUNC_SHA256";
    pub const HASH_SHA512: &str = "entext:IDS_HASH_FUNC_SHA512";
    pub const HASH_PBKDF2: &str = "entext:IDS_HASH_FUNC_PBKDF2SHA256";
    pub const BIG_ENDIAN: &str = "ext:IDS_BYTE_ORDER_BIG_ENDIAN";
    pub const LITTLE_ENDIAN: &str = "ext:IDS_BYTE_ORDER_LITTLE_ENDIAN";
    pub const XML_NOT_SERIALIZABLE: &str = "xml2:IDS_XMLERR_TYPENOTSERIALIZABLE";
    pub const XML_BAD_CHAR: &str = "xml2:IDS_XMLERR_BADXMLCHARACTER_FORMAT";
    pub const XML_BAD_NAME: &str = "xml2:IDS_XMLERR_BADXMLNAME";
    pub const XML_INVALID_DATA: &str = "xml2:IDS_XMLERR_INVALIDDATA";
    pub const XML_NS_MAPPING: &str = "xml2:IDS_XMLERR_INVALID_NAMESPACE_MAPPING";
    pub const XML_VERSION: &str = "xml2:IDS_XMLERR_UNKNOWN_XML_VERSION";
    pub const XML_PARSE: &str = "xml2:IDS_XMLREADER_ERR_PARSE";
    pub const XML_NO_SOURCE: &str = "xml2:IDS_XMLREADER_ERR_NOSOURCE";
    pub const XML_ERROR: &str = "xml2:IDS_XML_ERROR";
    pub const XML_FATAL: &str = "xml2:IDS_XML_FATALERROR";
    pub const XML_WRITE_ORDER: &str = "xml2:IDS_XMLWRITER_ERR_BAD_WRITE_ORDER";
    pub const XML_DUP_ATTR: &str = "xml2:IDS_XMLWRITER_ERR_DUPLICATE_ATTRIBUTES";
    pub const XML_INDENT_CHARS: &str = "xml2:IDS_XMLWRITER_ERR_INDENTCHARS";
    pub const XML_PI_TARGET: &str = "xml2:IDS_XMLWRITER_ERR_INVALID_PI_TARGET";
    pub const XML_WRITING: &str = "xml2:IDS_XMLWRITER_ERR_WRITING";
    pub const XML_NO_TARGET: &str = "xml2:IDS_XMLWRITER_ERR_NO_TARGET";
    pub const XML_ENCODING: &str = "xml2:IDS_XMLWRITER_ERR_UNKNOWN_ENCODING";
    pub const XDTO_VALIDATION: &str = "xdto:IDS_XDTOEXCEPTION_VALIDATION";
    pub const XDTO_TYPE_MISMATCH: &str = "xdto:IDS_XDTOEXCEPTION_TYPEMISMATCH";
    pub const XDTO_SERIALIZATION: &str = "xdto:IDS_XDTOEXCEPTION_SERIALIZATION";
    pub const XDTO_MODEL_ERROR: &str = "xdto:IDS_XDTO_VALIDATION_ERROR";
    pub const XDTO_CONTEXT_TYPE: &str = "xdto:IDS_XDTO_VALIDATION_CONTEXT_TYPE";
    pub const XDTO_CONTEXT_PACKAGE: &str = "xdto:IDS_XDTO_VALIDATION_CONTEXT_PACKAGE";
    pub const XDTO_FACET_7_1: &str = "xdto:IDS_XDTO_VALIDATION_FACET_7_1";
    pub const XDTO_VALUETYPE_5_1_3: &str = "xdto:IDS_XDTO_VALIDATION_VALUETYPE_5_1_3";
    pub const XDTO_VALUE_TYPE: &str = "xdto:IDS_XDTOEXCEPTION_VALIDATION_VALUETYPE";
    pub const XDTO_FACET_MISMATCH: &str = "xdto:IDS_XDTOEXCEPTION_VALIDATION_VALUETYPE_FACET";
    pub const XDTO_READ_OBJECT: &str = "xdto:IDS_XDTOEXCEPTION_SERIALIZATION_OBJECTTYPE";
    pub const XDTO_WRITE_CUSTOM: &str = "xdto:IDS_XDTOEXCEPTION_SERIALIZATION_CUSTOM_PROPERTY";
    pub const XDTO_WRITE_PROPERTY: &str = "xdto:IDS_XDTOEXCEPTION_SERIALIZATION_PROPERTY";
    pub const XDTO_STRUCTURE: &str = "xdto:IDS_XDTOEXCEPTION_VALIDATION_OBJECTTYPE";
    pub const XDTO_CHECK_PROPERTY: &str = "xdto:IDS_XDTOEXCEPTION_VALIDATION_OBJECTTYPE_PROPERTY";
    pub const XDTO_CHECK_CUSTOM: &str =
        "xdto:IDS_XDTOEXCEPTION_VALIDATION_OBJECTTYPE_PROPERTY_CUSTOM";
    pub const XDTO_NS_MAPPING: &str = "xdto:IDS_XDTOREADEREXCEPTION_NAMESPACE";
    pub const XDTO_NS_MAPPING_PREFIX: &str = "xdto:IDS_XDTOREADEREXCEPTION_NAMESPACE_FORMAT";
    pub const XDTO_UNDEFINED_TYPE: &str = "xdto:IDS_XDTOREADEREXCEPTION_UNDEFINEDTYPE";
    pub const XDTO_UNDEFINED_TYPE_NAME: &str = "xdto:IDS_XDTOREADEREXCEPTION_UNDEFINEDTYPE_FORMAT";
    pub const XDTO_REQUIRED: &str = "xdto:IDS_XDTO_ERROR_REQUIREDPROPERTY_MISSING";
    pub const XDTO_EXPECTED: &str = "xdto:IDS_XDTO_ERROR_PROPERTY_EXPECTED_PROPERTIES";
    pub const XDTO_LOWER: &str = "xdto:IDS_XDTO_ERROR_PROPERTY_LOWERBOUNDS_MISMATCH";
    pub const XDTO_UPPER: &str = "xdto:IDS_XDTO_ERROR_PROPERTY_UPPERBOUNDS_MISMATCH";
    pub const XDTO_PROPERTY_TYPE: &str = "xdto:IDS_XDTO_ERROR_PROPERTY_TYPE_MISMATCH";
    pub const XDTO_UNORDERED: &str = "xdto:IDS_XDTO_ERROR_PROPERTY_UNORDERED_PROPERTIES";
    pub const XDTO_WRITE_ERROR: &str = "xdto:IDS_XDTOWRITEREXCEPTION_WRITEERROR";
    pub const XDTO_UNKNOWN_PROPERTY: &str = "xdto:IDS_XDTOEXCEPTION_UNKNOWNPROPERTY_NAMED";
    pub const XDTO_ABSTRACT: &str = "xdto:IDS_XDTOEXCEPTION_ABSTRACTTYPE_NAMED";
    pub const XDTO_TWO_TYPES: &str = "xdto:IDS_XDTOEXCEPTION_TYPEMISMATCH_2_TYPES";
    pub const XDTO_TYPE_NOT_FOUND: &str = "xdto:IDS_XDTOEXCEPTION_TYPEMISMATCH_TYPE_NOT_FOUND";
    pub const XDTO_OUT_OF_BOUNDS: &str = "xdto:IDS_XDTOEXCEPTION_OUTOFBOUNDS";
    pub const XDTO_START_ITEM: &str = "xdto:IDS_XDTONODETYPE_STARTITEM";
    pub const XDTO_READER_CONTEXT: &str = "xdto:IDS_XDTO_STREAM_READER_PROPERTY_CONTEXT";
    pub const XDTO_TYPE_MAPPING_LEXICAL: &str = "xdto:IDS_XDTOEXCEPTION_TYPEMAPPING_LEXICAL_V8";
    pub const PICTURE_TYPE: &str = "core85:IDS_DISP_E_GETPICTURETYPE";
    pub const FONT_BOLD: &str = "core85:IDS_FONT_BOLD";
    pub const FONT_ITALIC: &str = "core85:IDS_FONT_ITALIC";
    pub const FONT_UNDERLINE: &str = "core85:IDS_FONT_UNDERLINE";
    pub const FONT_STRIKEOUT: &str = "core85:IDS_FONT_STRIKEOUT";
    pub const XDTOJSON_INVALID_TYPE: &str = "json:IDS_XDTOJSON_INVALIDTYPE";
    pub const XDTO_MAPPING_MISSING: &str = "xdto:IDS_XDTOEXCEPTION_TYPEMAPPING_MISSING";
    pub const XDTO_UNKNOWN_PACKAGE: &str = "xdto:IDS_XDTO_ERROR_UNKNOWN_PACKAGE";
    pub const XDTO_TYPE_MAPPING: &str = "xdto:IDS_XDTOEXCEPTION_TYPEMAPPING";
    pub const XDTO_TYPE_MAPPING_FORMAT: &str = "xdto:IDS_XDTOEXCEPTION_TYPEMAPPING_V8_XDTO";
    pub const XDTO_NODE_VALUE: &str = "xdto:IDS_XDTONODETYPE_VALUE";
    pub const XDTO_VALUE_CONTEXT: &str = "xdto:IDS_XDTO_STREAM_READER_VALUE_CONTEXT";
    pub const XDTOJSON_UNKNOWN_TYPE: &str = "json:IDS_XDTOJSON_UNKNOWNTYPE";
}

pub const MESSAGE_KEYS: &[&str] = &[
    msg::CONVERT_2NUMERIC,
    msg::CONVERT_2DATE,
    msg::CONVERT_2BOOL,
    msg::CMPTYPES,
    msg::XML_CONVERSION,
    msg::TYPE_MISMATCH,
    msg::PROPERTY_TYPE_MISMATCH,
    msg::PROP_NOT_FOUND,
    msg::PROP_NOT_WRITABLE,
    msg::METHOD_NOT_FOUND,
    msg::VALUE_MISMATCH,
    msg::INDEX_OFFBOUND,
    msg::RANGE,
    msg::BAD_KEY,
    msg::INDEXED_READONLY,
    msg::BUFFER_READONLY,
    msg::BYTE_RANGE,
    msg::NEGATIVE_SIZE,
    msg::INTEGER_RANGE,
    msg::STREAM_CLOSED,
    msg::FILE_NOT_OPEN,
    msg::CHAR_CONVERSION,
    msg::CANT_READ,
    msg::CANT_WRITE,
    msg::CANT_SEEK,
    msg::BUFFER_OVERFLOW,
    msg::PROPERTY_VALUE_MISMATCH,
    msg::FILE_NOT_FOUND,
    msg::FILE_OPERATION,
    msg::FILE_SHARING,
    msg::FILE_ACCESS,
    msg::FILE_BAD_PATH,
    msg::FILE_IS_DIR,
    msg::FILE_COPY,
    msg::FILE_COPY_MOVE,
    msg::STREAM_FILE_NOT_FOUND,
    msg::COPY_FILES,
    msg::MOVE_FILES,
    msg::DELETE_FILES,
    msg::HASH_CRC32,
    msg::HASH_MD5,
    msg::HASH_SHA1,
    msg::HASH_SHA256,
    msg::HASH_SHA512,
    msg::HASH_PBKDF2,
    msg::BIG_ENDIAN,
    msg::LITTLE_ENDIAN,
    msg::XML_NOT_SERIALIZABLE,
    msg::XML_BAD_CHAR,
    msg::XML_BAD_NAME,
    msg::XML_INVALID_DATA,
    msg::XML_NS_MAPPING,
    msg::XML_VERSION,
    msg::XML_PARSE,
    msg::XML_NO_SOURCE,
    msg::XML_ERROR,
    msg::XML_FATAL,
    msg::XML_WRITE_ORDER,
    msg::XML_DUP_ATTR,
    msg::XML_INDENT_CHARS,
    msg::XML_PI_TARGET,
    msg::XML_NO_TARGET,
    msg::XML_WRITING,
    msg::XML_ENCODING,
    msg::XDTO_VALIDATION,
    msg::XDTO_TYPE_MISMATCH,
    msg::XDTO_SERIALIZATION,
    msg::XDTO_READ_OBJECT,
    msg::XDTO_WRITE_CUSTOM,
    msg::XDTO_WRITE_PROPERTY,
    msg::XDTO_STRUCTURE,
    msg::XDTO_CHECK_PROPERTY,
    msg::XDTO_CHECK_CUSTOM,
    msg::XDTO_NS_MAPPING,
    msg::XDTO_NS_MAPPING_PREFIX,
    msg::XDTO_UNDEFINED_TYPE,
    msg::XDTO_UNDEFINED_TYPE_NAME,
    msg::XDTO_EXPECTED,
    msg::XDTO_REQUIRED,
    msg::XDTO_LOWER,
    msg::XDTO_UPPER,
    msg::XDTO_PROPERTY_TYPE,
    msg::XDTO_UNORDERED,
    msg::XDTO_WRITE_ERROR,
    msg::XDTO_UNKNOWN_PROPERTY,
    msg::XDTO_ABSTRACT,
    msg::XDTO_TWO_TYPES,
    msg::XDTO_TYPE_NOT_FOUND,
    msg::XDTO_OUT_OF_BOUNDS,
    msg::XDTO_START_ITEM,
    msg::XDTO_READER_CONTEXT,
    msg::XDTOJSON_INVALID_TYPE,
    msg::XDTOJSON_UNKNOWN_TYPE,
    msg::XDTO_TYPE_MAPPING,
    msg::XDTO_MAPPING_MISSING,
    msg::XDTO_UNKNOWN_PACKAGE,
    msg::XDTO_MODEL_ERROR,
    msg::XDTO_CONTEXT_TYPE,
    msg::XDTO_CONTEXT_PACKAGE,
    msg::XDTO_FACET_7_1,
    msg::XDTO_VALUETYPE_5_1_3,
    msg::XDTO_VALUE_TYPE,
    msg::XDTO_FACET_MISMATCH,
    msg::XDTO_TYPE_MAPPING_FORMAT,
    msg::XDTO_NODE_VALUE,
    msg::XDTO_VALUE_CONTEXT,
    msg::XDTO_TYPE_MAPPING_LEXICAL,
    msg::PICTURE_TYPE,
    msg::FONT_BOLD,
    msg::FONT_ITALIC,
    msg::FONT_UNDERLINE,
    msg::FONT_STRIKEOUT,
];

pub fn session_locale(ex: &Execution) -> Option<&'static Locale> {
    locale(ex.env().locale()).or_else(|| locale("ru_RU"))
}

pub fn arg_value(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Value> {
    match a {
        None | Some(Arg::Omitted) => Ok(Value::Undefined),
        Some(Arg::Val(v)) => Ok(v.clone()),
        Some(Arg::Place(p)) => Ok(p.get(ex)?),
    }
}

pub fn is_given(a: Option<&Arg>) -> bool {
    matches!(a, Some(Arg::Val(_) | Arg::Place(_)))
}

pub fn arg_str(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Str> {
    match arg_value(ex, a)? {
        Value::String(s) => Ok(s),
        Value::Undefined | Value::Null => Ok(Str::new()),
        v => Ok(ex.present(&v)?),
    }
}

pub fn to_number(ex: &mut Execution, v: &Value) -> CallResult<Number> {
    match v {
        Value::Number(n) => Ok(n.clone()),
        Value::Bool(b) => Ok(Number::from_i64(i64::from(*b))),
        Value::String(s) => parse_number(ex, s.units()).ok_or_else(|| raw(msg::CONVERT_2NUMERIC)),
        _ => Err(raw(msg::CONVERT_2NUMERIC)),
    }
}

pub fn parse_number(ex: &Execution, units: &[u16]) -> Option<Number> {
    match session_locale(ex) {
        Some(loc) => bsl_intl::parse::parse_number(units, loc).ok(),
        None => Number::parse(String::from_utf16(units).ok()?.trim()),
    }
}

pub fn arg_number(ex: &mut Execution, a: Option<&Arg>, _param: usize) -> CallResult<Number> {
    let v = arg_value(ex, a)?;
    to_number(ex, &v)
}

pub fn arg_date(ex: &mut Execution, a: Option<&Arg>) -> CallResult<BslDate> {
    match arg_value(ex, a)? {
        Value::Date(d) => Ok(d),
        Value::String(s) => {
            bsl_intl::calendar::date_from_digits(s.units()).map_err(|e| intl_error(e).raw().into())
        }
        _ => Err(raw(msg::CONVERT_2DATE)),
    }
}

pub fn arg_bool(ex: &mut Execution, a: Option<&Arg>) -> CallResult<bool> {
    let v = arg_value(ex, a)?;
    Ok(ops::to_bool(ex, &v)?)
}

pub fn arg_i64(ex: &mut Execution, a: Option<&Arg>, param: usize) -> CallResult<i64> {
    Ok(arg_number(ex, a, param)?.trunc_to_i64_saturating())
}

pub(crate) fn key_error(key: &'static str, args: Vec<String>, suffix: String) -> HostError {
    let mut e = HostError::key(key);
    e.msg = MsgRef::Key(MsgKey { key, args, suffix });
    e
}

pub(crate) fn raw(key: &'static str) -> bsl_vm::CallError {
    HostError::key(key).raw().into()
}

pub fn type_mismatch(param: usize) -> HostError {
    key_error(msg::TYPE_MISMATCH, vec![param.to_string()], String::new())
}

pub(crate) fn with_other(mut e: HostError, usage: bool) -> HostError {
    let mut c = vec![Category::AllErrors, Category::ScriptRuntimeError];
    if usage {
        c.push(Category::ScriptUseError);
    }
    c.push(Category::OtherError);
    e.categories = Some(CategorySet::of(&c));
    e
}

pub(crate) fn strict_number(
    ex: &mut Execution,
    a: Option<&Arg>,
    param: usize,
    other: bool,
) -> CallResult<Number> {
    match arg_value(ex, a)? {
        Value::Number(n) => Ok(n),
        _ => {
            let e = type_mismatch(param);
            Err(if other { with_other(e, true) } else { e }.into())
        }
    }
}

pub(crate) fn strict_i64(
    ex: &mut Execution,
    a: Option<&Arg>,
    param: usize,
    other: bool,
) -> CallResult<i64> {
    Ok(strict_number(ex, a, param, other)?.trunc_to_i64_saturating())
}

pub(crate) fn strict_count(
    ex: &mut Execution,
    a: Option<&Arg>,
    param: usize,
    other: bool,
) -> CallResult<u64> {
    let n = strict_number(ex, a, param, other)?;
    let e = HostError::key(msg::INTEGER_RANGE);
    let e = if other { with_other(e, true) } else { e };
    if !n.is_integer() {
        return Err(e.into());
    }
    u64::try_from(n.trunc_to_i64_saturating()).map_err(|_| e.into())
}

pub(crate) fn value_mismatch(param: usize) -> HostError {
    key_error(msg::VALUE_MISMATCH, vec![param.to_string()], String::new())
}

pub fn lib_error(e: bsl_lib::LibError) -> HostError {
    key_error(e.key, e.args, e.suffix)
}

pub fn intl_error(e: bsl_intl::LibError) -> HostError {
    key_error(e.key, e.args, e.suffix)
}

pub fn global(
    reg: &mut Registry,
    ru: &str,
    en: &str,
    f: impl Fn(&mut Execution, &mut [Arg]) -> CallResult<Value> + Send + Sync + 'static,
) {
    reg.global_fn(ru, en, Arc::new(f));
}
