use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use mdconf::xml_out::{DumpOptions, Part, dump};
use mdconf::{Child, Configuration, ExternalObject, LoadOptions, MdObject, ModuleKind, SubObject};
use serde::Serialize;
use serde_json::json;
use unicode_normalization::UnicodeNormalization;

use super::metadata::Format;
use super::module::write_json;
use super::modules::resolve_module;
use super::scope::{Scope, ScopeArgs};
use crate::viewer::names::{child_tag_en, fold_path_name, sub_kind_en};
use crate::viewer::path::{self, RECALC_EN};
use crate::viewer::source;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportedFile {
    pub file: String,
    pub bytes: u64,
    pub part: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Exported {
    pub files: Vec<ExportedFile>,
    pub modules: usize,
    pub templates: usize,
}

fn prune_child(owner: &str, c: &mut Child, contains: &dyn Fn(&str) -> bool) -> bool {
    let path = format!("{owner}.{}.{}", child_tag_en(c.tag), c.name);
    let own = contains(&path);
    if !own {
        c.module = None;
    }
    let mut any = own;
    for c2 in &mut c.children {
        any |= prune_child(&path, c2, contains);
    }
    any
}

fn prune_parts(path: &str, o: &mut MdObject, contains: &dyn Fn(&str) -> bool) -> bool {
    o.template = None;
    o.form = None;
    o.flowchart = None;
    o.external.clear();
    let mut any = false;
    for c in &mut o.children {
        any |= prune_child(path, c, contains);
    }
    for list in [&mut o.forms, &mut o.templates] {
        list.retain(|s| contains(&format!("{path}.{}.{}", sub_kind_en(s.kind), s.name)));
        any |= !list.is_empty();
    }
    for r in &mut o.recalculations {
        if contains(&format!("{path}.{RECALC_EN}.{}", r.name)) {
            any = true;
        } else {
            r.module = None;
        }
    }
    any
}

fn prune_object(path: &str, o: &mut MdObject, scope: &Scope) -> bool {
    o.subsystems.retain_mut(|s| {
        let nested = format!("{path}.Subsystem.{}", s.name);
        prune_object(&nested, s, scope)
    });
    if scope.contains(path) {
        return true;
    }
    let mut any = !o.subsystems.is_empty();
    o.modules.clear();
    any |= prune_parts(path, o, &|p| scope.contains(p));
    any
}

pub fn prune(cfg: &mut Configuration, scope: &Scope) {
    cfg.xml_origin = None;
    if !scope.contains("Configuration") {
        cfg.modules.clear();
    }
    if scope.is_all() {
        return;
    }
    cfg.objects.retain_mut(|o| {
        let path = format!("{}.{}", o.kind.as_str(), o.name);
        prune_object(&path, o, scope)
    });
}

fn segments(path: &str) -> Vec<String> {
    path.split('.').map(fold_path_name).collect()
}

fn restrict_to_modules(
    cfg: &mut Configuration,
    modules: &[(String, ModuleKind)],
    elements: &[String],
) {
    let mut wanted: HashMap<Vec<String>, Vec<ModuleKind>> = HashMap::new();
    for (owner, kind) in modules {
        wanted.entry(segments(owner)).or_default().push(*kind);
    }
    let elements: Vec<Vec<String>> = elements.iter().map(|e| segments(e)).collect();
    if let Some(kinds) = wanted.get(&segments("Configuration")) {
        cfg.modules.retain(|m| kinds.contains(&m.kind));
    }
    for o in &mut cfg.objects {
        let path = format!("{}.{}", o.kind.as_str(), o.name);
        let own = segments(&path);
        if let Some(kinds) = wanted.get(&own) {
            o.modules.retain(|m| kinds.contains(&m.kind));
            let covered = |p: &str| {
                let segs = segments(p);
                elements.iter().any(|e| segs.starts_with(e))
                    || wanted
                        .keys()
                        .any(|k| k.len() > own.len() && k.starts_with(&own) && segs.starts_with(k))
            };
            prune_parts(&path, o, &covered);
        }
    }
}

fn child_carries(c: &Child) -> bool {
    c.module.is_some() || c.children.iter().any(child_carries)
}

fn sub_carries(s: &SubObject) -> bool {
    s.module.is_some() || s.template.is_some()
}

fn external_carries(e: &ExternalObject) -> bool {
    !e.modules.is_empty()
        || e.children.iter().any(child_carries)
        || e.forms.iter().chain(&e.templates).any(sub_carries)
        || e.nested.iter().any(external_carries)
}

fn object_carries(o: &MdObject) -> bool {
    !o.modules.is_empty()
        || o.children.iter().any(child_carries)
        || o.forms.iter().chain(&o.templates).any(sub_carries)
        || o.recalculations.iter().any(|r| r.module.is_some())
        || o.external.iter().any(external_carries)
        || o.subsystems.iter().any(object_carries)
}

#[derive(Default)]
struct NameTwins {
    seen: HashMap<(String, String), String>,
}

impl NameTwins {
    fn take(&mut self, group: &str, name: &mut String) -> crate::Result<()> {
        let nfc: String = name.nfc().collect();
        match self.seen.get(&(group.to_string(), nfc.clone())) {
            Some(raw) => {
                return Err(format!(
                    "{group}: {raw:?} and {name:?} are one name in NFC and would share a file path"
                )
                .into());
            }
            None => {
                self.seen
                    .insert((group.to_string(), nfc.clone()), name.clone());
            }
        }
        *name = nfc;
        Ok(())
    }

    fn take_if(&mut self, carries: bool, group: &str, name: &mut String) -> crate::Result<()> {
        if carries {
            self.take(group, name)
        } else {
            *name = name.nfc().collect();
            Ok(())
        }
    }

    fn children(&mut self, group: &str, children: &mut [Child]) -> crate::Result<()> {
        for c in children {
            let g = format!("{group}/{}", child_tag_en(c.tag));
            self.take_if(c.module.is_some(), &g, &mut c.name)?;
            self.children(&format!("{g}/{}", c.name), &mut c.children)?;
        }
        Ok(())
    }

    fn subs(&mut self, group: &str, list: &mut [SubObject]) -> crate::Result<()> {
        for s in list {
            let g = format!("{group}/{}", sub_kind_en(s.kind));
            self.take_if(s.module.is_some() || s.template.is_some(), &g, &mut s.name)?;
        }
        Ok(())
    }

    fn external(&mut self, group: &str, e: &mut ExternalObject) -> crate::Result<()> {
        let group = format!("{group}/{}", e.kind.folder());
        self.take_if(external_carries(e), &group, &mut e.name)?;
        let own = format!("{group}/{}", e.name);
        self.children(&own, &mut e.children)?;
        self.subs(&own, &mut e.forms)?;
        self.subs(&own, &mut e.templates)?;
        for n in &mut e.nested {
            self.external(&format!("{own}/Nested"), n)?;
        }
        Ok(())
    }

    fn object(&mut self, group: &str, o: &mut MdObject) -> crate::Result<()> {
        self.take_if(object_carries(o), group, &mut o.name)?;
        let own = format!("{group}/{}", o.name);
        for s in &mut o.subsystems {
            self.object(&format!("{own}/Subsystem"), s)?;
        }
        self.children(&own, &mut o.children)?;
        self.subs(&own, &mut o.forms)?;
        self.subs(&own, &mut o.templates)?;
        for r in &mut o.recalculations {
            let g = format!("{own}/{RECALC_EN}");
            self.take_if(r.module.is_some(), &g, &mut r.name)?;
        }
        for e in &mut o.external {
            self.external(&format!("{own}/External"), e)?;
        }
        Ok(())
    }
}

fn normalize_names(cfg: &mut Configuration) -> crate::Result<()> {
    let mut twins = NameTwins::default();
    for o in &mut cfg.objects {
        let group = o.kind.as_str().to_string();
        twins.object(&group, o)?;
    }
    Ok(())
}

fn check_output_dir(out: &Path) -> crate::Result<()> {
    match std::fs::metadata(out) {
        Ok(m) if m.is_dir() => {
            let used = std::fs::read_dir(out)
                .map_err(|e| format!("{}: {e}", out.display()))?
                .next()
                .is_some();
            if used {
                return Err(format!("output directory is not empty: {}", out.display()).into());
            }
            Ok(())
        }
        Ok(_) => Err(format!("output is not a directory: {}", out.display()).into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(out).is_ok() {
                return Err(format!("output is not a directory: {}", out.display()).into());
            }
            Ok(())
        }
        Err(e) => Err(format!("{}: {e}", out.display()).into()),
    }
}

fn list_files(dir: &Path) -> crate::Result<Vec<ExportedFile>> {
    let mut found = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((path, prefix)) = stack.pop() {
        for entry in std::fs::read_dir(&path).map_err(|e| format!("{}: {e}", path.display()))? {
            let entry = entry.map_err(|e| format!("{}: {e}", path.display()))?;
            let name: String = entry.file_name().to_string_lossy().nfc().collect();
            let rel = format!("{prefix}{name}");
            let meta = entry
                .metadata()
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
            if meta.is_dir() {
                stack.push((entry.path(), format!("{rel}/")));
            } else {
                let part = if rel.ends_with(".bsl") {
                    "modules"
                } else {
                    "templates"
                };
                found.push(ExportedFile {
                    file: rel,
                    bytes: meta.len(),
                    part,
                });
            }
        }
    }
    found.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(found)
}

pub fn export(cfg: Configuration, scope: &Scope, out: &Path) -> crate::Result<Exported> {
    export_with_modules(cfg, scope, &[], &[], out)
}

pub fn export_with_modules(
    mut cfg: Configuration,
    scope: &Scope,
    modules: &[(String, ModuleKind)],
    elements: &[String],
    out: &Path,
) -> crate::Result<Exported> {
    check_output_dir(out)?;
    prune(&mut cfg, scope);
    restrict_to_modules(&mut cfg, modules, elements);
    normalize_names(&mut cfg)?;
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    dump(
        &cfg,
        out,
        &DumpOptions::only(&[Part::Modules, Part::Templates]),
    )?;
    let files = list_files(out)?;
    let modules = files.iter().filter(|f| f.part == "modules").count();
    let templates = files.len() - modules;
    Ok(Exported {
        files,
        modules,
        templates,
    })
}

pub const MANIFEST: &str = "manifest.json";

#[derive(Serialize)]
struct ManifestScope<'a> {
    kinds: &'a [String],
    paths: &'a [String],
    subsystems: &'a [String],
}

#[derive(Serialize)]
struct Manifest<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    source: String,
    #[serde(rename = "yellowcodeVersion")]
    yellowcode_version: &'a str,
    scope: ManifestScope<'a>,
    modules: usize,
    templates: usize,
    files: &'a [ExportedFile],
}

fn write_manifest(
    output: &Path,
    source: &Path,
    scope: &ScopeArgs,
    exported: &Exported,
) -> crate::Result<()> {
    let manifest = Manifest {
        schema_version: 1,
        source: source.display().to_string(),
        yellowcode_version: crate::VERSION,
        scope: ManifestScope {
            kinds: &scope.kinds,
            paths: &scope.paths,
            subsystems: &scope.subsystems,
        },
        modules: exported.modules,
        templates: exported.templates,
        files: &exported.files,
    };
    let mut text = serde_json::to_string_pretty(&manifest)?;
    text.push('\n');
    let temp = output.join(format!("{MANIFEST}.tmp"));
    let written =
        std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, output.join(MANIFEST)));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("export to {} is incomplete: {e}", output.display()).into());
    }
    Ok(())
}

struct SplitPaths {
    args: ScopeArgs,
    modules: Vec<(String, ModuleKind)>,
    elements: Vec<String>,
}

fn split_module_addresses(cfg: &Configuration, args: &ScopeArgs) -> crate::Result<SplitPaths> {
    let mut paths = Vec::new();
    let mut elements = Vec::new();
    let mut modules = Vec::new();
    for p in &args.paths {
        match path::resolve(cfg, p) {
            Ok(t) => {
                elements.push(path::canonical(cfg, &t));
                paths.push(p.clone());
            }
            Err(e) => match resolve_module(cfg, p) {
                Ok(m) => {
                    paths.push(m.owner.clone());
                    modules.push((m.owner, m.kind));
                }
                Err(m) if m.to_string().starts_with("no module ") => {
                    return Err(format!("{e}; {m}").into());
                }
                Err(_) => return Err(e.into()),
            },
        }
    }
    let covers: Vec<Vec<String>> = elements.iter().map(|e| segments(e)).collect();
    modules.retain(|(owner, _)| {
        let segs = segments(owner);
        !covers.iter().any(|e| segs.starts_with(e))
    });
    Ok(SplitPaths {
        args: ScopeArgs {
            paths,
            ..args.clone()
        },
        modules,
        elements,
    })
}

pub fn run_with_memory_limit(
    src: PathBuf,
    scope: ScopeArgs,
    output: PathBuf,
    format: Format,
    limit: Option<String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> crate::Result<()> {
    check_output_dir(&output)?;
    let memory = source::OpenMemory::resolve(limit)?;
    let cfg = source::open_with_memory(&src, &LoadOptions::default(), &memory, &mut |_| Ok(()))?;
    let split = split_module_addresses(&cfg, &scope)?;
    let resolved = Scope::resolve(&cfg, &split.args)?;
    let exported = export_with_modules(cfg, &resolved, &split.modules, &split.elements, &output)
        .map_err(|e| format!("export to {} is incomplete: {e}", output.display()))?;
    write_manifest(&output, &src, &scope, &exported)?;
    if exported.files.is_empty() {
        writeln!(err, "yellowcode: nothing to export in the scope")?;
    }
    let bytes: u64 = exported.files.iter().map(|f| f.bytes).sum();
    match format {
        Format::Text => writeln!(
            out,
            "exported {} modules and {} template files ({bytes} bytes) to {}",
            exported.modules,
            exported.templates,
            output.display()
        )?,
        Format::Json => write_json(
            out,
            &json!({
                "schemaVersion": 1,
                "output": output.display().to_string(),
                "modules": exported.modules,
                "templates": exported.templates,
                "bytes": bytes,
            }),
        )?,
    }
    Ok(())
}
