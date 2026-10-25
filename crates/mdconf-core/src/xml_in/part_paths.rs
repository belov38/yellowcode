use super::{ObjectDump, walk::err};
use crate::{MdKind, MdObject, Result};
use md_model::xml_origin::XmlPart;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Clone, Copy)]
enum Owner {
    Config,
    Object(MdKind),
    Form,
    Template,
    Other,
}
pub(super) struct Paths {
    owners: BTreeMap<PathBuf, Owner>,
    read: BTreeSet<PathBuf>,
}
impl Paths {
    pub fn new(dump: &ObjectDump) -> Self {
        fn objects(list: &[MdObject], parent: &Path, out: &mut BTreeMap<PathBuf, Owner>) {
            for o in list {
                let Some(folder) = o.kind.folder() else {
                    continue;
                };
                let dir = parent.join(folder).join(&o.name);
                out.insert(dir.clone(), Owner::Object(o.kind));
                for f in &o.forms {
                    out.insert(dir.join("Forms").join(&f.name), Owner::Form);
                }
                for t in &o.templates {
                    out.insert(dir.join("Templates").join(&t.name), Owner::Template);
                }
                for r in &o.recalculations {
                    out.insert(dir.join("Recalculations").join(&r.name), Owner::Other);
                }
                for c in o.commands() {
                    out.insert(dir.join("Commands").join(&c.name), Owner::Other);
                }
                objects(&o.subsystems, &dir, out);
            }
        }
        let mut owners = BTreeMap::new();
        owners.insert(PathBuf::new(), Owner::Config);
        objects(&dump.configuration.objects, Path::new(""), &mut owners);
        let unread: BTreeSet<_> = dump.report.unread_files.iter().cloned().collect();
        Self {
            owners,
            read: dump
                .files
                .keys()
                .filter(|p| !unread.contains(*p))
                .cloned()
                .collect(),
        }
    }
    pub fn classify(&self, path: &Path) -> Result<XmlPart> {
        if path == Path::new("Configuration.xml") || path == Path::new("ConfigDumpInfo.xml") {
            return Ok(XmlPart::Configuration);
        }
        if self.read.contains(path) {
            return Ok(if path.extension().is_some_and(|e| e == "bsl") {
                XmlPart::Modules
            } else {
                XmlPart::Objects
            });
        }
        let (owner, rel) = path
            .ancestors()
            .skip(1)
            .find_map(|ext| {
                if ext.file_name()? != "Ext" {
                    return None;
                }
                let owner = self.owners.get(ext.parent()?)?;
                Some((owner, path.strip_prefix(ext).ok()?))
            })
            .ok_or_else(|| err(path, "unowned part path"))?;
        let mut names = rel.iter();
        let stem = names.next().and_then(|s| s.to_str()).unwrap_or_default();
        let nested = names.next().is_some();
        let object = |kind| matches!(owner,Owner::Object(k)if *k==kind);
        let config = matches!(owner, Owner::Config);
        let part = match stem {
            "Rights.xml" if object(MdKind::Role) && !nested => XmlPart::Rights,
            "Predefined.xml"
                if matches!(
                    owner,
                    Owner::Object(
                        MdKind::Catalog
                            | MdKind::ChartOfCharacteristicTypes
                            | MdKind::ChartOfAccounts
                            | MdKind::ChartOfCalculationTypes
                    )
                ) && !nested =>
            {
                XmlPart::Predefined
            }
            "Content.xml" if object(MdKind::ExchangePlan) && !nested => XmlPart::Misc,
            "Schedule.xml" if object(MdKind::ScheduledJob) && !nested => XmlPart::Misc,
            "Style.xml" if object(MdKind::Style) && !nested => XmlPart::Misc,
            "Form.xml" | "Form"
                if (!nested || stem == "Form")
                    && (matches!(owner, Owner::Form) || object(MdKind::CommonForm)) =>
            {
                XmlPart::Forms
            }
            s if (s == "Template" || s.starts_with("Template."))
                && (matches!(owner, Owner::Template) || object(MdKind::CommonTemplate)) =>
            {
                XmlPart::Templates
            }
            "Flowchart.xml" if object(MdKind::BusinessProcess) && !nested => XmlPart::Flowcharts,
            "CommandInterface.xml" if (config || object(MdKind::Subsystem)) && !nested => {
                XmlPart::CommandInterface
            }
            "MainSectionCommandInterface.xml"
            | "HomePageWorkArea.xml"
            | "ClientApplicationInterface.xml"
                if config && !nested =>
            {
                XmlPart::CommandInterface
            }
            "Help.xml" if !nested => XmlPart::Help,
            "Help" if nested => XmlPart::Help,
            "Picture.xml" | "Picture"
                if object(MdKind::CommonPicture) && (!nested || stem == "Picture") =>
            {
                XmlPart::Pictures
            }
            "Logo.xml"
            | "Logo"
            | "Splash.xml"
            | "Splash"
            | "MainSectionPicture.xml"
            | "MainSectionPicture"
                if config && (!nested || !stem.ends_with(".xml")) =>
            {
                XmlPart::Pictures
            }
            "ParentConfigurations.bin" | "MobileClientSignature.bin" if config && !nested => {
                XmlPart::Misc
            }
            "Package.bin" if object(MdKind::XDTOPackage) && !nested => XmlPart::Misc,
            _ if object(MdKind::WSReference) && !nested => XmlPart::Misc,
            s if !nested && matches!(owner, Owner::Object(_)) && s.ends_with(".bin") => {
                let Owner::Object(kind) = owner else {
                    return Err(err(path, "missing module owner"));
                };
                if !mdschema::schema()?
                    .modules(kind.as_str())
                    .iter()
                    .any(|(_, f)| {
                        f.strip_suffix(".bsl")
                            .is_some_and(|f| s == format!("{f}.bin"))
                    })
                {
                    return Err(err(path, "unknown binary module"));
                }
                XmlPart::Misc
            }
            _ => return Err(err(path, "unknown XML part path")),
        };
        Ok(part)
    }
}
