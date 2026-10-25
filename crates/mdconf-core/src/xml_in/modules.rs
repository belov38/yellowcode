use super::{ObjectOptions, ObjectReport, ShellDump, walk::err};
use crate::{Child, ChildTag, Diagnostics, MdObject, Module, ModuleKind, Result};
use md_xmlout::codec::modules::CONFIG_MODULES;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

struct Reader<'a> {
    files: &'a BTreeMap<PathBuf, PathBuf>,
    options: &'a ObjectOptions,
    report: &'a mut ObjectReport,
    used: BTreeSet<PathBuf>,
}
impl Reader<'_> {
    fn module(&mut self, rel: &Path, kind: ModuleKind) -> Result<Option<Module>> {
        let Some(real) = self.files.get(rel) else {
            return Ok(None);
        };
        if !self.used.insert(rel.into()) {
            return Err(err(rel, "module referenced twice"));
        }
        let meta = std::fs::symlink_metadata(real).map_err(|e| err(real, e.to_string()))?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(err(real, "module is not a regular file"));
        }
        let cap = self.options.max_module_bytes.min(
            self.options
                .max_total_module_bytes
                .saturating_sub(self.report.module_bytes),
        );
        if meta.len() > cap || cap == u64::MAX {
            return Err(err(real, "module byte limit"));
        }
        let mut source = Vec::new();
        std::fs::File::open(real)
            .map_err(|e| err(real, e.to_string()))?
            .take(cap + 1)
            .read_to_end(&mut source)
            .map_err(|e| err(real, e.to_string()))?;
        if source.len() as u64 > cap {
            return Err(err(real, "module byte limit"));
        }
        self.report.module_bytes += source.len() as u64;
        self.report.modules += 1;
        Ok(Some(Module { kind, source }))
    }
    fn children(&mut self, children: &mut [Child], dir: &Path) -> Result<()> {
        for c in children {
            if c.tag == ChildTag::Command {
                c.module = self.module(
                    &dir.join("Commands")
                        .join(&c.name)
                        .join("Ext")
                        .join("CommandModule.bsl"),
                    ModuleKind::CommandModule,
                )?;
            }
        }
        Ok(())
    }
    fn object(&mut self, o: &mut MdObject, dir: &Path, diag: &mut Diagnostics) -> Result<()> {
        let s = mdschema::schema()?;
        for (_, file) in s.modules(o.kind.as_str()) {
            if let Some(kind) = crate::modules::kind_of_file(file) {
                o.modules
                    .extend(self.module(&dir.join("Ext").join(file), kind)?);
            } else {
                diag.unsupported
                    .push(format!("{}: unsupported module slot {file}", dir.display()));
            }
        }
        if o.kind == crate::MdKind::CommonForm {
            o.modules.extend(self.module(
                &dir.join("Ext").join("Form").join("Module.bsl"),
                ModuleKind::FormModule,
            )?);
        }
        self.children(&mut o.children, dir)?;
        for f in &mut o.forms {
            f.module = self.module(
                &dir.join("Forms")
                    .join(&f.name)
                    .join("Ext")
                    .join("Form")
                    .join("Module.bsl"),
                ModuleKind::FormModule,
            )?;
        }
        for r in &mut o.recalculations {
            r.module = self.module(
                &dir.join("Recalculations")
                    .join(&r.name)
                    .join("Ext")
                    .join("RecordSetModule.bsl"),
                ModuleKind::RecordSetModule,
            )?;
        }
        for child in &mut o.subsystems {
            let folder = s
                .folder(child.kind.as_str())
                .ok_or_else(|| err(dir, "missing nested folder"))?;
            self.object(child, &dir.join(folder).join(&child.name), diag)?;
        }
        Ok(())
    }
}
pub(super) fn load(
    dump: &mut ShellDump,
    options: &ObjectOptions,
    report: &mut ObjectReport,
) -> Result<()> {
    let mut r = Reader {
        files: &dump.files,
        options,
        report,
        used: BTreeSet::new(),
    };
    let s = mdschema::schema()?;
    for o in &mut dump.configuration.objects {
        let folder = s
            .folder(o.kind.as_str())
            .ok_or_else(|| err(Path::new(&o.name), "missing object folder"))?;
        r.object(
            o,
            &Path::new(folder).join(&o.name),
            &mut dump.configuration.diagnostics,
        )?;
    }
    for (_, kind, file) in CONFIG_MODULES {
        dump.configuration
            .modules
            .extend(r.module(&Path::new("Ext").join(file), kind)?);
    }
    r.report.unread_files = dump
        .report
        .unread_files
        .iter()
        .filter(|p| !r.used.contains(*p))
        .cloned()
        .collect();
    for p in &r.report.unread_files {
        if p.extension().is_some_and(|e| e == "bsl") {
            dump.configuration
                .diagnostics
                .unsupported
                .push(format!("{}: unowned module", p.display()));
        }
    }
    Ok(())
}
