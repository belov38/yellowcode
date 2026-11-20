use crate::extension_header::{ExtensionHeader, HeaderError, WireInteger};
use brace::{Guid, Node};
use mdschema::StaticConfigPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationExtensionPurpose {
    Patch,
    Customization,
    AddOn,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigurationError {
    #[error("extension configuration: expected root with seven parts and a trailer")]
    RootShape,
    #[error("extension configuration: expected integer root version")]
    RootVersion,
    #[error("extension configuration: unsupported root version (expected 2)")]
    UnsupportedRootVersion,
    #[error("extension configuration: expected singleton root-entry GUID")]
    RootIdentity,
    #[error("extension configuration: expected seven GUID/list part records")]
    Parts,
    #[error("extension configuration: unexpected first-part class")]
    FirstPartClass,
    #[error("extension configuration: expected first-part version and body")]
    FirstPartShape,
    #[error("extension configuration: expected integer first-part version")]
    PartVersion,
    #[error("extension configuration: unsupported first-part version (expected 1)")]
    UnsupportedPartVersion,
    #[error("extension configuration: expected body version and 76 fields")]
    BodyShape,
    #[error("extension configuration: expected integer body version")]
    BodyVersion,
    #[error("extension configuration: unsupported body version (expected 76)")]
    UnsupportedBodyVersion,
    #[error("extension configuration: expected {{0,header}} wrapper")]
    HeaderWrapper,
    #[error(transparent)]
    Header(#[from] HeaderError),
    #[error("extension configuration: expected quoted NamePrefix")]
    NamePrefix,
    #[error("extension configuration: expected integer compatibility at 26")]
    CompatibilityAt26,
    #[error("extension configuration: expected integer compatibility at 43")]
    CompatibilityAt43,
    #[error("extension configuration: expected integer purpose")]
    Purpose,
    #[error(
        "extension configuration: expected integer KeepMappingToExtendedConfigurationObjectsByIDs"
    )]
    KeepMappingByIds,
}

#[derive(Debug)]
pub struct ExtensionConfiguration<'a> {
    pub root_entry_id: Guid,
    pub header: ExtensionHeader<'a>,
    pub name_prefix: &'a str,
    pub wire_purpose: WireInteger<'a>,
    pub compatibility_at_26: WireInteger<'a>,
    pub compatibility_at_43: WireInteger<'a>,
    pub wire_keep_mapping_by_ids: WireInteger<'a>,
    source: &'a Node,
    body: &'a Node,
}

fn version(
    node: &Node,
    expected: &str,
    malformed: ConfigurationError,
    unsupported: ConfigurationError,
) -> Result<(), ConfigurationError> {
    let wire = WireInteger::from_node(node).ok_or(malformed)?;
    if wire.raw() != expected {
        return Err(unsupported);
    }
    Ok(())
}

fn table_property<'a>(body: &'a Node, name: &str) -> Option<&'a Node> {
    let StaticConfigPath::Body(path) = mdschema::static_schema().config_prop(name)?.path()? else {
        return None;
    };
    path.iter().try_fold(body, |node, index| node.at(index))
}

impl<'a> ExtensionConfiguration<'a> {
    pub fn from_node(node: &'a Node) -> Result<Self, ConfigurationError> {
        use ConfigurationError as E;
        let root = node
            .as_list()
            .filter(|root| root.len() == 11)
            .ok_or(E::RootShape)?;
        let [root_version, identity, count, parts @ .., _trailer] = root else {
            return Err(E::RootShape);
        };
        version(root_version, "2", E::RootVersion, E::UnsupportedRootVersion)?;
        let Some([root_id]) = identity.as_list() else {
            return Err(E::RootIdentity);
        };
        let root_entry_id = root_id.as_guid().ok_or(E::RootIdentity)?;
        if count.as_atom() != Some("7") {
            return Err(E::Parts);
        }
        for part in parts {
            let Some([class, payload]) = part.as_list() else {
                return Err(E::Parts);
            };
            if class.as_guid().is_none() || !payload.is_list() {
                return Err(E::Parts);
            }
        }
        let first = parts.first().and_then(Node::as_list).ok_or(E::Parts)?;
        if first.first().and_then(Node::as_guid)
            != Guid::parse("9cd510cd-abfc-11d4-9434-004095e12fc7")
        {
            return Err(E::FirstPartClass);
        }
        let payload = first
            .get(1)
            .and_then(Node::as_list)
            .ok_or(E::FirstPartShape)?;
        let [part_version, body, ..] = payload else {
            return Err(E::FirstPartShape);
        };
        version(part_version, "1", E::PartVersion, E::UnsupportedPartVersion)?;
        let fields = body.as_list().ok_or(E::BodyShape)?;
        let body_version = fields.first().ok_or(E::BodyShape)?;
        version(
            body_version,
            "76",
            E::BodyVersion,
            E::UnsupportedBodyVersion,
        )?;
        if fields.len() != 77 {
            return Err(E::BodyShape);
        }
        let Some([zero, header]) = body.at(1).and_then(Node::as_list) else {
            return Err(E::HeaderWrapper);
        };
        if zero.as_atom() != Some("0") {
            return Err(E::HeaderWrapper);
        }
        let header = ExtensionHeader::from_node(header)?;
        let name_prefix = table_property(body, "NamePrefix")
            .and_then(Node::as_str)
            .ok_or(E::NamePrefix)?;
        let compatibility_at_26 = table_property(body, "CompatibilityMode")
            .and_then(WireInteger::from_node)
            .ok_or(E::CompatibilityAt26)?;
        let compatibility_at_43 = body
            .at(43)
            .and_then(WireInteger::from_node)
            .ok_or(E::CompatibilityAt43)?;
        let wire_purpose = body
            .at(44)
            .and_then(WireInteger::from_node)
            .ok_or(E::Purpose)?;
        let wire_keep_mapping_by_ids = body
            .at(49)
            .and_then(WireInteger::from_node)
            .ok_or(E::KeepMappingByIds)?;
        Ok(Self {
            root_entry_id,
            header,
            name_prefix,
            wire_purpose,
            compatibility_at_26,
            compatibility_at_43,
            wire_keep_mapping_by_ids,
            source: node,
            body,
        })
    }

    pub fn purpose(&self) -> ConfigurationExtensionPurpose {
        match self.wire_purpose.raw().parse::<i64>() {
            Ok(0) => ConfigurationExtensionPurpose::Patch,
            Ok(1) => ConfigurationExtensionPurpose::Customization,
            Ok(2) => ConfigurationExtensionPurpose::AddOn,
            _ => ConfigurationExtensionPurpose::Unknown,
        }
    }

    pub fn keep_mapping_by_ids(&self) -> Option<bool> {
        match self.wire_keep_mapping_by_ids.raw().parse::<i64>() {
            Ok(0) => Some(false),
            Ok(1) => Some(true),
            _ => None,
        }
    }

    pub fn compatibility_spellings_match(&self) -> bool {
        self.compatibility_at_26.raw() == self.compatibility_at_43.raw()
    }

    pub fn source(&self) -> &'a Node {
        self.source
    }

    pub fn body(&self) -> &'a Node {
        self.body
    }
}
