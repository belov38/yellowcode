mod reader;
mod values;
mod writer;

pub(crate) use reader::json_reader;
pub(crate) use writer::{json_account, json_writer};

use crate::enums::type_by_id;
use bsl_vm::Registry;
use platform_catalog::TypeIdx;
use std::sync::OnceLock;

pub(crate) mod msg {
    pub const NO_TARGET: &str = "json:IDS_NO_TARGET_STREAM_ERROR";
    pub const NO_SOURCE: &str = "json:IDS_READER_NOT_OPEN_ERROR";
    pub const NO_CURRENT: &str = "json:IDS_JSON_STREAM_READER_ERR_INVALID_VALUE";
    pub const UNSUPPORTED: &str = "mngcore:IDS_JSON_UNSUPPORTED_VALUE_TYPE";
    pub const BAD_KEY_TYPE: &str = "json:IDS_INVALID_KEY_TYPE";
    pub const BAD_PROPERTY_NAME: &str = "mngcore:IDS_JSON_INVALID_PROPERTY_NAME";
    pub const MAX_DEPTH: &str = "json:IDS_INVALID_MAX_DEPTH";
}

pub(crate) struct Types {
    pub writer: TypeIdx,
    pub settings: TypeIdx,
    pub reader: TypeIdx,
    pub value_type: TypeIdx,
    pub serializer_settings: TypeIdx,
    pub date_format: TypeIdx,
    pub date_variant: TypeIdx,
    pub line_break: TypeIdx,
    pub escape: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
pub(crate) fn types() -> &'static Types {
    static T: OnceLock<Types> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        let by_name = |n: &str| {
            platform_catalog::catalog()
                .find_type(n)
                .unwrap_or_else(|| panic!("catalog type {n}"))
        };
        Types {
            writer: t("JSONWriter"),
            settings: t("JSONWriterSettings"),
            reader: t("JSONReader"),
            value_type: t("JSONValueType"),
            serializer_settings: t("JSONSerializerSettings"),
            date_format: by_name("ФорматДатыJSON"),
            date_variant: by_name("ВариантЗаписиДатыJSON"),
            line_break: t("JSONLineBreak"),
            escape: t("JSONCharactersEscapeMode"),
        }
    })
}

pub(crate) fn enum_presentation(
    t: TypeIdx,
    name_en: &str,
    lang: platform_catalog::messages::Lang,
) -> Option<bsl_prim::Str> {
    if t == types().date_variant {
        let key = match name_en {
            "LocalDate" => "json:IDS_ENUMVAL_DATEWRITINGVARIANT_LOCAL",
            "LocalDateWithOffset" => "json:IDS_ENUMVAL_DATEWRITINGVARIANT_LOCALWITHOFFSET",
            "UniversalDate" => "json:IDS_ENUMVAL_DATEWRITINGVARIANT_UTC",
            _ => return None,
        };
        return platform_catalog::messages::text_key(key, lang).map(bsl_prim::Str::from);
    }
    if t != types().value_type {
        return None;
    }
    reader::value_type_text(name_en, lang)
}

pub(crate) fn register(reg: &mut Registry) {
    writer::register(reg);
    reader::register(reg);
    values::register(reg);
}
