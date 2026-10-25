#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibError {
    pub key: &'static str,
    pub args: Vec<String>,
    pub suffix: String,
}

impl LibError {
    pub fn new(key: &'static str) -> LibError {
        LibError {
            key,
            args: Vec::new(),
            suffix: String::new(),
        }
    }

    pub fn with_arg(mut self, a: impl ToString) -> LibError {
        self.args.push(a.to_string());
        self
    }

    pub fn with_suffix(mut self, s: impl Into<String>) -> LibError {
        self.suffix = s.into();
        self
    }
}

pub mod msg {
    pub const PARAM_VALUE: &str = "core85:IDS_PARAMETER_N_VALUE_MISMATCH";
    pub const TEMPLATE_WRONG: &str = "core85:IDS_TEMPLATE_PARAM_WRONG";
    pub const TEMPLATE_TOOMANY: &str = "core85:IDS_TEMPLATE_PARAMS_TOOMANY";
    pub const BADARGVALUE: &str = "bsl:IDS_RTE_BADARGVALUE";
    pub const NEGATIVE_POW: &str = "core85:IDS_NUMERROR_NEGATIVEPOW";
    pub const INT_RANGE: &str = "ext:IDS_ERROR_INTEGER_RANGE_MISMATCH";
    pub const HEX_FORMAT: &str = "core85:IDS_ERR_INVALID_HEX_STRING_FORMAT";
    pub const BIN_FORMAT: &str = "core85:IDS_ERR_INVALID_BINARY_STRING_FORMAT";
    pub const BAD_ENCODING: &str = "core85:IDS_CNVERROR_BADNAME";
    pub const BAD_ORDER: &str = "json:IDS_BAD_WRITING_ORDER";
    pub const DATE_JS: &str = "json:IDS_INVALID_DATE_TO_WRITE_JS_FORMAT";
    pub const DATE_MS: &str = "json:IDS_INVALID_DATE_TO_WRITE_MICROSOFT_FORMAT";
    pub const UNEXPECTED: &str = "json:IDS_READING_UNEXPECTED_TOKEN";
    pub const BAD_STATE: &str = "json:IDS_INVALID_JSONREADERSTATE";
    pub const DATE_FORMAT: &str = "json:IDS_INVALID_DATE_FORMAT";
}

pub const MESSAGE_KEYS: &[&str] = &[
    msg::PARAM_VALUE,
    msg::TEMPLATE_WRONG,
    msg::TEMPLATE_TOOMANY,
    msg::BADARGVALUE,
    msg::NEGATIVE_POW,
    msg::INT_RANGE,
    msg::HEX_FORMAT,
    msg::BIN_FORMAT,
    msg::BAD_ENCODING,
    msg::BAD_ORDER,
    msg::DATE_JS,
    msg::DATE_MS,
    msg::UNEXPECTED,
    msg::BAD_STATE,
    msg::DATE_FORMAT,
];
