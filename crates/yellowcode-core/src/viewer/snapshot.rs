mod analysis;
mod form_structure;
pub use form_structure::FormStructureProjection;
mod names_directory;
mod names_forms;
pub use names_directory::ManagedNamesDirectory;
mod names_metadata;
use super::{
    Lang,
    body::{self, ViewerBody},
    names::ManagedNames,
    tree::{ManagedFlatNode, ManagedTree},
};
pub use analysis::{
    GeneratedKind, ManagedCheckItem, PreviewFormat, PreviewOptions, PreviewProjection,
    SelectedAnalysis, SelectedAnalysisError, SelectedFormError,
};
use mdconf::{
    ManagedEntrySource,
    startup::{
        ConfigurationIdentity, ManagedConfigurationError, ManagedViewerOpen, ViewerCounters,
    },
};
use memory_budget::{BudgetError, Shared};
pub use names_metadata::{ManagedNamesMetadataError, NamesCancellation, NamesControl};
use platform_catalog::{ManagedCatalog, ManagedCatalogError};
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};

#[derive(Debug, thiserror::Error)]
pub enum ManagedSnapshotError<E> {
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("{0}")]
    Model(#[from] ManagedConfigurationError<E>),
    #[error("{0}")]
    Catalog(#[from] ManagedCatalogError),
}
pub struct ManagedSnapshot<S: ManagedEntrySource> {
    open: ManagedViewerOpen<S>,
    catalog: ManagedCatalog,
    tree: ManagedTree,
    lang: Lang,
}
impl<S: ManagedEntrySource> ManagedSnapshot<S> {
    pub fn build(
        source: Shared<S>,
        catalog_bytes: &[u8],
        lang: Lang,
    ) -> Result<Self, ManagedSnapshotError<S::Error>> {
        let budget = source.memory_budget().clone();
        let catalog = ManagedCatalog::decode(catalog_bytes, &budget)?;
        let open = ManagedViewerOpen::build(source)?;
        let tree = ManagedTree::build(&open, &catalog, lang)?;
        Ok(Self {
            open,
            catalog,
            tree,
            lang,
        })
    }
    pub fn open(&self) -> &ManagedViewerOpen<S> {
        &self.open
    }
    pub fn catalog(&self) -> &ManagedCatalog {
        &self.catalog
    }
    pub fn names(&self) -> ManagedNames<'_> {
        ManagedNames::new(&self.catalog)
    }
    pub fn tree(&self) -> &ManagedTree {
        &self.tree
    }
    pub fn lang(&self) -> Lang {
        self.lang
    }
    pub fn info_body(&self, source: &str, wire_limit: u64) -> Result<ViewerBody, BudgetError> {
        body::json(
            self.catalog.memory_budget(),
            wire_limit,
            &Info {
                identity: self.open.identity(),
                stats: self.open.stats(),
                source,
                lang: self.lang,
            },
        )
    }
    pub fn stats_body(&self, wire_limit: u64) -> Result<ViewerBody, BudgetError> {
        body::json(
            self.catalog.memory_budget(),
            wire_limit,
            &Stats(self.open.stats()),
        )
    }
    pub fn children_body(
        &self,
        id: usize,
        wire_limit: u64,
    ) -> Result<Option<ViewerBody>, BudgetError> {
        let Some(parent) = self.tree.nodes().get(id) else {
            return Ok(None);
        };
        body::json(
            self.catalog.memory_budget(),
            wire_limit,
            &Children {
                nodes: self.tree.nodes(),
                children: parent.children.as_slice(),
            },
        )
        .map(Some)
    }
}
#[derive(Debug, thiserror::Error)]
pub enum ManagedDtSnapshotError {
    #[error("{0}")]
    Extract(#[from] super::source::ManagedDtExtractError),
    #[error("{0}")]
    Build(#[from] ManagedSnapshotError<super::source::ManagedDtSourceError>),
    #[error("{0}")]
    Progress(crate::Error),
}
pub type ManagedDtSnapshot = ManagedSnapshot<super::source::ManagedDtSource>;
impl ManagedDtSnapshot {
    pub fn load_dt(
        path: &std::path::Path,
        memory: &super::source::OpenMemory,
        catalog_bytes: &[u8],
        lang: Lang,
        progress: &mut impl FnMut(&'static str) -> crate::Result<()>,
    ) -> Result<Self, ManagedDtSnapshotError> {
        let source = super::source::ManagedDtSource::extract(path, memory, progress)?;
        progress("metadata").map_err(ManagedDtSnapshotError::Progress)?;
        source.verify_origin(path)?;
        let catalog = ManagedCatalog::decode(catalog_bytes, memory.ledger())
            .map_err(ManagedSnapshotError::Catalog)?;
        let open = ManagedViewerOpen::build(source.clone()).map_err(ManagedSnapshotError::Model)?;
        progress("tree").map_err(ManagedDtSnapshotError::Progress)?;
        let tree =
            ManagedTree::build(&open, &catalog, lang).map_err(ManagedSnapshotError::Memory)?;
        progress("statistics").map_err(ManagedDtSnapshotError::Progress)?;
        source.verify_origin(path)?;
        Ok(Self {
            open,
            catalog,
            tree,
            lang,
        })
    }
}
struct Info<'a> {
    identity: &'a ConfigurationIdentity,
    stats: &'a ViewerCounters,
    source: &'a str,
    lang: Lang,
}
impl Serialize for Info<'_> {
    fn serialize<T: Serializer>(&self, s: T) -> Result<T::Ok, T::Error> {
        let language = if self.lang == Lang::Ru { "ru" } else { "en" };
        let synonym = self.identity.synonym.as_ref().and_then(|rows| {
            rows.iter()
                .find(|(code, _)| code.as_str() == language)
                .or_else(|| rows.first())
                .map(|(_, text)| text.as_str())
                .filter(|text| !text.is_empty())
        });
        let mut map = s.serialize_map(Some(8))?;
        map.serialize_entry("name", self.identity.name.as_str())?;
        map.serialize_entry("synonym", &synonym)?;
        map.serialize_entry("yellowcode_version", crate::VERSION)?;
        map.serialize_entry(
            "configuration_version",
            &self.identity.version.as_ref().map(|text| text.as_str()),
        )?;
        map.serialize_entry("source", self.source)?;
        map.serialize_entry("lang", language)?;
        map.serialize_entry("stats", &Stats(self.stats))?;
        map.serialize_entry("icons_v", super::icons::version())?;
        map.end()
    }
}
struct Kinds<'a>(&'a ViewerCounters);
impl Serialize for Kinds<'_> {
    fn serialize<T: Serializer>(&self, s: T) -> Result<T::Ok, T::Error> {
        let mut map = s.serialize_map(Some(self.0.kinds.len()))?;
        for (kind, count) in self.0.kinds.iter() {
            map.serialize_entry(kind.as_str(), count)?;
        }
        map.end()
    }
}
struct Stats<'a>(&'a ViewerCounters);
impl Serialize for Stats<'_> {
    fn serialize<T: Serializer>(&self, s: T) -> Result<T::Ok, T::Error> {
        let value = self.0;
        let mut map = s.serialize_map(Some(8))?;
        map.serialize_entry("objects", &Kinds(value))?;
        map.serialize_entry("nested_subsystems", &value.nested_subsystems)?;
        map.serialize_entry("modules", &value.modules)?;
        map.serialize_entry("module_lines", &value.module_lines)?;
        map.serialize_entry("module_bytes", &value.module_bytes)?;
        map.serialize_entry("forms", &value.forms)?;
        map.serialize_entry("form_modules", &value.form_modules)?;
        map.serialize_entry("templates", &value.templates)?;
        map.end()
    }
}
struct Child<'a> {
    id: usize,
    node: &'a ManagedFlatNode,
}
impl Serialize for Child<'_> {
    fn serialize<T: Serializer>(&self, s: T) -> Result<T::Ok, T::Error> {
        let node = self.node;
        let mut map = s.serialize_map(Some(8))?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("label", node.label.as_str())?;
        map.serialize_entry("path", &node.path.as_ref().map(|s| s.as_str()))?;
        map.serialize_entry("module", &node.module.as_ref().map(|s| s.as_str()))?;
        map.serialize_entry("synonym", &node.synonym.as_ref().map(|s| s.as_str()))?;
        map.serialize_entry("type_text", &node.type_text.as_ref().map(|s| s.as_str()))?;
        map.serialize_entry("icon", node.icon.as_str())?;
        map.serialize_entry("children", &node.children.len())?;
        map.end()
    }
}
struct Children<'a> {
    nodes: &'a [ManagedFlatNode],
    children: &'a [usize],
}
impl Serialize for Children<'_> {
    fn serialize<T: Serializer>(&self, s: T) -> Result<T::Ok, T::Error> {
        let mut seq = s.serialize_seq(Some(self.children.len()))?;
        for &id in self.children {
            if let Some(node) = self.nodes.get(id) {
                seq.serialize_element(&Child { id, node })?;
            }
        }
        seq.end()
    }
}
