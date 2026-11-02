use crate::ConfigurationExt;
use crate::{Configuration, Result};
use md_xmlout::codec;
use md_xmlout::{DumpOptions, DumpReport, DumpSink, Part, PartStatus};
use std::collections::BTreeMap;
use std::path::Path;

pub fn dump(cfg: &Configuration, out: &Path, opts: &DumpOptions) -> Result<DumpReport> {
    Ok(dump_into(cfg, DumpSink::new(out), opts)?.0)
}

pub fn dump_to_memory(
    cfg: &Configuration,
    opts: &DumpOptions,
) -> Result<(DumpReport, BTreeMap<String, Vec<u8>>)> {
    dump_into(cfg, DumpSink::memory(), opts)
}

fn dump_into(
    cfg: &Configuration,
    mut sink: DumpSink,
    opts: &DumpOptions,
) -> Result<(DumpReport, BTreeMap<String, Vec<u8>>)> {
    cfg.require_full()?;
    if md_xmlout::xml_origin::dump(cfg, &mut sink, opts)? {
        return Ok(sink.finish(cfg.diagnostics.misses.clone(), Vec::new()));
    }
    if opts.parts.contains(&Part::Configuration) {
        let _ = codec::parts::configuration::try_configuration_xml(cfg)?;
    }
    if opts.parts.contains(&Part::Predefined) {
        codec::parts::predefined::preflight(cfg)?;
    }
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
    Ok(sink.finish(cfg.diagnostics.misses.clone(), not_written))
}
