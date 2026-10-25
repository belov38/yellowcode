use crate::ConfigurationExt;
use crate::{Configuration, Result};
use md_xmlout::codec;
use md_xmlout::{DumpOptions, DumpReport, DumpSink, Part, PartStatus};
use std::path::Path;

pub fn dump(cfg: &Configuration, out: &Path, opts: &DumpOptions) -> Result<DumpReport> {
    cfg.require_full()?;
    if let Some(report) = md_xmlout::xml_origin::dump(cfg, out, opts)? {
        return Ok(report);
    }
    let mut sink = DumpSink::new(out);
    let mut not_written = Vec::new();
    for &part in &opts.parts {
        let status = match part {
            Part::Objects => codec::objects::dump(cfg, &mut sink)?,
            Part::Modules => codec::modules::dump(cfg, &mut sink)?,
            Part::Forms => md_forms::forms::dump(cfg, &mut sink)?,
            Part::Rights
            | Part::Predefined
            | Part::CommandInterface
            | Part::Configuration
            | Part::Help
            | Part::Pictures
            | Part::Misc => codec::parts::dump(cfg, &mut sink, part)?,
            Part::Templates => md_graphics::templates::dump(cfg, &mut sink)?,
            Part::Flowcharts => md_graphics::graph::dump(cfg, &mut sink)?,
        };
        if status == PartStatus::NotImplemented {
            not_written.push(part);
        }
    }
    Ok(sink.into_report(cfg.diagnostics.misses.clone(), not_written))
}
