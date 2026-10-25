use crate::Configuration;
use crate::parts::configdump::Names;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryCategory {
    Root,
    Configuration,
    Object,
    Module,
    FormBody,
    TemplateBody,
    Flowchart,
    Rights,
    CommandInterface,
    Predefined,
    Schedule,
    Content,
    Style,
    Picture,
    Help,
    Package,
    Binary,
    Other,
}

impl EntryCategory {
    pub const ALL: [EntryCategory; 18] = [
        Self::Root,
        Self::Configuration,
        Self::Object,
        Self::Module,
        Self::FormBody,
        Self::TemplateBody,
        Self::Flowchart,
        Self::Rights,
        Self::CommandInterface,
        Self::Predefined,
        Self::Schedule,
        Self::Content,
        Self::Style,
        Self::Picture,
        Self::Help,
        Self::Package,
        Self::Binary,
        Self::Other,
    ];

    pub fn parse(s: &str) -> Option<EntryCategory> {
        Self::ALL
            .into_iter()
            .find(|c| c.name().eq_ignore_ascii_case(s))
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Root => "Root",
            Self::Configuration => "Configuration",
            Self::Object => "Object",
            Self::Module => "Module",
            Self::FormBody => "FormBody",
            Self::TemplateBody => "TemplateBody",
            Self::Flowchart => "Flowchart",
            Self::Rights => "Rights",
            Self::CommandInterface => "CommandInterface",
            Self::Predefined => "Predefined",
            Self::Schedule => "Schedule",
            Self::Content => "Content",
            Self::Style => "Style",
            Self::Picture => "Picture",
            Self::Help => "Help",
            Self::Package => "Package",
            Self::Binary => "Binary",
            Self::Other => "Other",
        }
    }
}

fn of_element(sub: &str) -> Option<EntryCategory> {
    use EntryCategory::*;
    Some(match sub {
        "Form" => FormBody,
        "Template" => TemplateBody,
        "Flowchart" => Flowchart,
        "Rights" => Rights,
        "CommandInterface"
        | "MainSectionCommandInterface"
        | "ClientApplicationInterface"
        | "HomePageWorkArea" => CommandInterface,
        "Predefined" => Predefined,
        "Schedule" => Schedule,
        "Content" => Content,
        "Style" => Style,
        "Picture" | "Logo" | "Splash" | "MainSectionPicture" => Picture,
        "Help" => Help,
        "Package" | "WSDefinition" => Package,
        "ParentConfigurations" | "MobileClientSignature" => Binary,
        "Module"
        | "ObjectModule"
        | "ManagerModule"
        | "RecordSetModule"
        | "ValueManagerModule"
        | "CommandModule"
        | "OrdinaryApplicationModule"
        | "ExternalConnectionModule"
        | "ManagedApplicationModule"
        | "SessionModule" => Module,
        _ => return None,
    })
}

pub(crate) fn classify(
    cfg: &Configuration,
    names: &[String],
    module_slots: &HashSet<&str>,
) -> crate::Result<BTreeMap<String, (EntryCategory, Option<String>)>> {
    let resolver = Names::new(cfg).map_err(|what| crate::Error::Format {
        entry: String::new(),
        what,
    })?;
    let (cfg_id, header_id) = (cfg.id.to_string(), cfg.header_id.to_string());
    let mut out = BTreeMap::new();
    for name in names {
        let class = if matches!(name.as_str(), "root" | "version" | "versions" | "deleted") {
            (EntryCategory::Root, None)
        } else if module_slots.contains(name.as_str()) {
            (EntryCategory::Module, None)
        } else if *name == cfg_id || *name == header_id {
            (EntryCategory::Configuration, None)
        } else {
            match resolver.element(name) {
                Ok(_) if !name.contains('.') => (EntryCategory::Object, None),
                Ok(full) => {
                    let sub = full.rsplit('.').next().unwrap_or_default();
                    match of_element(sub) {
                        Some(category) => (category, None),
                        None => (EntryCategory::Other, Some(sub.to_string())),
                    }
                }
                Err(reason) => (EntryCategory::Other, Some(reason)),
            }
        };
        out.insert(name.clone(), class);
    }
    Ok(out)
}
