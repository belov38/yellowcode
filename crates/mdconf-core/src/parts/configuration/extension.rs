use crate::extension_header::{HeaderError, InstalledHeader, WireInteger};
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
    #[error(
        "extension configuration: expected root with six or seven parts and a trailer, found {}",
        Found(*.items)
    )]
    RootShape { items: Option<usize> },
    #[error("extension configuration: expected integer root version")]
    RootVersion,
    #[error("extension configuration: unsupported root version (expected 2)")]
    UnsupportedRootVersion,
    #[error("extension configuration: expected singleton root-entry GUID")]
    RootIdentity,
    #[error(
        "extension configuration: expected a part count matching six or seven GUID/list part records"
    )]
    Parts,
    #[error("extension configuration: unexpected first-part class")]
    FirstPartClass,
    #[error("extension configuration: expected first-part version and body")]
    FirstPartShape,
    #[error("extension configuration: expected integer first-part version")]
    PartVersion,
    #[error("extension configuration: unsupported first-part version (expected 1)")]
    UnsupportedPartVersion,
    #[error(
        "extension configuration: expected body version {} with {}, found {}",
        Version(*.version),
        Expected(*.expected),
        Found(*.fields)
    )]
    BodyShape {
        version: Option<i64>,
        expected: Option<usize>,
        fields: Option<usize>,
    },
    #[error("extension configuration: expected integer body version")]
    BodyVersion,
    #[error(
        "extension configuration: unsupported body version {} with {fields} values in a root of {parts} parts (supported: {MIN_BODY_VERSION} or later)",
        Version(*.version)
    )]
    UnsupportedBodyVersion {
        version: Option<i64>,
        fields: usize,
        parts: usize,
    },
    #[error(
        "extension configuration: body version {} with {fields} values in a root of {parts} parts was not observed, and its value at {position} is not a {}",
        Version(*.version),
        position_meaning(*.position)
    )]
    UnexpectedValue {
        version: Option<i64>,
        fields: usize,
        parts: usize,
        position: usize,
    },
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

pub const MIN_BODY_VERSION: i64 = 53;
pub const MIN_BODY_VALUES: usize = 45;
const KEEP_MAPPING_AT: usize = 49;
const COMPATIBILITY_AT_26: usize = 26;
const COMPATIBILITY_AT_43: usize = 43;
const PURPOSE_AT: usize = 44;

const OBSERVED: [(&str, usize, usize); 4] =
    [("53", 49, 6), ("55", 51, 6), ("59", 53, 6), ("76", 77, 7)];

fn position_meaning(position: usize) -> &'static str {
    match position {
        COMPATIBILITY_AT_26 | COMPATIBILITY_AT_43 => "compatibility mode",
        PURPOSE_AT => "purpose code",
        _ => "flag 0 or 1",
    }
}

struct Found(Option<usize>);
impl std::fmt::Display for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(items) => write!(f, "a list of {items} values"),
            None => f.write_str("no list"),
        }
    }
}

struct Expected(Option<usize>);
impl std::fmt::Display for Expected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(values) => write!(f, "{values} values"),
            None => write!(f, "at least {MIN_BODY_VALUES} values"),
        }
    }
}

struct Version(Option<i64>);
impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(version) => write!(f, "{version}"),
            None => f.write_str("(absent or out of range)"),
        }
    }
}

#[derive(Debug)]
pub struct ExtensionConfiguration<'a> {
    pub root_entry_id: Guid,
    pub header: InstalledHeader<'a>,
    pub name_prefix: &'a str,
    pub wire_purpose: WireInteger<'a>,
    pub compatibility_at_26: WireInteger<'a>,
    pub compatibility_at_43: WireInteger<'a>,
    pub wire_keep_mapping_by_ids: Option<WireInteger<'a>>,
    pub wire_body_version: WireInteger<'a>,
    pub body_values: usize,
    pub parts: usize,
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

fn plausible_compatibility(wire: WireInteger<'_>) -> bool {
    let raw = wire.raw();
    raw.bytes().all(|b| b.is_ascii_digit())
        && raw
            .parse::<u32>()
            .is_ok_and(|code| (80306..=80599).contains(&code))
}

fn digit_in(wire: WireInteger<'_>, allowed: &[&str]) -> bool {
    allowed.contains(&wire.raw())
}

impl<'a> ExtensionConfiguration<'a> {
    pub fn from_node(node: &'a Node) -> Result<Self, ConfigurationError> {
        use ConfigurationError as E;
        let root = node.as_list().ok_or(E::RootShape { items: None })?;
        let shape = E::RootShape {
            items: Some(root.len()),
        };
        let [root_version, identity, count, parts @ .., _trailer] = root else {
            return Err(shape);
        };
        if !matches!(parts.len(), 6 | 7) {
            return Err(shape);
        }
        version(root_version, "2", E::RootVersion, E::UnsupportedRootVersion)?;
        let Some([root_id]) = identity.as_list() else {
            return Err(E::RootIdentity);
        };
        let root_entry_id = root_id.as_guid().ok_or(E::RootIdentity)?;
        if count.as_atom() != Some(if parts.len() == 6 { "6" } else { "7" }) {
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
        let fields = body.as_list().ok_or(E::BodyShape {
            version: None,
            expected: None,
            fields: None,
        })?;
        let body_version = fields.first().ok_or(E::BodyShape {
            version: None,
            expected: None,
            fields: Some(0),
        })?;
        let wire_body_version = WireInteger::from_node(body_version).ok_or(E::BodyVersion)?;
        let number = wire_body_version.raw().parse::<i64>().ok();
        if !number.is_some_and(|number| number >= MIN_BODY_VERSION) {
            return Err(E::UnsupportedBodyVersion {
                version: number,
                fields: fields.len(),
                parts: parts.len(),
            });
        }
        let expected = OBSERVED
            .iter()
            .find(|(version, ..)| *version == wire_body_version.raw())
            .map(|&(_, values, _)| values);
        if expected.map_or(fields.len() < MIN_BODY_VALUES, |values| {
            values != fields.len()
        }) {
            return Err(E::BodyShape {
                version: number,
                expected,
                fields: Some(fields.len()),
            });
        }
        let Some([zero, header]) = body.at(1).and_then(Node::as_list) else {
            return Err(E::HeaderWrapper);
        };
        if zero.as_atom() != Some("0") {
            return Err(E::HeaderWrapper);
        }
        let header = InstalledHeader::parse(header, wire_body_version.raw() != "76")?;
        let name_prefix = table_property(body, "NamePrefix")
            .and_then(Node::as_str)
            .ok_or(E::NamePrefix)?;
        let compatibility_at_26 = table_property(body, "CompatibilityMode")
            .and_then(WireInteger::from_node)
            .ok_or(E::CompatibilityAt26)?;
        let compatibility_at_43 = body
            .at(COMPATIBILITY_AT_43)
            .and_then(WireInteger::from_node)
            .ok_or(E::CompatibilityAt43)?;
        let wire_purpose = body
            .at(PURPOSE_AT)
            .and_then(WireInteger::from_node)
            .ok_or(E::Purpose)?;
        let wire_keep_mapping_by_ids = body
            .at(KEEP_MAPPING_AT)
            .map(|node| WireInteger::from_node(node).ok_or(E::KeepMappingByIds))
            .transpose()?;
        let observed = OBSERVED.contains(&(wire_body_version.raw(), fields.len(), parts.len()));
        if !observed {
            let unexpected = |position| E::UnexpectedValue {
                version: number,
                fields: fields.len(),
                parts: parts.len(),
                position,
            };
            if !plausible_compatibility(compatibility_at_26) {
                return Err(unexpected(COMPATIBILITY_AT_26));
            }
            if !plausible_compatibility(compatibility_at_43) {
                return Err(unexpected(COMPATIBILITY_AT_43));
            }
            if !digit_in(wire_purpose, &["0", "1", "2"]) {
                return Err(unexpected(PURPOSE_AT));
            }
            if wire_keep_mapping_by_ids.is_some_and(|wire| !digit_in(wire, &["0", "1"])) {
                return Err(unexpected(KEEP_MAPPING_AT));
            }
        }
        Ok(Self {
            root_entry_id,
            header,
            name_prefix,
            wire_purpose,
            compatibility_at_26,
            compatibility_at_43,
            wire_keep_mapping_by_ids,
            wire_body_version,
            body_values: fields.len(),
            parts: parts.len(),
            source: node,
            body,
        })
    }

    pub fn observed(&self) -> bool {
        OBSERVED.contains(&(self.wire_body_version.raw(), self.body_values, self.parts))
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
        match self.wire_keep_mapping_by_ids?.raw().parse::<i64>() {
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
