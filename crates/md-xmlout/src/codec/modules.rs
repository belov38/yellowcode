use crate::paths;
use crate::{DumpSink, Part, PartStatus, Result};
use md_model::{Configuration, MdObject, Module, ModuleKind, SubObject};

pub const CONFIG_MODULES: [(&str, ModuleKind, &str); 4] = [
    (
        "0",
        ModuleKind::OrdinaryApplicationModule,
        "OrdinaryApplicationModule.bsl",
    ),
    (
        "5",
        ModuleKind::ExternalConnectionModule,
        "ExternalConnectionModule.bsl",
    ),
    (
        "6",
        ModuleKind::ManagedApplicationModule,
        "ManagedApplicationModule.bsl",
    ),
    ("7", ModuleKind::SessionModule, "SessionModule.bsl"),
];

fn file_of_kind(kind: ModuleKind) -> Option<&'static str> {
    Some(match kind {
        ModuleKind::Module => "Module.bsl",
        ModuleKind::ObjectModule => "ObjectModule.bsl",
        ModuleKind::ManagerModule => "ManagerModule.bsl",
        ModuleKind::RecordSetModule => "RecordSetModule.bsl",
        ModuleKind::ValueManagerModule => "ValueManagerModule.bsl",
        ModuleKind::CommandModule => "CommandModule.bsl",
        _ => return None,
    })
}

fn put(sink: &mut DumpSink, rel: String, m: &Module) -> Result<()> {
    sink.put(Part::Modules, &rel, &m.source)
}

fn dump_form(
    cfg: &Configuration,
    owner: &MdObject,
    f: &SubObject,
    sink: &mut DumpSink,
) -> Result<()> {
    if let Some(m) = &f.module {
        let rel = format!("{}/Ext/Form/Module.bsl", paths::sub_dir(cfg, owner, f));
        put(sink, rel, m)?;
    }
    Ok(())
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    let mut stack: Vec<&MdObject> = cfg.objects.iter().rev().collect();
    while let Some(o) = stack.pop() {
        let dir = paths::object_dir(cfg, o);
        for m in &o.modules {
            let rel = if m.kind == ModuleKind::FormModule {
                format!("{dir}/Ext/Form/Module.bsl")
            } else if let Some(f) = file_of_kind(m.kind) {
                format!("{dir}/Ext/{f}")
            } else {
                continue;
            };
            put(sink, rel, m)?;
        }
        for c in o.commands() {
            if let Some(m) = &c.module {
                put(
                    sink,
                    format!("{dir}/Commands/{}/Ext/CommandModule.bsl", c.name),
                    m,
                )?;
            }
        }
        for f in &o.forms {
            dump_form(cfg, o, f, sink)?;
        }
        for r in &o.recalculations {
            let Some(m) = &r.module else { continue };
            if let Some(f) = file_of_kind(m.kind) {
                put(sink, format!("{}/Ext/{f}", paths::recalc_dir(cfg, o, r)), m)?;
            }
        }
        stack.extend(o.subsystems.iter().rev());
    }
    for m in &cfg.modules {
        if let Some((_, _, file)) = CONFIG_MODULES.iter().find(|(_, k, _)| *k == m.kind) {
            put(sink, format!("Ext/{file}"), m)?;
        }
    }
    Ok(PartStatus::Written)
}
