use crate::tables::SchemaError;
use std::collections::HashMap;

pub use serde_json::Value;

#[derive(Debug)]
pub struct TemplateTables {
    pub mxl: Value,
    pub chart: Value,
    pub type_guid: HashMap<String, String>,
}

pub fn template_tables() -> Result<&'static TemplateTables, SchemaError> {
    crate::embedded_resource()?.template_tables()
}
impl crate::SchemaResource {
    pub fn template_tables(&'static self) -> Result<&'static TemplateTables, SchemaError> {
        self.templates
            .get_or_init(|| TemplateTables::load(self).map_err(|e| e.0))
            .as_ref()
            .map_err(|e| SchemaError(e.clone()))
    }
}

fn parse(file: &str, text: &str) -> Result<Value, SchemaError> {
    serde_json::from_str(text).map_err(|e| SchemaError(format!("{file}: {e}")))
}

impl TemplateTables {
    fn load(resource: &'static crate::SchemaResource) -> Result<TemplateTables, SchemaError> {
        let types = parse("types.json", resource.json("types.json")?)?;
        let mut type_guid = HashMap::new();
        if let Value::Object(m) = &types {
            for (guid, v) in m {
                if let Some(q) = v.get(1).and_then(Value::as_str) {
                    type_guid.insert(q.to_owned(), guid.clone());
                }
            }
        }
        Ok(TemplateTables {
            type_guid,
            mxl: parse("tables.json", resource.json("templates/tables.json")?)?,
            chart: parse(
                "chart_tables.json",
                resource.json("templates/chart_tables.json")?,
            )?,
        })
    }
}
