use crate::{DumpSink, Part, PartStatus, Result};
use brace::Node;
use md_model::{Configuration, MdObject};

pub mod cmdi;
pub mod configdump;
pub mod configuration;
pub mod help;
pub mod misc;
pub mod pictures;
pub mod predefined;
pub mod rights;

pub const XML_HEAD: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>";

pub fn dump(cfg: &Configuration, sink: &mut DumpSink, part: Part) -> Result<PartStatus> {
    match part {
        Part::Rights => rights::dump(cfg, sink),
        Part::Predefined => predefined::dump(cfg, sink),
        Part::CommandInterface => cmdi::dump(cfg, sink),
        Part::Configuration => {
            configuration::dump(cfg, sink)?;
            configdump::dump(cfg, sink)
        }
        Part::Misc => misc::dump(cfg, sink),
        Part::Help => help::dump(cfg, sink),
        Part::Pictures => pictures::dump(cfg, sink),
        _ => Ok(PartStatus::NotImplemented),
    }
}

pub type R<T> = std::result::Result<T, &'static str>;

fn each<'a>(objs: &'a [MdObject], f: &mut impl FnMut(&'a MdObject) -> Result<()>) -> Result<()> {
    for o in objs {
        f(o)?;
        each(&o.subsystems, f)?;
    }
    Ok(())
}

fn bool_text(v: bool) -> &'static str {
    if v { "true" } else { "false" }
}

pub fn int(n: &Node) -> Option<i64> {
    n.text()?.trim().parse().ok()
}

pub fn uint(n: &Node) -> Option<usize> {
    usize::try_from(int(n)?).ok()
}

pub fn txt(n: &Node) -> R<&str> {
    n.text().ok_or("shape")
}

pub fn count(n: &Node) -> R<usize> {
    txt(n)?;
    uint(n).ok_or("number")
}
