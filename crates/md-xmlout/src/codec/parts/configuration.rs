use crate::codec::values::write_prop;
use crate::{DumpSink, Part, PartStatus, Result, XmlWriter, document};
use md_model::*;
use std::collections::HashMap;

pub fn configuration_xml(cfg: &Configuration) -> Option<String> {
    if cfg.props.is_empty() {
        return None;
    }
    let schema = mdschema::schema().ok()?;
    let mut by_kind: HashMap<&str, Vec<&str>> = HashMap::new();
    for o in &cfg.objects {
        by_kind
            .entry(o.kind.as_str())
            .or_default()
            .push(o.name.as_str());
    }
    let mut w = XmlWriter::new(1);
    w.open("Configuration", &[("uuid", &cfg.id.to_string())]);
    w.open("InternalInfo", &[]);
    for (class, object) in &cfg.parts.contained_objects {
        w.open("xr:ContainedObject", &[]);
        w.leaf("xr:ClassId", Some(&class.to_string()), &[]);
        w.leaf("xr:ObjectId", object.map(|g| g.to_string()).as_deref(), &[]);
        w.close("xr:ContainedObject");
    }
    w.close("InternalInfo");
    w.open("Properties", &[]);
    for p in &cfg.props {
        write_prop(&mut w, &p.name, p.codec, &p.value);
    }
    w.close("Properties");
    w.open("ChildObjects", &[]);
    for kind in schema.xdto_props("ConfigurationChildObjects") {
        for name in by_kind.get(kind.name.as_str()).into_iter().flatten() {
            w.leaf(&kind.name, Some(name), &[]);
        }
    }
    w.close("ChildObjects");
    w.close("Configuration");
    Some(document(&w))
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    if let Some(xml) = configuration_xml(cfg) {
        sink.put(Part::Configuration, "Configuration.xml", xml.as_bytes())?;
    }
    Ok(PartStatus::Written)
}
