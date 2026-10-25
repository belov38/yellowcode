use crate::{Configuration, LoadProfile, Result};
pub use mdconf_core::xml_in::*;
use std::path::Path;

pub fn load_xml(dir: &Path, options: &PartsOptions) -> Result<Configuration> {
    let PartsDump {
        mut configuration, ..
    } = load_parts(dir, options)?;
    if configuration
        .xml_origin
        .as_ref()
        .is_some_and(|origin| origin.format() == XmlFormat::V2_20)
    {
        return Err(crate::Error::Xml {
            path: dir.into(),
            what: "full 2.20 XML load requires the Task 6 translation".into(),
        });
    }
    md_graphics::xml_in::load(&mut configuration)?;
    md_forms::xml_in::load(&mut configuration, options)?;
    configuration.profile = LoadProfile::Full;
    Ok(configuration)
}
