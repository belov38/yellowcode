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
    pub const CONVERT_2DATE: &str = "bsl:IDS_RTE_CONVERT_2DATE";
    pub const CONVERT_2NUMERIC: &str = "bsl:IDS_RTE_CONVERT_2NUMERIC";
    pub const PARAM_VALUE: &str = "core85:IDS_PARAMETER_N_VALUE_MISMATCH";
    pub const BAD_TZ: &str = "core85:IDS_BADTIMEZONEID";
    pub const FORMAT_STRING: &str = "ext:IDS_ERR_FORMAT_STRING";
}

pub const MESSAGE_KEYS: &[&str] = &[
    msg::CONVERT_2DATE,
    msg::CONVERT_2NUMERIC,
    msg::PARAM_VALUE,
    msg::BAD_TZ,
    msg::FORMAT_STRING,
];
