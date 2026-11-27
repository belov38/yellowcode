use crate::EntrySource;
use crate::extension_configuration::{ConfigurationError, ExtensionConfiguration};
use crate::extension_header::{ExtensionHeader, HeaderError, InstalledHeader};
use crate::extension_reader::{self as reader, ExtensionEntry, ExtensionReadError, ExtensionRoot};
use brace::{Guid, Node};
use mdschema::{StaticPath, static_schema};
use memory_budget::{BudgetError, Category, Charged, Limits, MemoryBudget, Reservation};

mod modules;
pub use modules::{ExtensionModule, ExtensionModuleText, ModuleReadError, ModuleReadFailure};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferencePosition {
    pub part_index: usize,
    pub class_index: usize,
    pub reference_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InventoryError {
    #[error("extension part {part}: wrong part GUID or missing static layout")]
    PartClass { part: usize },
    #[error("extension part {part}: malformed payload wrapper")]
    PartShape { part: usize },
    #[error("extension part {part}: unsupported structural version")]
    PartVersion { part: usize },
    #[error("extension part {part}: invalid class count or payload length")]
    ClassCount { part: usize },
    #[error("extension part {part}, class {class}: malformed class GUID/list")]
    Class { part: usize, class: usize },
    #[error("extension part {part}, class {class}: invalid reference count or length")]
    ReferenceCount { part: usize, class: usize },
    #[error("extension reference {0:?}: expected GUID atom")]
    Reference(ReferencePosition),
}

#[derive(Debug, Clone, Copy)]
pub struct ExtensionClass<'a> {
    pub part_index: usize,
    pub class_index: usize,
    pub class_id: Guid,
    pub raw_class: &'a str,
    pub kind: Option<&'static str>,
    entries: &'a [Node],
}

#[derive(Debug, Clone, Copy)]
pub struct ObjectReference<'a> {
    pub part_index: usize,
    pub class_index: usize,
    pub reference_index: usize,
    pub class_id: Guid,
    pub raw_class: &'a str,
    pub kind: Option<&'static str>,
    pub entry_id: Guid,
    pub raw_entry: &'a str,
}
impl ObjectReference<'_> {
    pub fn supports_header(&self) -> bool {
        self.header_path().is_some()
    }

    fn header_path(&self) -> Option<StaticPath> {
        self.kind
            .and_then(|kind| static_schema().kind(kind))
            .and_then(|spec| spec.header())
    }

    pub fn position(&self) -> ReferencePosition {
        ReferencePosition {
            part_index: self.part_index,
            class_index: self.class_index,
            reference_index: self.reference_index,
        }
    }
}

pub struct ExtensionReferences<'a> {
    class: ExtensionClass<'a>,
    entries: std::iter::Enumerate<std::slice::Iter<'a, Node>>,
}
impl<'a> ExtensionClass<'a> {
    pub fn references(&self) -> ExtensionReferences<'a> {
        ExtensionReferences {
            class: *self,
            entries: self.entries.iter().enumerate(),
        }
    }
}
impl<'a> Iterator for ExtensionReferences<'a> {
    type Item = Result<ObjectReference<'a>, InventoryError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.entries.next().map(|(reference_index, entry)| {
            let position = ReferencePosition {
                part_index: self.class.part_index,
                class_index: self.class.class_index,
                reference_index,
            };
            let entry_id = entry.as_guid().ok_or(InventoryError::Reference(position))?;
            let raw_entry = entry.as_atom().ok_or(InventoryError::Reference(position))?;
            Ok(ObjectReference {
                part_index: position.part_index,
                class_index: position.class_index,
                reference_index,
                class_id: self.class.class_id,
                raw_class: self.class.raw_class,
                kind: self.class.kind,
                entry_id,
                raw_entry,
            })
        })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.entries.size_hint()
    }
}
impl ExactSizeIterator for ExtensionReferences<'_> {}
impl std::iter::FusedIterator for ExtensionReferences<'_> {}

pub struct ExtensionClasses<'a> {
    root: &'a Node,
    parts: usize,
    part: usize,
    groups: std::iter::Enumerate<std::slice::Iter<'a, Node>>,
    terminal: bool,
}
impl<'a> ExtensionClasses<'a> {
    pub fn new(configuration: &ExtensionConfiguration<'a>) -> Self {
        Self::from_root(configuration.source(), configuration.parts)
    }

    pub(crate) fn from_root(root: &'a Node, parts: usize) -> Self {
        Self {
            root,
            parts,
            part: 0,
            groups: [].iter().enumerate(),
            terminal: false,
        }
    }

    fn advance(&mut self) -> Result<Option<ExtensionClass<'a>>, InventoryError> {
        loop {
            if let Some((class, node)) = self.groups.next() {
                let part = self.part - 1;
                let malformed = InventoryError::Class { part, class };
                let Some([id, count, entries @ ..]) = node.as_list() else {
                    return Err(malformed);
                };
                let class_id = id.as_guid().ok_or(malformed)?;
                let raw_class = id.as_atom().ok_or(malformed)?;
                if unsigned(count) != Some(entries.len()) {
                    return Err(InventoryError::ReferenceCount { part, class });
                }
                let kind = static_schema()
                    .classes_top()
                    .find(|(id, _)| Guid::parse(id) == Some(class_id))
                    .map(|(_, kind)| kind);
                return Ok(Some(ExtensionClass {
                    part_index: part,
                    class_index: class,
                    class_id,
                    raw_class,
                    kind,
                    entries,
                }));
            }
            if self.part == self.parts {
                return Ok(None);
            }
            let groups = part_groups(self.root, self.part)?;
            self.part += 1;
            self.groups = groups.iter().enumerate();
        }
    }
}

impl<'a> Iterator for ExtensionClasses<'a> {
    type Item = Result<ExtensionClass<'a>, InventoryError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.terminal {
            return None;
        }
        match self.advance() {
            Ok(Some(class)) => Some(Ok(class)),
            Ok(None) => {
                self.terminal = true;
                None
            }
            Err(error) => {
                self.terminal = true;
                Some(Err(error))
            }
        }
    }
}
impl std::iter::FusedIterator for ExtensionClasses<'_> {}

fn unsigned(node: &Node) -> Option<usize> {
    node.as_atom()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
}

fn part_groups(root: &Node, part: usize) -> Result<&[Node], InventoryError> {
    let malformed = InventoryError::PartShape { part };
    let Some([class, payload]) = root.at(part + 3).and_then(Node::as_list) else {
        return Err(malformed);
    };
    let expected = static_schema()
        .table("classes.json")
        .and_then(|t| t.get("config_parts"))
        .and_then(|t| t.as_object())
        .and_then(|parts| parts.get(part))
        .and_then(|(id, _)| Guid::parse(id))
        .ok_or(InventoryError::PartClass { part })?;
    if class.as_guid() != Some(expected) {
        return Err(InventoryError::PartClass { part });
    }
    let payload = if part == 1 {
        let Some([version, inner]) = payload.as_list() else {
            return Err(malformed);
        };
        structural_version(version, "6", part)?;
        inner
    } else {
        payload
    };
    let Some([version, _body, count, groups @ ..]) = payload.as_list() else {
        return Err(malformed);
    };
    structural_version(version, "1", part)?;
    if unsigned(count) != Some(groups.len()) {
        return Err(InventoryError::ClassCount { part });
    }
    Ok(groups)
}

fn structural_version(node: &Node, expected: &str, part: usize) -> Result<(), InventoryError> {
    let wire = crate::extension_header::WireInteger::from_node(node)
        .ok_or(InventoryError::PartShape { part })?;
    if wire.raw() != expected {
        return Err(InventoryError::PartVersion { part });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ObjectReadError {
    #[error(transparent)]
    Configuration(#[from] ConfigurationError),
    #[error(transparent)]
    Inventory(#[from] InventoryError),
    #[error("extension reference {position:?}: unsupported class {class_id} ({kind:?})")]
    UnsupportedKind {
        position: ReferencePosition,
        class_id: Guid,
        kind: Option<&'static str>,
    },
    #[error("extension reference {position:?}: missing entry {entry_id}")]
    MissingEntry {
        position: ReferencePosition,
        entry_id: Guid,
    },
    #[error("extension reference {position:?}: {source}")]
    Read {
        position: ReferencePosition,
        #[source]
        source: ExtensionReadError,
    },
    #[error("extension reference {position:?}: {source}")]
    Brace {
        position: ReferencePosition,
        #[source]
        source: brace::ManagedParseError,
    },
    #[error("extension reference {0:?}: exact schema header path is absent")]
    HeaderPath(ReferencePosition),
    #[error("extension reference {position:?}: {source}")]
    Header {
        position: ReferencePosition,
        #[source]
        source: HeaderError,
    },
    #[error("extension objects: {0}")]
    Memory(#[from] BudgetError),
}

struct Loaded<'a> {
    tree: Charged<Node>,
    reference: ObjectReference<'a>,
    path: StaticPath,
    installed: bool,
}
impl Loaded<'_> {
    fn header_node(&self) -> Result<&Node, ObjectReadError> {
        self.path
            .iter()
            .try_fold(self.tree.as_ref(), |n, i| n.at(i))
            .ok_or(ObjectReadError::HeaderPath(self.reference.position()))
    }

    fn header(&self) -> Result<ExtensionHeader<'_>, ObjectReadError> {
        ExtensionHeader::from_node(self.header_node()?).map_err(|source| ObjectReadError::Header {
            position: self.reference.position(),
            source,
        })
    }

    fn installed_header(&self) -> Result<InstalledHeader<'_>, ObjectReadError> {
        InstalledHeader::from_node(self.header_node()?).map_err(|source| ObjectReadError::Header {
            position: self.reference.position(),
            source,
        })
    }

    fn configured_header(&self) -> Result<InstalledHeader<'_>, ObjectReadError> {
        InstalledHeader::parse(self.header_node()?, self.installed).map_err(|source| {
            ObjectReadError::Header {
                position: self.reference.position(),
                source,
            }
        })
    }

    pub(super) fn header_id(&self) -> Result<Guid, ObjectReadError> {
        if self.installed {
            Ok(self.installed_header()?.id)
        } else {
            Ok(self.header()?.id)
        }
    }
}

pub struct ExtensionObject<'a> {
    pub reference: ObjectReference<'a>,
    loaded: &'a Loaded<'a>,
    source: &'a dyn EntrySource,
    local: &'a MemoryBudget,
}
impl ExtensionObject<'_> {
    pub fn child_collections(
        &self,
    ) -> Result<
        crate::extension_children::ExtensionChildCollections<'_>,
        crate::extension_children::ChildError,
    > {
        crate::extension_children::ExtensionChildCollections::from_object(
            self.loaded.tree.as_ref(),
            self.loaded.reference.kind,
        )
    }

    pub fn header(&self) -> Result<ExtensionHeader<'_>, ObjectReadError> {
        self.loaded.header()
    }

    pub fn installed_header(&self) -> Result<InstalledHeader<'_>, ObjectReadError> {
        self.loaded.installed_header()
    }

    pub fn configured_header(&self) -> Result<InstalledHeader<'_>, ObjectReadError> {
        self.loaded.configured_header()
    }
}

struct Session<'a> {
    loaded: Option<Loaded<'a>>,
    local: MemoryBudget,
    _caller: Reservation,
    classes: ExtensionClasses<'a>,
    references: Option<ExtensionReferences<'a>>,
    installed: bool,
}
impl<'a> Session<'a> {
    fn advance(
        &mut self,
        source: &dyn EntrySource,
        only: Option<Guid>,
    ) -> Result<bool, ObjectReadError> {
        self.loaded = None;
        let reference = loop {
            if let Some(references) = &mut self.references
                && let Some(reference) = references.next()
            {
                let reference = reference?;
                if only.is_some_and(|id| id != reference.entry_id) {
                    continue;
                }
                break reference;
            }
            let Some(class) = self.classes.next() else {
                return Ok(false);
            };
            self.references = Some(class?.references());
        };
        let mut loaded = load_reference(source, &self.local, reference)?;
        loaded.installed = self.installed;
        loaded.header_id()?;
        self.loaded = Some(loaded);
        Ok(true)
    }
}

fn load_reference<'a>(
    source: &dyn EntrySource,
    local: &MemoryBudget,
    reference: ObjectReference<'a>,
) -> Result<Loaded<'a>, ObjectReadError> {
    let position = reference.position();
    let path = reference
        .header_path()
        .ok_or(ObjectReadError::UnsupportedKind {
            position,
            class_id: reference.class_id,
            kind: reference.kind,
        })?;
    let name = reader::root_name(reference.entry_id);
    let name = std::str::from_utf8(&name).map_err(|_| BudgetError::Overflow)?;
    let entry = ExtensionEntry::Object(reference.entry_id);
    let wrap = |source| ObjectReadError::Read { position, source };
    let size =
        reader::size(source, name, entry)
            .map_err(wrap)?
            .ok_or(ObjectReadError::MissingEntry {
                position,
                entry_id: reference.entry_id,
            })?;
    let tree = {
        let raw = reader::read_present(source, name, size, entry, local).map_err(wrap)?;
        reader::admit(&raw, local, entry, 0).map_err(wrap)?;
        brace::parse_bytes_budgeted(&raw, local, Category::ReaderWorking)
            .map_err(|source| ObjectReadError::Brace { position, source })?
    };
    Ok(Loaded {
        tree,
        reference,
        path,
        installed: false,
    })
}

fn workspace(root_hold: &Reservation) -> Result<(MemoryBudget, Reservation), ObjectReadError> {
    let extra = reader::WORKING_LIMIT
        .checked_sub(root_hold.bytes())
        .ok_or(BudgetError::Overflow)?;
    let caller = root_hold
        .budget()
        .try_reserve(Category::ReaderWorking, extra)
        .map_err(BudgetError::from)?;
    let local = MemoryBudget::try_new(Limits::new(extra))?;
    Ok((local, caller))
}

pub struct InstalledObjects<'a> {
    local: MemoryBudget,
    _caller: Reservation,
    source: &'a dyn EntrySource,
    _root: &'a ExtensionRoot,
}

impl<'a> InstalledObjects<'a> {
    pub(crate) fn new(
        root: &'a ExtensionRoot,
        source: &'a dyn EntrySource,
        hold: &Reservation,
    ) -> Result<Self, ObjectReadError> {
        let (local, caller) = workspace(hold)?;
        Ok(Self {
            local,
            _caller: caller,
            source,
            _root: root,
        })
    }

    pub fn with_header<R>(
        &mut self,
        reference: ObjectReference<'_>,
        view: impl FnOnce(InstalledHeader<'_>) -> R,
    ) -> Result<R, ObjectReadError> {
        let loaded = load_reference(self.source, &self.local, reference)?;
        let header = InstalledHeader::from_node(loaded.header_node()?).map_err(|source| {
            ObjectReadError::Header {
                position: reference.position(),
                source,
            }
        })?;
        Ok(view(header))
    }
}

pub struct ExtensionObjects<'a> {
    state: Option<Session<'a>>,
    source: &'a dyn EntrySource,
    _root: &'a ExtensionRoot,
}
impl<'a> ExtensionObjects<'a> {
    pub(crate) fn new(
        root: &'a ExtensionRoot,
        source: &'a dyn EntrySource,
        root_hold: &Reservation,
    ) -> Result<Self, ObjectReadError> {
        let configuration = root.configuration()?;
        let installed = configuration.wire_body_version.raw() != "76";
        let classes = ExtensionClasses::new(&configuration);
        let (local, caller) = workspace(root_hold)?;
        Ok(Self {
            state: Some(Session {
                loaded: None,
                local,
                _caller: caller,
                classes,
                references: None,
                installed,
            }),
            source,
            _root: root,
        })
    }

    #[allow(
        clippy::should_implement_trait,
        reason = "a lending view borrows this mutable session"
    )]
    pub fn next(&mut self) -> Result<Option<ExtensionObject<'_>>, ObjectReadError> {
        self.step(None)
    }

    pub fn find(&mut self, entry_id: Guid) -> Result<Option<ExtensionObject<'_>>, ObjectReadError> {
        self.step(Some(entry_id))
    }

    fn step(&mut self, only: Option<Guid>) -> Result<Option<ExtensionObject<'_>>, ObjectReadError> {
        let Some(mut state) = self.state.take() else {
            return Ok(None);
        };
        if !state.advance(self.source, only)? {
            return Ok(None);
        }
        self.state = Some(state);
        Ok(self.state.as_ref().and_then(|state| {
            let loaded = state.loaded.as_ref()?;
            Some(ExtensionObject {
                reference: loaded.reference,
                loaded,
                source: self.source,
                local: &state.local,
            })
        }))
    }
}
