use crate::{DumpOptions, DumpSink, Part, Result};
use md_model::{Configuration, xml_origin::XmlPart};
impl From<XmlPart> for Part {
    fn from(part: XmlPart) -> Self {
        match part {
            XmlPart::Objects => Self::Objects,
            XmlPart::Modules => Self::Modules,
            XmlPart::Forms => Self::Forms,
            XmlPart::Rights => Self::Rights,
            XmlPart::Predefined => Self::Predefined,
            XmlPart::CommandInterface => Self::CommandInterface,
            XmlPart::Configuration => Self::Configuration,
            XmlPart::Help => Self::Help,
            XmlPart::Pictures => Self::Pictures,
            XmlPart::Misc => Self::Misc,
            XmlPart::Templates => Self::Templates,
            XmlPart::Flowcharts => Self::Flowcharts,
        }
    }
}
pub fn dump(cfg: &Configuration, sink: &mut DumpSink, opts: &DumpOptions) -> Result<bool> {
    let Some(origin) = &cfg.xml_origin else {
        return Ok(false);
    };
    let versions_changed =
        origin.named_versions() != cfg.parts.dump_info.as_ref().map(|d| d.versions.as_slice());
    let configdump = if versions_changed && opts.parts.contains(&Part::Configuration) {
        cfg.parts
            .dump_info
            .as_ref()
            .map(|d| {
                crate::codec::parts::configdump::convert(cfg, d).map_err(|what| {
                    crate::Error::Format {
                        entry: "ConfigDumpInfo.xml".into(),
                        what,
                    }
                })
            })
            .transpose()?
    } else {
        None
    };
    for (path, file) in origin.files() {
        if versions_changed && path == std::path::Path::new("ConfigDumpInfo.xml") {
            continue;
        }
        let part = Part::from(file.part());
        if opts.parts.contains(&part) {
            let rel = path
                .iter()
                .filter_map(|s| s.to_str())
                .collect::<Vec<_>>()
                .join("/");
            sink.put(part, &rel, file.bytes())?;
        }
    }
    if let Some(xml) = configdump {
        sink.put(Part::Configuration, "ConfigDumpInfo.xml", xml.as_bytes())?;
    }
    Ok(true)
}
