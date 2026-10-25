use super::index::{GuidText, doc_tree, text};
use super::{ManagedIndex, Tree};
use crate::{
    CompatibilityMode, ConfigSettings, DataLockControlMode, ManagedEntries, ManagedEntriesError,
    ManagedEntrySource, MdRef, ObjectAutonumerationMode, ScriptVariant,
};
use mdschema::{StaticConfigPath, StaticConfigProp, static_schema};
use memory_budget::{BudgetError, Charged};

#[derive(Debug, thiserror::Error)]
pub enum ManagedSettingsError<E> {
    #[error("{0}")]
    Entries(#[from] ManagedEntriesError<E>),
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("{property}: {what}")]
    Format {
        property: &'static str,
        what: &'static str,
    },
}
type Result<T, E> = std::result::Result<T, ManagedSettingsError<E>>;
fn bad<E>(property: &'static str, what: &'static str) -> ManagedSettingsError<E> {
    ManagedSettingsError::Format { property, what }
}

#[derive(Debug)]
pub struct ManagedSettings;
impl ManagedSettings {
    pub fn read<S: ManagedEntrySource + ?Sized>(
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
    ) -> Result<Charged<ConfigSettings>, S::Error> {
        if !index.owns_scope(entries.memory_budget()) {
            return Err(BudgetError::Limit(memory_budget::MemoryLimit {
                category: memory_budget::Category::Metadata,
                requested: 0,
                reason: memory_budget::LimitReason::InvalidSize,
            })
            .into());
        }
        let entry = GuidText::new(index.cfg_id);
        let data = entries
            .doc(entry.as_str()?)?
            .ok_or_else(|| bad("Configuration", "not a brace document"))?;
        let raw = doc_tree(&data).ok_or_else(|| bad("Configuration", "not a brace document"))?;
        let tree = Tree::new("Configuration", raw);
        let body = tree
            .get(&[3, 1, 1])
            .ok_or_else(|| bad("Configuration", "no configuration body"))?;
        let header = body
            .get(&[1, 1])
            .ok_or_else(|| bad("Configuration", "no configuration header"))?;
        let name = header
            .get(&[2])
            .and_then(Tree::text)
            .ok_or_else(|| bad("Name", "no header[2]"))?;
        let script_variant = match decode_enum(body, "ScriptVariant")? {
            "English" => ScriptVariant::English,
            "Russian" => ScriptVariant::Russian,
            _ => return Err(bad("ScriptVariant", "unexpected schema enum value")),
        };
        let data_lock_control_mode = match decode_enum(body, "DataLockControlMode")? {
            "Automatic" => DataLockControlMode::Automatic,
            "Managed" => DataLockControlMode::Managed,
            "AutomaticAndManaged" => DataLockControlMode::AutomaticAndManaged,
            _ => return Err(bad("DataLockControlMode", "unexpected schema enum value")),
        };
        let object_autonumeration_mode = match decode_enum(body, "ObjectAutonumerationMode")? {
            "AutoFree" => ObjectAutonumerationMode::AutoFree,
            "NotAutoFree" => ObjectAutonumerationMode::NotAutoFree,
            _ => {
                return Err(bad(
                    "ObjectAutonumerationMode",
                    "unexpected schema enum value",
                ));
            }
        };
        let (compat, _) = spec_node(body, "CompatibilityMode", "compat")?;
        let compatibility_mode = CompatibilityMode(
            compat
                .as_u32()
                .ok_or_else(|| bad("CompatibilityMode", "not a number"))?,
        );
        let (language, _) = spec_node(body, "DefaultLanguage", "ref")?;
        let id = language
            .as_guid()
            .ok_or_else(|| bad("DefaultLanguage", "not a uuid"))?;
        let language = index
            .names
            .get(&id)
            .ok_or_else(|| bad("DefaultLanguage", "unknown object uuid"))?
            .as_str();
        let name = text(entries.memory_budget(), &[name])?.into_string()?;
        let language = text(entries.memory_budget(), &[language])?.into_string()?;
        Ok(name
            .try_zip(language)?
            .map(|(name, language)| ConfigSettings {
                name,
                script_variant,
                default_language: MdRef {
                    id: Some(id),
                    name: language,
                },
                compatibility_mode,
                data_lock_control_mode,
                object_autonumeration_mode,
            }))
    }
}
fn spec_node<'a, E>(
    body: Tree<'a>,
    property: &'static str,
    codec: &'static str,
) -> Result<(Tree<'a>, StaticConfigProp), E> {
    let spec = static_schema()
        .config_prop(property)
        .ok_or_else(|| bad(property, "no schema entry"))?;
    if spec.codec() != codec {
        return Err(bad(property, "unexpected schema codec"));
    }
    let Some(StaticConfigPath::Body(path)) = spec.path() else {
        return Err(bad(property, "not a body path"));
    };
    let value = body
        .get_path(path.iter())
        .ok_or_else(|| bad(property, "no value at schema path"))?;
    Ok((value, spec))
}
fn decode_enum<E>(body: Tree<'_>, property: &'static str) -> Result<&'static str, E> {
    let (node, spec) = spec_node(body, property, "enum")?;
    let code = node.text().ok_or_else(|| bad(property, "not a scalar"))?;
    spec.extra()
        .get("map")
        .and_then(|map| map.get(code))
        .and_then(|value| value.as_str())
        .ok_or_else(|| bad(property, "unknown enum code"))
}

pub fn read_compat_version<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
) -> Result<u32, S::Error> {
    read_configuration_version(entries).map(|(_, compat)| compat)
}

pub(crate) fn read_configuration_version<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
) -> Result<(mdschema::ConfigFormat, u32), S::Error> {
    let data = entries
        .doc("version")?
        .ok_or_else(|| bad("version", "missing brace document"))?;
    let raw = doc_tree(&data).ok_or_else(|| bad("version", "not a brace document"))?;
    let number = |path: &[usize], what| {
        raw.get(path)
            .and_then(brace::Node::as_u32)
            .ok_or_else(|| bad("version", what))
    };
    let format = mdschema::ConfigFormat {
        version: number(&[0, 0], "no configuration format")?,
        revision: number(&[0, 1], "no configuration format revision")?,
    };
    Ok((format, number(&[0, 2, 0], "no compatibility version")?))
}
