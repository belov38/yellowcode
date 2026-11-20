use super::{
    ExternalBuildOptions, ExternalDocument, LoadedXmlExternal, PreparedExternal, XmlLoadOptions,
};
use crate::{EntryData, EntryMeta, EntrySink, EntrySource, Error, Result};
use brace::Guid;
use memory_budget::{BudgetError, Category, Charged, MemoryBudget};
use std::{
    cell::RefCell,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub fn load_xml(
    root: &Path,
    options: &XmlLoadOptions,
    memory: &MemoryBudget,
) -> Result<LoadedXmlExternal> {
    let codecs = FullXml {
        shared: RefCell::new(None),
    };
    mdconf_core::external::xml_in::load_xml_with(root, options, memory, &codecs)
}
pub fn prepare(
    doc: &ExternalDocument,
    options: &ExternalBuildOptions,
    memory: &MemoryBudget,
) -> Result<PreparedExternal> {
    mdconf_core::external::build::prepare_with(doc, options, memory, &FullBuild)
}
pub fn build(
    doc: &ExternalDocument,
    sink: &mut dyn EntrySink,
    options: &ExternalBuildOptions,
    memory: &MemoryBudget,
) -> Result<()> {
    prepare(doc, options, memory)?.write_to(sink)
}
pub fn write(
    doc: &ExternalDocument,
    path: &Path,
    options: &ExternalBuildOptions,
    memory: &MemoryBudget,
) -> Result<()> {
    prepare(doc, options, memory)?.write(path)
}
fn failure(path: &str, what: impl Into<String>) -> Error {
    Error::Build {
        object: "external document".into(),
        path: path.into(),
        what: what.into(),
    }
}
fn memory(error: BudgetError) -> Error {
    match error {
        BudgetError::Limit(e) => Error::Memory(e),
        other => Error::Io(std::io::Error::new(std::io::ErrorKind::OutOfMemory, other)),
    }
}
fn cancel(options: &Option<Arc<AtomicBool>>) -> bool {
    options.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
}

struct Resolution<'a>(&'a ExternalDocument);
impl md_forms::forms::FormSettingsLookup for Resolution<'_> {
    fn name(&self, id: &Guid) -> Option<&str> {
        if mdconf_core::external::build::can_restore_reference(self.0, *id).ok()? {
            self.0.name(*id)
        } else {
            None
        }
    }
    fn type_name(&self, id: &Guid) -> Option<&str> {
        self.0.type_name(*id)
    }
    fn sub_header(&self, id: &Guid) -> Option<&[usize]> {
        self.0.index.sub_hdr.get(id).map(Vec::as_slice)
    }
    fn common_picture(&self, name: &str) -> bool {
        md_forms::forms::FormSettingsLookup::common_picture(self.0, name)
    }
}
impl md_forms::forms::FormResolution for Resolution<'_> {
    fn predefined_name(&self, _: &Guid) -> Option<&str> {
        None
    }
}
impl md_forms::forms::xml_build::FormBuildResolution for Resolution<'_> {
    fn metadata_id(&self, name: &str) -> Option<Guid> {
        if let Some(id) = Guid::parse(name) {
            return Some(id);
        }
        let id = md_forms::forms::xml_build::FormBuildResolution::metadata_id(self.0, name)?;
        mdconf_core::external::build::can_restore_reference(self.0, id)
            .ok()?
            .then_some(id)
    }
    fn metadata_entries(&self, visit: &mut dyn FnMut(Guid, &str)) {
        md_forms::forms::xml_build::FormBuildResolution::metadata_entries(
            self.0,
            &mut |id, name| {
                if mdconf_core::external::build::can_restore_reference(self.0, id).unwrap_or(false)
                {
                    visit(id, name);
                }
            },
        );
    }
    fn type_id(&self, name: &str) -> Option<Guid> {
        let name = name.strip_prefix("cfg:").unwrap_or(name);
        md_forms::forms::xml_build::FormBuildResolution::type_id(self.0, name)
    }
}
impl md_graphics::templates::simple_build::InverseLookup for Resolution<'_> {
    fn object_id(&self, name: &str) -> Option<Guid> {
        md_forms::forms::xml_build::FormBuildResolution::metadata_id(self, name)
    }
}
impl md_graphics::templates::dcs_build::InverseLookup for Resolution<'_> {
    fn type_id(&self, name: &str) -> Option<Guid> {
        md_forms::forms::xml_build::FormBuildResolution::type_id(self, name)
    }
    fn object_id(&self, name: &str) -> Option<Guid> {
        md_forms::forms::xml_build::FormBuildResolution::metadata_id(self, name)
    }
}
impl md_graphics::templates::mxl_build::InverseLookup for Resolution<'_> {
    fn type_id(&self, name: &str) -> Option<Guid> {
        md_forms::forms::xml_build::FormBuildResolution::type_id(self, name)
    }
    fn object_id(&self, name: &str) -> Option<Guid> {
        md_forms::forms::xml_build::FormBuildResolution::metadata_id(self, name)
    }
}

struct FullXml {
    shared: RefCell<Option<Arc<md_forms::forms::shared::Shared>>>,
}
impl mdconf_core::external::xml_in::XmlCodecs for FullXml {
    fn form_document(
        &self,
        input: mdconf_core::external::xml_in::FormXmlInput<'_>,
        document: &ExternalDocument,
        options: &XmlLoadOptions,
        budget: &MemoryBudget,
    ) -> Result<md_model::forms::Form> {
        use md_forms::forms::xml_build::{FormBuildError, FormBuildInput, build_form};
        let built = build_form(
            FormBuildInput {
                xml: input.xml,
                module: input.module,
                files: input.files,
                owner: Some(input.owner),
            },
            &Resolution(document),
            budget,
            &|| cancel(&options.cancelled),
        )
        .map_err(|e| match e {
            FormBuildError::Memory(e) => Error::Memory(e),
            FormBuildError::Budget(e) => memory(e),
            FormBuildError::Cancelled => Error::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "form build cancelled",
            )),
            other => failure(&input.path.to_string_lossy(), other.to_string()),
        })?;
        let name = format!("{}.0", input.id);
        let stored = form_storage(built.as_bytes(), &name, budget, &options.cancelled)?;
        let hold = budget
            .try_reserve(
                Category::Metadata,
                (std::mem::size_of::<FormMeter>() + 2 * std::mem::size_of::<usize>()) as u64,
            )
            .map_err(Error::Memory)?
            .try_into_shared()
            .map_err(memory)?;
        (input.retain)(hold.clone());
        let meter = Arc::new(FormMeter {
            hold,
            cancelled: options.cancelled.clone(),
        });
        let source = FormEntry {
            name,
            bytes: &stored,
            budget,
            meter: &*meter,
        };
        let entries = mdconf_core::entries::Entries::with_meter(&source, Some(meter.clone()))?;
        let load = crate::LoadOptions {
            keep_source: true,
            meter: Some(meter.clone()),
            ..Default::default()
        };
        let context = mdconf_core::codec::LoadCtx {
            schema: mdschema::schema()?,
            entries: &entries,
            opts: &load,
        };
        let existing = self.shared.borrow().as_ref().map(Arc::clone);
        let shared = match existing {
            Some(shared) => shared,
            None => {
                let metadata = mdconf_core::external::build::encode_object_tree(document, budget)?;
                let shared = Arc::new(md_forms::forms::shared::external(
                    &context,
                    metadata.as_tree(),
                    document,
                )?);
                *self.shared.borrow_mut() = Some(Arc::clone(&shared));
                shared
            }
        };
        read_form_checked(&context, input.id, &shared, &options.cancelled)
    }
}
fn read_form_checked(
    context: &mdconf_core::codec::LoadCtx<'_>,
    id: Guid,
    shared: &Arc<md_forms::forms::shared::Shared>,
    cancelled: &Option<Arc<AtomicBool>>,
) -> Result<md_model::forms::Form> {
    let form = md_forms::forms::read_external(context, id, shared);
    if cancel(cancelled) {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "form read cancelled",
        )));
    }
    form
}
struct FormMeter {
    hold: memory_budget::Shared<()>,
    cancelled: Option<Arc<AtomicBool>>,
}
impl mdconf_core::LoadMeter for FormMeter {
    fn checkpoint(&self) -> Result<()> {
        if cancel(&self.cancelled) {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "form read cancelled",
            )));
        }
        Ok(())
    }
    fn admit(&self, _: &str, bytes: u64) -> std::result::Result<(), mdconf_core::MeterRefusal> {
        let amount = bytes
            .checked_mul(32)
            .and_then(|n| n.checked_add(4096))
            .ok_or(mdconf_core::MeterRefusal(memory_budget::MemoryLimit {
                category: Category::Metadata,
                requested: u64::MAX,
                reason: memory_budget::LimitReason::Overflow,
            }))?;
        self.hold
            .reservation()
            .try_grow(amount)
            .map_err(mdconf_core::MeterRefusal)
    }
    fn hold(&self, _: &str, bytes: u64) -> std::result::Result<(), mdconf_core::MeterRefusal> {
        self.hold
            .reservation()
            .try_grow(bytes)
            .map_err(mdconf_core::MeterRefusal)
    }
    fn release(&self, bytes: u64) {
        let result = self.hold.reservation().shrink(bytes);
        debug_assert!(result.is_ok());
    }
}
fn form_storage(
    raw: &[u8],
    name: &str,
    budget: &MemoryBudget,
    cancelled: &Option<Arc<AtomicBool>>,
) -> Result<crate::StoredBytes> {
    use std::io::Write;
    let _workspace = budget
        .try_reserve(Category::Metadata, 512 << 10)
        .map_err(Error::Memory)?;
    struct Output {
        bytes: memory_budget::BudgetedVec<u8>,
        error: Option<BudgetError>,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            match self.bytes.try_extend_from_slice(bytes) {
                Ok(()) => Ok(bytes.len()),
                Err(error) => {
                    self.error = Some(error);
                    Err(std::io::ErrorKind::OutOfMemory.into())
                }
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Output {
        bytes: memory_budget::BudgetedVec::new(budget, Category::Metadata).map_err(memory)?,
        error: None,
    };
    let result = (|| -> Result<()> {
        let mut writer =
            zraw::DeflateWriter::new(&mut output, zraw::LEVEL_1C).map_err(|source| {
                Error::Deflate {
                    entry: name.into(),
                    source,
                }
            })?;
        for chunk in raw.chunks(64 << 10) {
            if cancel(cancelled) {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "form compression cancelled",
                )));
            }
            writer.write_all(chunk)?;
        }
        writer.finish()?;
        Ok(())
    })();
    if let Some(error) = output.error {
        return Err(memory(error));
    }
    result?;
    crate::EntryBacking::from_charged(output.bytes.into_charged_vec()).map_err(memory)
}
struct FormEntry<'a> {
    name: String,
    bytes: &'a crate::StoredBytes,
    budget: &'a MemoryBudget,
    meter: &'a dyn mdconf_core::LoadMeter,
}
impl EntrySource for FormEntry<'_> {
    fn list(&self) -> Result<Vec<EntryMeta>> {
        Err(failure("form", "form entry requires a prepaid listing"))
    }
    fn list_prepaid_checked(
        &self,
        budget: &MemoryBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<crate::EntryList> {
        check()?;
        let charge = budget
            .try_reserve(
                Category::Metadata,
                (self.name.len() + std::mem::size_of::<EntryMeta>()) as u64,
            )
            .map_err(Error::Memory)?;
        check()?;
        let list = vec![EntryMeta {
            name: self.name.clone(),
            ctime: Default::default(),
            mtime: Default::default(),
            attrs: 0,
        }];
        memory_budget::Shared::try_new(list, charge)
            .map(crate::EntryBacking::Charged)
            .map_err(memory)
    }
    fn list_owned_checked(&self, check: &dyn Fn() -> Result<()>) -> Result<crate::EntryList> {
        let list = self.list_prepaid_checked(self.budget, check)?;
        self.meter
            .hold(
                "form entry name lookup",
                (self.name.len() + std::mem::size_of::<EntryMeta>() + 128) as u64,
            )
            .map_err(|mdconf_core::MeterRefusal(limit)| Error::Memory(limit))?;
        Ok(list)
    }
    fn stored(&self, _: &str) -> Result<Option<Vec<u8>>> {
        Err(failure("form", "form entry requires a bounded owned read"))
    }
    fn stored_len(&self, name: &str) -> Result<Option<u64>> {
        Ok((name == self.name).then_some(self.bytes.len() as u64))
    }
    fn stored_owned_bounded(&self, name: &str, limit: usize) -> Result<Option<crate::StoredBytes>> {
        if name != self.name {
            return Ok(None);
        }
        if self.bytes.len() > limit {
            return Err(failure(name, "stored form byte limit"));
        }
        Ok(Some(self.bytes.clone()))
    }
    fn stored_inflated_checked(
        &self,
        name: &str,
        limit: u64,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Option<crate::Inflated>> {
        crate::BudgetedEntries::new(self, self.budget.clone())
            .stored_inflated_checked(name, limit, check)
    }
}

struct FullBuild;
impl mdconf_core::external::build::BuildCodecs for FullBuild {
    fn help(
        &self,
        help: &md_model::parts::Help,
        document: &ExternalDocument,
        budget: &MemoryBudget,
        options: &ExternalBuildOptions,
    ) -> Result<Charged<Vec<u8>>> {
        let mut rebuilt = md_model::parts::Help {
            pages: Vec::new(),
            files: help.files.clone(),
        };
        for (language, bytes) in &help.pages {
            if cancel(&options.cancelled) {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "help build cancelled",
                )));
            }
            rebuilt.pages.push((
                language.clone(),
                md_graphics::templates::simple_build::native_html_with_files(
                    bytes,
                    &Resolution(document),
                    &help.files,
                )
                .map_err(|e| failure("Help", e.to_string()))?,
            ));
        }
        let node = mdconf_core::external::build::help_node(&rebuilt)?;
        brace::write_doc_budgeted(
            &brace::Doc {
                root: node,
                bom: true,
                lead: String::new(),
                trailer: String::new(),
            },
            budget,
            Category::Metadata,
        )
        .map_err(memory)
    }
    fn form(
        &self,
        sub: &crate::SubObject,
        _: &ExternalDocument,
        budget: &MemoryBudget,
        _: &ExternalBuildOptions,
    ) -> Result<Charged<Vec<u8>>> {
        let source = sub
            .form
            .as_ref()
            .and_then(|f| f.source.as_ref())
            .ok_or_else(|| failure(&sub.name, "missing encoded managed form"))?;
        let mut doc = match source.entry() {
            Some(EntryData::Brace(doc)) => doc.clone(),
            Some(_) => return Err(failure(&sub.name, "form source is not brace")),
            None => brace::parse_doc(
                source
                    .bytes()
                    .ok_or_else(|| failure(&sub.name, "missing form source"))?,
            )
            .map_err(|e| failure(&sub.name, e.to_string()))?,
        };
        let module = sub
            .module
            .as_ref()
            .map(|m| m.source.as_slice())
            .unwrap_or_default();
        let module = std::str::from_utf8(module.strip_prefix(b"\xef\xbb\xbf").unwrap_or(module))
            .map_err(|_| failure(&sub.name, "form module is not UTF-8"))?;
        *doc.root
            .get_mut(&[2])
            .ok_or_else(|| failure(&sub.name, "missing form module slot"))? =
            brace::Node::str(module);
        brace::write_doc_budgeted(&doc, budget, Category::Metadata).map_err(memory)
    }
    fn template(
        &self,
        template: &md_model::templates::Template,
        document: &ExternalDocument,
        budget: &MemoryBudget,
        options: &ExternalBuildOptions,
    ) -> Result<Charged<Vec<u8>>> {
        let never = AtomicBool::new(false);
        let cancel = options.cancelled.as_deref().unwrap_or(&never);
        if template.kind == Some(md_model::templates::TemplateType::SpreadsheetDocument) {
            use md_graphics::templates::mxl_build::{self, Asset, BuildError};
            let xml = template
                .files
                .iter()
                .find_map(|(name, data)| (name == "Template.xml").then_some(data.as_slice()))
                .ok_or_else(|| failure("Template.xml", "missing spreadsheet content"))?;
            let mut assets = memory_budget::BudgetedVec::try_with_capacity(
                budget,
                Category::Metadata,
                template.files.len(),
            )
            .map_err(memory)?;
            for (path, bytes) in &template.files {
                if path != "Template.xml" {
                    assets.try_push(Asset { path, bytes }).map_err(memory)?;
                }
            }
            return mxl_build::build(
                xml,
                assets.as_slice(),
                &Resolution(document),
                budget,
                cancel,
            )
            .map_err(|e| match e {
                BuildError::Memory(e) => memory(e),
                BuildError::Cancelled => Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "spreadsheet build cancelled",
                )),
                other => failure("SpreadsheetDocument", other.to_string()),
            });
        }
        if template.kind == Some(md_model::templates::TemplateType::DataCompositionSchema) {
            use md_graphics::templates::dcs_build::{self, Asset, BuildError};
            let xml = template
                .files
                .iter()
                .find_map(|(name, data)| (name == "Template.xml").then_some(data.as_slice()))
                .ok_or_else(|| failure("Template.xml", "missing DCS content"))?;
            let mut assets = memory_budget::BudgetedVec::try_with_capacity(
                budget,
                Category::Metadata,
                template.files.len(),
            )
            .map_err(memory)?;
            for (path, bytes) in &template.files {
                if path != "Template.xml" {
                    assets.try_push(Asset { path, bytes }).map_err(memory)?;
                }
            }
            return dcs_build::build(
                xml,
                assets.as_slice(),
                &Resolution(document),
                budget,
                cancel,
            )
            .map_err(|e| match e {
                BuildError::Memory(e) => memory(e),
                BuildError::Cancelled => Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "DCS build cancelled",
                )),
                other => failure("DCS", other.to_string()),
            });
        }
        md_graphics::templates::simple_build::build(template, &Resolution(document), budget, cancel)
            .map_err(|e| match e {
                md_graphics::templates::simple_build::BuildError::Memory(e) => memory(e),
                md_graphics::templates::simple_build::BuildError::Cancelled => {
                    Error::Io(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "template build cancelled",
                    ))
                }
                other => failure("Template", other.to_string()),
            })
    }
}
