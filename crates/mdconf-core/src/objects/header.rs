use brace::{Guid, Node};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireInteger<'a>(&'a str);

impl<'a> WireInteger<'a> {
    pub(crate) fn from_node(node: &'a Node) -> Option<Self> {
        let raw = node.as_atom()?;
        let digits = raw
            .strip_prefix('-')
            .or_else(|| raw.strip_prefix('+'))
            .unwrap_or(raw);
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(Self(raw))
    }

    pub fn raw(self) -> &'a str {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectBelonging {
    Own,
    Adopted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyState {
    Controlled,
    Extended,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderPropertyState<'a> {
    pub property_id: Guid,
    pub wire_state: WireInteger<'a>,
}

impl HeaderPropertyState<'_> {
    pub fn state(&self) -> PropertyState {
        match self.wire_state.raw().parse::<i64>() {
            Ok(2) => PropertyState::Controlled,
            Ok(3) => PropertyState::Extended,
            _ => PropertyState::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("extension header: expected a complete MDObjectBase list")]
    Shape,
    #[error("extension header: expected structure version 3")]
    Version,
    #[error("extension header: expected identity {{1,0,GUID}}")]
    Identity,
    #[error("extension header: expected a quoted name")]
    Name,
    #[error("extension header: expected a quoted comment")]
    Comment,
    #[error("extension header: expected an integer belonging atom")]
    Belonging,
    #[error("extension header: invalid property count or list length")]
    Count,
    #[error("extension header: expected an extended-object GUID atom")]
    Target,
    #[error("extension header: expected zero trailer")]
    Trailer,
    #[error("extension header: expected a property GUID atom")]
    PropertyGuid,
    #[error("extension header: expected an integer property-state atom")]
    PropertyState,
}

#[derive(Debug)]
pub struct ExtensionHeader<'a> {
    pub id: Guid,
    pub name: &'a str,
    pub synonym: &'a Node,
    pub comment: &'a str,
    pub wire_belonging: WireInteger<'a>,
    pub extended_object: Guid,
    source: &'a Node,
    pairs: &'a [Node],
}

impl<'a> ExtensionHeader<'a> {
    pub fn from_node(node: &'a Node) -> Result<Self, HeaderError> {
        let parsed = InstalledHeader::parse(node, false)?;
        Ok(Self {
            id: parsed.id,
            name: parsed.name,
            synonym: parsed.synonym,
            comment: parsed.comment,
            wire_belonging: parsed.wire_belonging,
            extended_object: parsed.extended_object.ok_or(HeaderError::Target)?,
            source: node,
            pairs: parsed.pairs,
        })
    }

    pub fn belonging(&self) -> ObjectBelonging {
        belonging_code(self.wire_belonging)
    }

    pub fn source(&self) -> &'a Node {
        self.source
    }

    pub fn property_states(
        &self,
    ) -> impl ExactSizeIterator<Item = Result<HeaderPropertyState<'a>, HeaderError>> + 'a {
        property_states(self.pairs)
    }
}

pub struct InstalledHeader<'a> {
    pub id: Guid,
    pub name: &'a str,
    pub synonym: &'a Node,
    pub comment: &'a str,
    pub wire_belonging: WireInteger<'a>,
    pub extended_object: Option<Guid>,
    pairs: &'a [Node],
}

impl<'a> InstalledHeader<'a> {
    pub fn from_node(node: &'a Node) -> Result<Self, HeaderError> {
        Self::parse(node, true)
    }

    fn parse(node: &'a Node, installed: bool) -> Result<Self, HeaderError> {
        let Some(
            [
                version,
                identity,
                name,
                synonym,
                comment,
                belonging,
                count,
                rest @ ..,
            ],
        ) = node.as_list()
        else {
            return Err(HeaderError::Shape);
        };
        let (identity_version, pairs, target, trailer) = match version.as_atom() {
            Some("1") if installed => ("0", rest, None, None),
            Some("2") if installed => {
                let (target, pairs) = rest.split_last().ok_or(HeaderError::Shape)?;
                ("1", pairs, Some(target), None)
            }
            other => {
                let (trailer, rest) = rest.split_last().ok_or(HeaderError::Shape)?;
                let (target, pairs) = rest.split_last().ok_or(HeaderError::Shape)?;
                if other != Some("3") {
                    return Err(HeaderError::Version);
                }
                ("1", pairs, Some(target), Some(trailer))
            }
        };
        let Some([one, zero, id]) = identity.as_list() else {
            return Err(HeaderError::Identity);
        };
        if one.as_atom() != Some(identity_version) || zero.as_atom() != Some("0") {
            return Err(HeaderError::Identity);
        }
        let id = id.as_guid().ok_or(HeaderError::Identity)?;
        let name = name.as_str().ok_or(HeaderError::Name)?;
        let comment = comment.as_str().ok_or(HeaderError::Comment)?;
        let wire_belonging = WireInteger::from_node(belonging).ok_or(HeaderError::Belonging)?;
        let count = count
            .as_atom()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or(HeaderError::Count)?;
        if count.checked_mul(2) != Some(pairs.len()) {
            return Err(HeaderError::Count);
        }
        let extended_object = target
            .map(|target| target.as_guid().ok_or(HeaderError::Target))
            .transpose()?;
        if trailer.is_some_and(|trailer| trailer.as_atom() != Some("0")) {
            return Err(HeaderError::Trailer);
        }
        Ok(Self {
            id,
            name,
            synonym,
            comment,
            wire_belonging,
            extended_object,
            pairs,
        })
    }

    pub fn belonging(&self) -> ObjectBelonging {
        belonging_code(self.wire_belonging)
    }

    pub fn property_states(
        &self,
    ) -> impl ExactSizeIterator<Item = Result<HeaderPropertyState<'a>, HeaderError>> + 'a {
        property_states(self.pairs)
    }
}

fn belonging_code(raw: WireInteger<'_>) -> ObjectBelonging {
    match raw.raw().parse::<i64>() {
        Ok(0) => ObjectBelonging::Own,
        Ok(1) => ObjectBelonging::Adopted,
        _ => ObjectBelonging::Unknown,
    }
}

fn property_states(
    pairs: &[Node],
) -> impl ExactSizeIterator<Item = Result<HeaderPropertyState<'_>, HeaderError>> {
    pairs.as_chunks::<2>().0.iter().map(|[guid, state]| {
        let property_id = guid.as_guid().ok_or(HeaderError::PropertyGuid)?;
        let wire_state = WireInteger::from_node(state).ok_or(HeaderError::PropertyState)?;
        Ok(HeaderPropertyState {
            property_id,
            wire_state,
        })
    })
}
