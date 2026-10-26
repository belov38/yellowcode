use crate::{Configuration, LoadProfile, Result};
pub use mdconf_core::xml_in::*;
use std::path::Path;

pub fn load_xml(dir: &Path, options: &PartsOptions) -> Result<Configuration> {
    let mut dump = load_parts(dir, options)?;
    if dump
        .configuration
        .xml_origin
        .as_ref()
        .is_some_and(|origin| origin.format() == XmlFormat::V2_20)
    {
        dump = translate_2_20(dump, options)?;
    }
    let PartsDump {
        mut configuration, ..
    } = dump;
    md_graphics::xml_in::load(&mut configuration)?;
    md_forms::xml_in::load(&mut configuration, options)?;
    configuration.profile = LoadProfile::Full;
    Ok(configuration)
}
