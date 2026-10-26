use crate::SchemaError;
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
pub struct Header {
    pub before: String,
    pub after: String,
}
#[derive(Debug, Deserialize)]
pub struct DefaultProperty {
    pub context: String,
    pub after: String,
    pub name: String,
    pub xml: String,
    pub evidence: usize,
}
#[derive(Debug, Deserialize)]
pub struct ValueChange {
    pub context: String,
    pub before: String,
    pub after: String,
    pub evidence: usize,
}
#[derive(Debug, Deserialize)]
pub struct Rules {
    pub headers: Header,
    pub defaults: Vec<DefaultProperty>,
    pub changes: Vec<ValueChange>,
}
pub fn rules() -> Result<&'static Rules, SchemaError> {
    static RULES: OnceLock<Result<Rules, String>> = OnceLock::new();
    RULES
        .get_or_init(|| {
            serde_json::from_str(include_str!("../data/fmt/metadata_2_20.json"))
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| SchemaError(format!("fmt/metadata_2_20.json: {e}")))
}
