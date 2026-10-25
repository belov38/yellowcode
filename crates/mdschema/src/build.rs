use crate::SchemaError;
use std::collections::BTreeMap;

pub fn events() -> Result<&'static BTreeMap<String, String>, SchemaError> {
    crate::embedded_resource()?.events()
}
impl crate::SchemaResource {
    pub fn events(&'static self) -> Result<&'static BTreeMap<String, String>, SchemaError> {
        self.events
            .get_or_init(|| {
                let json = self.json("build/events.json").map_err(|e| e.0)?;
                serde_json::from_str(json).map_err(|e| format!("build/events.json: {e}"))
            })
            .as_ref()
            .map_err(|e| SchemaError(e.clone()))
    }
}
