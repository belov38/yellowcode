use crate::tables::SchemaError;
use crate::{FactJson, FactMap};

pub use serde_json::Value;

#[derive(Debug)]
pub struct TemplateTables {
    pub mxl: FactJson,
    pub chart: FactJson,
    pub type_guid: FactMap<String, String>,
    pub(crate) types: FactMap<String, crate::PlatformType>,
}

pub fn template_tables() -> Result<&'static TemplateTables, SchemaError> {
    crate::embedded_resource()?.template_tables()
}
impl crate::SchemaResource {
    pub fn template_tables(&'static self) -> Result<&'static TemplateTables, SchemaError> {
        if self.require_embedded().is_ok() {
            return self
                .embedded_templates()
                .map_err(|e| SchemaError(e.to_string()));
        }
        self.templates.legacy_plain(|| TemplateTables::load(self))
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
        let types: FactMap<String, crate::PlatformType> =
            crate::type_names::parse_platform_types(raw)?
                .into_iter()
                .collect();
        let rows: crate::spec::Ordered<Value> =
            serde_json::from_str(raw).map_err(|e| SchemaError(e.to_string()))?;
        let mut type_guid = FactMap::new();
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
            mxl: parse("tables.json", resource.json("templates/tables.json")?)?.into(),
            chart: parse(
                "chart_tables.json",
                resource.json("templates/chart_tables.json")?,
            )?
            .into(),
        })
    }
}
