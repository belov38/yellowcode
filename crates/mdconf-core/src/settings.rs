pub use md_model::{
    CompatibilityMode, ConfigSettings, DataLockControlMode, ObjectAutonumerationMode, ScriptVariant,
};

use crate::{Error, MdRef, Result};
use brace::{Guid, Node};
use mdschema::{ConfigPath, Schema};
use std::collections::HashMap;

const ENTRY: &str = "configuration";

fn bad(what: String) -> Error {
    Error::Format {
        entry: ENTRY.into(),
        what,
    }
}

fn spec_node<'a, 's>(
    schema: &'s Schema,
    body: &'a Node,
    name: &str,
    codec: &str,
) -> Result<(&'a Node, &'s mdschema::ConfigPropSpec)> {
    let spec = schema
        .config_prop(name)
        .ok_or_else(|| bad(format!("{name}: no schema entry")))?;
    if spec.codec != codec {
        return Err(bad(format!(
            "{name}: codec {} (expected {codec})",
            spec.codec
        )));
    }
    let Some(ConfigPath::Body(path)) = &spec.path else {
        return Err(bad(format!("{name}: not a body path")));
    };
    let node = body
        .get(path)
        .ok_or_else(|| bad(format!("{name}: no value at {path:?}")))?;
    Ok((node, spec))
}

fn decode_enum(schema: &Schema, body: &Node, name: &str) -> Result<String> {
    let (node, spec) = spec_node(schema, body, name, "enum")?;
    let code = node
        .text()
        .ok_or_else(|| bad(format!("{name}: not a scalar")))?;
    spec.extra
        .get("map")
        .and_then(|m| m.get(code))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| bad(format!("{name}: unknown code {code:?}")))
}

pub(crate) fn decode_settings(
    schema: &Schema,
    body: &Node,
    header: &Node,
    names: &HashMap<Guid, String>,
) -> Result<ConfigSettings> {
    let name = header
        .at(2)
        .and_then(Node::text)
        .ok_or_else(|| bad("Name: no header[2]".into()))?
        .to_owned();

    let sv = decode_enum(schema, body, "ScriptVariant")?;
    let script_variant = match sv.as_str() {
        "English" => ScriptVariant::English,
        "Russian" => ScriptVariant::Russian,
        other => return Err(bad(format!("ScriptVariant: unexpected {other:?}"))),
    };
    let dl = decode_enum(schema, body, "DataLockControlMode")?;
    let data_lock_control_mode = match dl.as_str() {
        "Automatic" => DataLockControlMode::Automatic,
        "Managed" => DataLockControlMode::Managed,
        "AutomaticAndManaged" => DataLockControlMode::AutomaticAndManaged,
        other => return Err(bad(format!("DataLockControlMode: unexpected {other:?}"))),
    };
    let an = decode_enum(schema, body, "ObjectAutonumerationMode")?;
    let object_autonumeration_mode = match an.as_str() {
        "AutoFree" => ObjectAutonumerationMode::AutoFree,
        "NotAutoFree" => ObjectAutonumerationMode::NotAutoFree,
        other => {
            return Err(bad(format!(
                "ObjectAutonumerationMode: unexpected {other:?}"
            )));
        }
    };

    let (node, _) = spec_node(schema, body, "CompatibilityMode", "compat")?;
    let compatibility_mode = CompatibilityMode(
        node.as_u32()
            .ok_or_else(|| bad("CompatibilityMode: not a number".into()))?,
    );

    let (node, _) = spec_node(schema, body, "DefaultLanguage", "ref")?;
    let id = node
        .as_guid()
        .ok_or_else(|| bad("DefaultLanguage: not a uuid".into()))?;
    let lang = names
        .get(&id)
        .ok_or_else(|| bad(format!("DefaultLanguage: unknown object {id}")))?;
    let default_language = MdRef {
        id: Some(id),
        name: lang.clone(),
    };

    Ok(ConfigSettings {
        name,
        script_variant,
        default_language,
        compatibility_mode,
        data_lock_control_mode,
        object_autonumeration_mode,
    })
}
