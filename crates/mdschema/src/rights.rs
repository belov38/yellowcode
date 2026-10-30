use crate::FactJson;
use crate::tables::SchemaError;
use serde_json::{Map, Value};

#[derive(Debug)]
pub struct RightsTables {
    pub rights: Vec<(String, String)>,
    pub rights_by_kind: Vec<(String, Vec<String>)>,
    pub cmdi: FactJson,
    pub configdump: FactJson,
}

pub fn rights_tables() -> Result<&'static RightsTables, SchemaError> {
    crate::embedded_resource()?.rights_tables()
}
impl crate::SchemaResource {
    pub fn rights_tables(&'static self) -> Result<&'static RightsTables, SchemaError> {
        if self.require_embedded().is_ok() {
            return self
                .embedded_rights()
                .map_err(|e| SchemaError(e.to_string()));
        }
        self.rights.legacy_plain(|| RightsTables::load(self))
    }
}

fn object(file: &str, v: &Value) -> Result<Map<String, Value>, SchemaError> {
    v.as_object()
        .cloned()
        .ok_or_else(|| SchemaError(format!("{file}: not an object")))
}

fn parse(file: &str, text: &str) -> Result<Value, SchemaError> {
    serde_json::from_str(text).map_err(|e| SchemaError(format!("{file}: {e}")))
}

impl RightsTables {
    fn load(resource: &'static crate::SchemaResource) -> Result<RightsTables, SchemaError> {
        let rights_file = parse("rights.json", resource.json("rights/rights.json")?)?;
        let top = object("rights.json", &rights_file)?;
        let map = top
            .get("rights")
            .ok_or_else(|| SchemaError("rights.json: no \"rights\"".into()))?;
        let mut rights = Vec::new();
        for (guid, name) in object("rights.json", map)? {
            let name = name
                .as_str()
                .ok_or_else(|| SchemaError(format!("rights.json: {guid}: not a string")))?;
            rights.push((guid, name.to_string()));
        }

        let by_kind = parse(
            "rights_by_kind.json",
            resource.json("rights/rights_by_kind.json")?,
        )?;
        let mut rights_by_kind = Vec::new();
        for (kind, list) in object("rights_by_kind.json", &by_kind)? {
            if kind == "_doc" {
                continue;
            }
            let names = list
                .as_array()
                .ok_or_else(|| SchemaError(format!("rights_by_kind.json: {kind}: not an array")))?
                .iter()
                .map(|n| {
                    n.as_str().map(str::to_string).ok_or_else(|| {
                        SchemaError(format!("rights_by_kind.json: {kind}: not a string"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            rights_by_kind.push((kind, names));
        }

        Ok(RightsTables {
            rights,
            rights_by_kind,
            cmdi: parse("cmdi.json", resource.json("rights/cmdi.json")?)?.into(),
            configdump: parse("configdump.json", resource.json("rights/configdump.json")?)?.into(),
        })
    }
}
