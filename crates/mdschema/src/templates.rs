use crate::tables::SchemaError;
use std::collections::HashMap;

pub use serde_json::Value;

#[derive(Debug)]
pub struct TemplateTables {
    pub mxl: Value,
    pub chart: Value,
    pub type_guid: HashMap<String, String>,
    types: HashMap<String, crate::PlatformType>,
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
    pub fn type_guid_expanded(&self, name: crate::ExpandedTypeName<'_>, set: bool) -> Option<&str> {
        crate::type_names::unique_type_guid(
            self.types.iter().map(|(guid, ty)| {
                (
                    guid.as_str(),
                    crate::BorrowedPlatformType {
                        name: ty.name(),
                        set: ty.set,
                    },
                )
            }),
            name,
            set,
        )
    }
    fn load(resource: &'static crate::SchemaResource) -> Result<TemplateTables, SchemaError> {
        let raw = resource.json("types.json")?;
        let types = crate::type_names::parse_platform_types(raw)?;
        let rows: crate::spec::Ordered<Value> =
            serde_json::from_str(raw).map_err(|e| SchemaError(e.to_string()))?;
        let mut type_guid = HashMap::new();
        for (guid, row) in rows.0 {
            if !guid.starts_with('_')
                && row.get(2).is_none_or(Value::is_null)
                && let Some(name) = types.get(&guid).and_then(|ty| ty.legacy_qname())
            {
                type_guid.insert(name, guid);
            }
        }
        Ok(TemplateTables {
            type_guid,
            types,
            mxl: parse("tables.json", resource.json("templates/tables.json")?)?,
            chart: parse(
                "chart_tables.json",
                resource.json("templates/chart_tables.json")?,
            )?,
        })
    }
}
