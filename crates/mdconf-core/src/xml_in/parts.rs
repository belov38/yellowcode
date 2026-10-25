use super::{
    shell_xml::{self, MD, N},
    walk::err,
    *,
};
use crate::{
    CompatibilityMode, ConfigSettings, DataLockControlMode, LoadProfile, MdRef,
    ObjectAutonumerationMode, ScriptVariant,
};
use md_model::xml_origin::{XmlOrigin, XmlPart};
use memory_budget::{Limits, MemoryBudget};
use std::{collections::BTreeMap, io::Read, path::Path};

#[derive(Debug, Clone)]
pub struct PartsOptions {
    pub objects: ObjectOptions,
    pub max_retained_file_bytes: u64,
    pub max_retained_bytes: u64,
}
impl Default for PartsOptions {
    fn default() -> Self {
        Self {
            objects: ObjectOptions::default(),
            max_retained_file_bytes: 64 << 20,
            max_retained_bytes: 512 << 20,
        }
    }
}
#[derive(Debug)]
pub struct PartsDump {
    pub configuration: Configuration,
    pub object_report: ObjectReport,
}

pub fn load_parts(dir: &Path, o: &PartsOptions) -> Result<PartsDump> {
    let dump = load_objects(dir, &o.objects)?;
    if !dump.shells.unsupported.is_empty() || !dump.shells.unreferenced_descriptions.is_empty() {
        return Err(err(
            dir,
            "unsupported or unreferenced metadata descriptions",
        ));
    }
    let paths = super::part_paths::Paths::new(&dump);
    let mut cfg = dump.configuration;
    let mut origin = XmlOrigin::new(dump.format);
    let mut retained = 0u64;
    for (rel, real) in &dump.files {
        let part = paths.classify(rel)?;
        let cap = o
            .max_retained_file_bytes
            .min(o.max_retained_bytes.saturating_sub(retained));
        let meta = std::fs::symlink_metadata(real).map_err(|e| err(real, e.to_string()))?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(err(real, "part is not a regular file"));
        }
        if meta.len() > cap || cap == u64::MAX {
            return Err(err(real, "retained XML file byte limit"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(real)
            .map_err(|e| err(real, e.to_string()))?
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| err(real, e.to_string()))?;
        if bytes.len() as u64 > cap {
            return Err(err(real, "retained XML byte limit"));
        }
        retained += bytes.len() as u64;
        if !origin.insert(rel.clone(), part, bytes) {
            return Err(err(rel, "invalid retained path"));
        }
    }
    let mut cx = Context::new(&origin, &cfg, o)?;
    cx.assembly.report.value_bytes = dump.report.value_bytes;
    configuration(&mut cfg, &mut cx)?;
    super::part_predefined::index(&cfg.objects, Path::new(""), &mut cx)?;
    for object in &mut cfg.objects {
        object_parts(object, Path::new(""), &mut cx)?;
    }
    super::part_misc::config_payloads(&mut cx, &mut cfg.parts)?;
    cfg.predef = cx.predef;
    let diagnostics = cx.assembly.reader.into_diagnostics();
    for (key, count) in diagnostics.misses {
        *cfg.diagnostics.misses.entry(key).or_default() += count;
    }
    cfg.diagnostics.unsupported.extend(diagnostics.unsupported);
    cfg.diagnostics.unsupported.retain(|s|s!="XML objects: parts and configuration settings are not loaded; unsuitable for full dumping");
    cfg.xml_origin = Some(origin);
    cfg.profile = LoadProfile::Query;
    Ok(PartsDump {
        configuration: cfg,
        object_report: dump.report,
    })
}

pub(super) struct Context<'a> {
    pub origin: &'a XmlOrigin,
    pub opts: &'a PartsOptions,
    pub names: BTreeMap<String, brace::Guid>,
    pub assembly: super::objects::Assembly,
    pub predef: std::collections::HashMap<brace::Guid, String>,
    pub budget: MemoryBudget,
    pub work: u64,
}
impl<'a> Context<'a> {
    fn new(origin: &'a XmlOrigin, cfg: &Configuration, opts: &'a PartsOptions) -> Result<Self> {
        Ok(Self {
            origin,
            opts,
            assembly: super::objects::Assembly {
                reader: ValueReader::new(cfg)?,
                report: Default::default(),
                max_value_bytes: opts.objects.max_value_bytes,
            },
            names: cfg
                .index
                .names
                .iter()
                .map(|(g, n)| (n.clone(), *g))
                .collect(),
            predef: Default::default(),
            budget: MemoryBudget::new(Limits::new(opts.objects.shells.max_xml_memory)),
            work: 0,
        })
    }
    pub fn arena<'s>(
        &mut self,
        path: &Path,
        bytes: &'s [u8],
    ) -> Result<crate::xml::view::XmlArena<'s>> {
        let arena = shell_xml::parse(
            bytes,
            path,
            &self.opts.objects.shells,
            &self.budget,
            &mut self.work,
        )?;
        let root = arena
            .root()
            .ok_or_else(|| err(path, "missing part XML root"))?;
        super::part_xml::validate(root, path)?;
        Ok(arena)
    }
    pub fn reference(&self, name: &str, path: &Path) -> Result<MdRef> {
        let id = self
            .names
            .get(name)
            .copied()
            .ok_or_else(|| err(path, format!("unresolved part reference {name}")))?;
        Ok(MdRef {
            id: Some(id),
            name: name.into(),
        })
    }
}
pub(super) fn text<'a>(n: N<'a, '_>, tag: &str) -> &'a str {
    n.children()
        .find(|c| c.local() == tag)
        .and_then(|c| c.text())
        .unwrap_or_default()
}
pub(super) fn boolean(n: N<'_, '_>, tag: &str, path: &Path) -> Result<bool> {
    match text(n, tag) {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(err(path, format!("invalid boolean {tag}"))),
    }
}
fn configuration(cfg: &mut Configuration, cx: &mut Context<'_>) -> Result<()> {
    let path = Path::new("Configuration.xml");
    let bytes = cx
        .origin
        .get(path)
        .ok_or_else(|| err(path, "missing configuration"))?
        .bytes();
    let arena = cx.arena(path, bytes)?;
    let root = arena
        .root()
        .ok_or_else(|| err(path, "missing XML root"))?
        .children()
        .find(|n| n.local() == "Configuration")
        .ok_or_else(|| err(path, "missing Configuration element"))?;
    let schema = mdschema::schema()?;
    let specs: Vec<_> = schema
        .config_props()
        .iter()
        .map(|s| {
            let codec = match s.codec.as_str() {
                "bool" => crate::Codec::Bool,
                "int" => crate::Codec::Int,
                "str" => crate::Codec::Str,
                "lstr" => crate::Codec::LStr,
                "ref" => crate::Codec::Ref,
                "mdlist" => crate::Codec::MdList,
                "purposes" => crate::Codec::Purposes,
                "enum" | "compat" | "const" => crate::Codec::Enum,
                _ => crate::Codec::default(),
            };
            (
                s.name.clone(),
                mdschema::PropSpec {
                    codec,
                    map: s
                        .extra
                        .get("map")
                        .and_then(|m| m.as_object())
                        .map(|m| {
                            m.iter()
                                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.into())))
                                .collect()
                        })
                        .unwrap_or_else(|| {
                            s.extra
                                .get("value")
                                .and_then(|v| v.as_str())
                                .map(|v| [("const".into(), v.into())].into_iter().collect())
                                .unwrap_or_default()
                        }),
                    ..Default::default()
                },
            )
        })
        .collect();
    cfg.props = cx.assembly.properties(
        root,
        &specs,
        "Configuration",
        "ConfigurationProperties",
        path,
    )?;
    let props = shell_xml::required(root, MD, "Properties", path)?;
    let mode = |name| shell_xml::leaf(shell_xml::required(props, MD, name, path)?, path);
    let variant = match mode("ScriptVariant")? {
        "Russian" => ScriptVariant::Russian,
        "English" => ScriptVariant::English,
        _ => return Err(err(path, "invalid ScriptVariant")),
    };
    let compat = mode("CompatibilityMode")?;
    let code = match compat {
        "Version8_2_16" => 0,
        "Version8_1" => 1,
        "Version8_2_13" => 2,
        _ => {
            let mut pieces = compat
                .strip_prefix("Version")
                .unwrap_or_default()
                .split('_');
            let mut number = 0u32;
            for factor in [10000u32, 100, 1] {
                number = number
                    .checked_add(
                        pieces
                            .next()
                            .and_then(|s| s.parse::<u32>().ok())
                            .and_then(|n| n.checked_mul(factor))
                            .ok_or_else(|| err(path, "invalid compatibility"))?,
                    )
                    .ok_or_else(|| err(path, "compatibility overflow"))?;
            }
            if pieces.next().is_some() {
                return Err(err(path, "invalid compatibility"));
            }
            number
        }
    };
    cx.names
        .insert(format!("Configuration.{}", mode("Name")?), cfg.id);
    cfg.settings = ConfigSettings {
        name: mode("Name")?.into(),
        script_variant: variant,
        default_language: if mode("DefaultLanguage")?.is_empty() {
            MdRef::default()
        } else {
            cx.reference(mode("DefaultLanguage")?, path)?
        },
        compatibility_mode: CompatibilityMode(code),
        data_lock_control_mode: match mode("DataLockControlMode")? {
            "Automatic" => DataLockControlMode::Automatic,
            "Managed" => DataLockControlMode::Managed,
            "AutomaticAndManaged" => DataLockControlMode::AutomaticAndManaged,
            _ => return Err(err(path, "invalid lock mode")),
        },
        object_autonumeration_mode: match mode("ObjectAutonumerationMode")? {
            "AutoFree" => ObjectAutonumerationMode::AutoFree,
            "NotAutoFree" => ObjectAutonumerationMode::NotAutoFree,
            _ => return Err(err(path, "invalid autonumeration")),
        },
    };
    if let Some(info) = root.children().find(|n| n.local() == "InternalInfo") {
        for part in info.children().filter(|n| n.local() == "ContainedObject") {
            let class = brace::Guid::parse(text(part, "ClassId"))
                .ok_or_else(|| err(path, "invalid contained class"))?;
            let object = brace::Guid::parse(text(part, "ObjectId"));
            cfg.parts.contained_objects.push((class, object));
        }
    }
    Ok(())
}
fn object_parts(object: &mut crate::MdObject, parent: &Path, cx: &mut Context<'_>) -> Result<()> {
    let folder = object
        .kind
        .folder()
        .ok_or_else(|| err(parent, "missing object folder"))?;
    let dir = parent.join(folder).join(&object.name);
    let ext = dir.join("Ext");
    for stem in [
        "Rights.xml",
        "Predefined.xml",
        "Content.xml",
        "Schedule.xml",
    ] {
        let path = ext.join(stem);
        if let Some(file) = cx.origin.get(&path) {
            let admitted = match stem {
                "Rights.xml" => file.part() == XmlPart::Rights,
                "Predefined.xml" => file.part() == XmlPart::Predefined,
                "Content.xml" => {
                    object.kind == crate::MdKind::ExchangePlan && file.part() == XmlPart::Misc
                }
                "Schedule.xml" => {
                    object.kind == crate::MdKind::ScheduledJob && file.part() == XmlPart::Misc
                }
                _ => false,
            };
            if !admitted {
                continue;
            }
            let arena = cx.arena(&path, file.bytes())?;
            match stem {
                "Rights.xml" => {
                    object.parts.rights = Some(super::part_rights::read(
                        cx,
                        arena.root().ok_or_else(|| err(&path, "missing XML root"))?,
                        &path,
                    )?)
                }
                "Predefined.xml" => {
                    object.parts.predefined = Some(super::part_predefined::read(
                        cx,
                        object,
                        arena.root().ok_or_else(|| err(&path, "missing XML root"))?,
                        &path,
                    )?)
                }
                "Content.xml" => {
                    object.parts.exchange_content = Some(super::part_misc::content(
                        cx,
                        arena.root().ok_or_else(|| err(&path, "missing XML root"))?,
                        &path,
                    )?)
                }
                "Schedule.xml" => {
                    object.parts.schedule = Some(super::part_misc::schedule(
                        arena.root().ok_or_else(|| err(&path, "missing XML root"))?,
                        &path,
                    )?)
                }
                _ => {}
            }
        }
    }
    super::part_misc::payloads(cx, &ext, &mut object.parts, object.kind)?;
    for form in &object.forms {
        if let Some(help) =
            super::part_misc::help(cx, &dir.join("Forms").join(&form.name).join("Ext"))
        {
            object.parts.form_help.push((form.id, help));
        }
    }
    for child in &mut object.subsystems {
        object_parts(child, &dir, cx)?;
    }
    Ok(())
}
