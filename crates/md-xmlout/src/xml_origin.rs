use crate::{DumpOptions, DumpReport, DumpSink, Part, Result};
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
pub fn dump(
    cfg: &Configuration,
    out: &std::path::Path,
    opts: &DumpOptions,
) -> Result<Option<DumpReport>> {
    let Some(origin) = &cfg.xml_origin else {
        return Ok(None);
    };
    let mut sink = DumpSink::new(out);
    for (path, file) in origin.files() {
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
    Ok(Some(
        sink.into_report(cfg.diagnostics.misses.clone(), Vec::new()),
    ))
}
