use super::ManagedSnapshot;
use brace::Guid;
use mdconf::{
    ChildTag, EntryData, ExternalObject, ManagedEntrySource, ModuleKind, SubKind,
    startup::{
        ManagedConfigurationError, ManagedSelectionError, ModuleLocation, ModuleSlot, kind_spec,
        module_kind, modules,
    },
    xml_out::{
        managed_client_interface_xml, managed_command_interface_xml, managed_configdump_xml,
        managed_configuration_xml, managed_content_xml, managed_external_xml, managed_help_page,
        managed_help_xml, managed_home_page_xml, managed_object_xml, managed_picture_xml,
        managed_predefined_xml, managed_recalculation_xml, managed_rights_xml,
        managed_schedule_xml, managed_style_xml, managed_subobject_xml,
    },
};
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget, Shared,
};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

#[derive(Debug, thiserror::Error)]
pub enum ManagedNamesMetadataError<E> {
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("{0}")]
    Selection(#[from] ManagedSelectionError<E>),
    #[error("{0}")]
    Configuration(#[from] ManagedConfigurationError<E>),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    FormProjection(#[from] super::SelectedFormError<E>),
    #[error("names metadata projection canceled")]
    Canceled,
    #[error("inconsistent names metadata inventory")]
    Inventory,
}
type Result<T, E> = std::result::Result<T, ManagedNamesMetadataError<E>>;

pub trait NamesCancellation {
    fn flag(&self) -> &AtomicBool;
    fn is_canceled(&self) -> bool {
        self.flag().load(Ordering::Acquire)
    }
    fn deadline(&self) -> Option<Instant> {
        None
    }
}
impl NamesCancellation for AtomicBool {
    fn flag(&self) -> &AtomicBool {
        self
    }
}
pub struct NamesControl<'a> {
    flag: &'a AtomicBool,
    deadline: Option<Instant>,
    interrupted: &'a dyn Fn() -> bool,
}
impl<'a> NamesControl<'a> {
    pub fn new(
        flag: &'a AtomicBool,
        deadline: Option<Instant>,
        interrupted: &'a dyn Fn() -> bool,
    ) -> Self {
        Self {
            flag,
            deadline,
            interrupted,
        }
    }
}
impl NamesCancellation for NamesControl<'_> {
    fn flag(&self) -> &AtomicBool {
        self.flag
    }
    fn is_canceled(&self) -> bool {
        self.flag.load(Ordering::Acquire) || (self.interrupted)()
    }
    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
}
pub(super) fn check<E>(canceled: &dyn NamesCancellation) -> Result<(), E> {
    if canceled.is_canceled() {
        return Err(ManagedNamesMetadataError::Canceled);
    }
    if canceled.deadline().is_some_and(|at| Instant::now() >= at) {
        return Err(super::SelectedFormError::Native(
            crate::viewer::analysis_parent::AnalysisError::Timeout,
        )
        .into());
    }
    Ok(())
}
pub(super) fn path(
    budget: &MemoryBudget,
    parts: &[&str],
) -> std::result::Result<BudgetedText, BudgetError> {
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for part in parts {
        out.try_push_str(part)?;
    }
    Ok(out)
}
fn emit<E>(
    canceled: &dyn NamesCancellation,
    visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    path: &str,
    body: &str,
) -> Result<(), E> {
    check(canceled)?;
    visit(path, body.as_bytes())?;
    check(canceled)
}
enum CommandDocumentKind {
    Commands,
    Home,
    Client,
}
struct CommandDocument {
    _source: Shared<EntryData>,
    body: Charged<String>,
}
impl<S: ManagedEntrySource> ManagedSnapshot<S> {
    pub fn visit_names_configdump(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        check(canceled)?;
        let context = self.open.context();
        let selected = context.read_configdump_info()?;
        if let Some(part) = &selected.value
            && let Some(body) =
                managed_configdump_xml(self.catalog.memory_budget(), context, part, document_limit)?
        {
            emit(canceled, visit, "ConfigDumpInfo.xml", &body)?;
        }
        check(canceled)
    }
    pub fn visit_names_templates(
        &self,
        canceled: &dyn NamesCancellation,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let context = self.open.context();
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            check(canceled)?;
            if let Some(selected) =
                mdconf::startup::read_selected_template(context, row.object.id, None)?
                && let Some(template) = &selected.value
            {
                let directory = self.names_object_dir(ordinal, canceled)?;
                for (name, bytes) in &template.files {
                    let key = path(budget, &[directory.as_str(), "/Ext/", name])?;
                    check(canceled)?;
                    visit(key.as_str(), bytes)?;
                    check(canceled)?;
                }
            }
            for child in row.object.templates.iter() {
                check(canceled)?;
                if let Some(selected) =
                    mdconf::startup::read_selected_template(context, row.object.id, Some(child.id))?
                    && let Some(template) = &selected.value
                {
                    let directory = self.names_object_dir(ordinal, canceled)?;
                    for (name, bytes) in &template.files {
                        let key = path(
                            budget,
                            &[
                                directory.as_str(),
                                "/Templates/",
                                &child.name,
                                "/Ext/",
                                name,
                            ],
                        )?;
                        check(canceled)?;
                        visit(key.as_str(), bytes)?;
                        check(canceled)?;
                    }
                }
            }
            for (owner, directory) in self.names_external_dirs(ordinal, canceled)?.iter() {
                for (child, name) in self.names_external_subs(*owner, SubKind::Template)? {
                    check(canceled)?;
                    if let Some(selected) =
                        mdconf::startup::read_selected_template(context, *owner, Some(child))?
                        && let Some(template) = &selected.value
                    {
                        for (file, bytes) in &template.files {
                            let key = path(
                                budget,
                                &[directory.as_str(), "/Templates/", name, "/Ext/", file],
                            )?;
                            check(canceled)?;
                            visit(key.as_str(), bytes)?;
                            check(canceled)?;
                        }
                    }
                }
            }
        }
        check(canceled)
    }
    pub fn visit_names_flowcharts(
        &self,
        canceled: &dyn NamesCancellation,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self
            .open
            .objects()
            .iter()
            .enumerate()
            .filter(|(_, row)| row.parent.is_none())
        {
            check(canceled)?;
            if let Some(selected) =
                mdconf::startup::read_selected_flowchart(self.open.context(), row.object.id)?
                && let Some(flowchart) = &selected.value
            {
                let directory = self.names_object_dir(ordinal, canceled)?;
                let key = path(budget, &[directory.as_str(), "/Ext/Flowchart.xml"])?;
                check(canceled)?;
                visit(key.as_str(), &flowchart.xml)?;
                check(canceled)?;
            }
        }
        check(canceled)
    }
    fn names_command_document(
        &self,
        id: brace::Guid,
        suffix: &str,
        kind: CommandDocumentKind,
        document_limit: u64,
    ) -> Result<Option<CommandDocument>, S::Error> {
        let context = self.open.context();
        let budget = self.catalog.memory_budget();
        let mut key = path(budget, &[])?;
        if std::fmt::Write::write_fmt(&mut key, format_args!("{id}.{suffix}")).is_err() {
            return Err(key.error().copied().unwrap_or(BudgetError::Overflow).into());
        }
        let entries = context.entries()?;
        let Some(source) = entries
            .get(key.as_str())
            .map_err(ManagedConfigurationError::from)?
        else {
            return Ok(None);
        };
        let EntryData::Brace(doc) = &*source else {
            return Ok(None);
        };
        let resolve = |id| context.index().names.get(&id).map(|name| name.as_str());
        let body = match kind {
            CommandDocumentKind::Commands => {
                managed_command_interface_xml(budget, &doc.root, resolve, document_limit)?
            }
            CommandDocumentKind::Home => {
                managed_home_page_xml(budget, &doc.root, resolve, document_limit)?
            }
            CommandDocumentKind::Client => {
                managed_client_interface_xml(budget, &doc.root, document_limit)?
            }
        };
        Ok(body.map(|body| CommandDocument {
            _source: source,
            body,
        }))
    }
    pub fn visit_names_help(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let context = self.open.context();
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            check(canceled)?;
            if let Some(selected) = context.read_object_help(row.object.id)?
                && let Some(help) = selected.value.as_ref()
            {
                let dir = self.names_object_dir(ordinal, canceled)?;
                self.visit_names_help_part(canceled, dir.as_str(), help, document_limit, visit)?;
            }
            for form in row.object.forms.iter() {
                check(canceled)?;
                if let Some(selected) = context.read_form_help(row.object.id, form.id)?
                    && let Some(help) = selected.value.as_ref()
                {
                    let dir = self.names_object_dir(ordinal, canceled)?;
                    let dir = path(budget, &[dir.as_str(), "/Forms/", &form.name])?;
                    self.visit_names_help_part(
                        canceled,
                        dir.as_str(),
                        help,
                        document_limit,
                        visit,
                    )?;
                }
            }
        }
        check(canceled)?;
        let selected = context.read_configuration_help()?;
        if let Some(help) = selected.value.as_ref() {
            self.visit_names_help_part(canceled, "", help, document_limit, visit)?;
        }
        check(canceled)
    }
    fn visit_names_help_part(
        &self,
        canceled: &dyn NamesCancellation,
        directory: &str,
        help: &mdconf::parts::Help,
        limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        let body = managed_help_xml(budget, help, limit)?;
        let dir = path(
            budget,
            &[
                directory,
                if directory.is_empty() {
                    "Ext/"
                } else {
                    "/Ext/"
                },
            ],
        )?;
        let index = self.open.context().index();
        let root_name = path(
            budget,
            &["Configuration.", self.open.identity().name.as_str()],
        )?;
        let resolve = |id| {
            index
                .names
                .get(&id)
                .map(|name| name.as_str())
                .or_else(|| (id == index.cfg_id).then_some(root_name.as_str()))
        };
        for (language, html) in &help.pages {
            check(canceled)?;
            let page = managed_help_page(budget, html, &help.files, resolve, limit)?;
            let key = path(budget, &[dir.as_str(), "Help/", language, ".html"])?;
            check(canceled)?;
            visit(key.as_str(), &page)?;
            check(canceled)?;
        }
        for (name, data) in &help.files {
            let key = path(budget, &[dir.as_str(), "Help/_files/", name])?;
            check(canceled)?;
            visit(key.as_str(), data)?;
            check(canceled)?;
        }
        let key = path(budget, &[dir.as_str(), "Help.xml"])?;
        emit(canceled, visit, key.as_str(), &body)
    }
    pub fn visit_names_pictures(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let context = self.open.context();
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            check(canceled)?;
            let Some(selected) = context.read_common_picture(row.object.id)? else {
                continue;
            };
            let Some(picture) = selected.value.as_ref() else {
                continue;
            };
            let dir = self.names_object_dir(ordinal, canceled)?;
            let ext = path(self.catalog.memory_budget(), &[dir.as_str(), "/Ext"])?;
            self.visit_names_picture(
                canceled,
                ext.as_str(),
                "Picture",
                picture,
                document_limit,
                visit,
            )?;
        }
        for (ordinal, stem) in ["Logo", "Splash", "MainSectionPicture"]
            .into_iter()
            .enumerate()
        {
            check(canceled)?;
            let selected = context
                .read_configuration_picture(ordinal)?
                .ok_or(ManagedNamesMetadataError::Inventory)?;
            if let Some(picture) = selected.value.as_ref() {
                self.visit_names_picture(canceled, "Ext", stem, picture, document_limit, visit)?;
            }
        }
        check(canceled)
    }
    fn visit_names_picture(
        &self,
        canceled: &dyn NamesCancellation,
        directory: &str,
        stem: &str,
        picture: &mdconf::parts::Picture,
        limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        let body = managed_picture_xml(budget, picture, limit)?;
        let key = path(budget, &[directory, "/", stem, "/Picture.", &picture.ext])?;
        check(canceled)?;
        visit(key.as_str(), &picture.data)?;
        check(canceled)?;
        let key = path(budget, &[directory, "/", stem, ".xml"])?;
        emit(canceled, visit, key.as_str(), &body)
    }
    pub fn visit_names_command_interfaces(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self
            .open
            .objects()
            .iter()
            .enumerate()
            .filter(|(_, row)| row.object.kind == mdconf::MdKind::Subsystem)
        {
            check(canceled)?;
            if let Some(document) = self.names_command_document(
                row.object.id,
                "1",
                CommandDocumentKind::Commands,
                document_limit,
            )? {
                let dir = self.names_object_dir(ordinal, canceled)?;
                let rel = path(budget, &[dir.as_str(), "/Ext/CommandInterface.xml"])?;
                emit(canceled, visit, rel.as_str(), &document.body)?;
            }
        }
        for (suffix, name, kind) in [
            ("a", "CommandInterface.xml", CommandDocumentKind::Commands),
            (
                "9",
                "MainSectionCommandInterface.xml",
                CommandDocumentKind::Commands,
            ),
            ("8", "HomePageWorkArea.xml", CommandDocumentKind::Home),
            (
                "b",
                "ClientApplicationInterface.xml",
                CommandDocumentKind::Client,
            ),
        ] {
            check(canceled)?;
            if let Some(document) = self.names_command_document(
                self.open.context().index().header_id,
                suffix,
                kind,
                document_limit,
            )? {
                let rel = path(budget, &["Ext/", name])?;
                emit(canceled, visit, rel.as_str(), &document.body)?;
            }
        }
        check(canceled)
    }

    pub fn visit_names_binary_parts(
        &self,
        canceled: &dyn NamesCancellation,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            check(canceled)?;
            let Some(selected) = self.open.context().read_binary_parts(row.object.id)? else {
                continue;
            };
            check(canceled)?;
            let Some(parts) = selected.value.as_ref() else {
                continue;
            };
            if parts.binary_modules.is_empty()
                && parts.ws_definition.is_empty()
                && parts.xdto_package.is_none()
            {
                continue;
            }
            let dir = self.names_object_dir(ordinal, canceled)?;
            for (name, bytes) in parts.binary_modules.iter().chain(&parts.ws_definition) {
                let rel = path(budget, &[dir.as_str(), "/Ext/", name])?;
                check(canceled)?;
                visit(rel.as_str(), bytes)?;
                check(canceled)?;
            }
            if let Some(bytes) = &parts.xdto_package {
                let rel = path(budget, &[dir.as_str(), "/Ext/Package.bin"])?;
                check(canceled)?;
                visit(rel.as_str(), bytes)?;
                check(canceled)?;
            }
        }
        check(canceled)?;
        let selected = self.open.context().read_configuration_binaries()?;
        check(canceled)?;
        if let Some(files) = selected.value.as_ref() {
            for (name, bytes) in files.iter() {
                let rel = path(budget, &["Ext/", name])?;
                check(canceled)?;
                visit(rel.as_str(), bytes)?;
                check(canceled)?;
            }
        }
        check(canceled)
    }

    pub fn visit_names_styles(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            check(canceled)?;
            let Some(selected) = self.open.context().read_style(row.object.id)? else {
                continue;
            };
            check(canceled)?;
            let Some(style) = selected.value.as_ref() else {
                continue;
            };
            if let Some(body) = managed_style_xml(budget, style, document_limit)? {
                let dir = self.names_object_dir(ordinal, canceled)?;
                let rel = path(budget, &[dir.as_str(), "/Ext/Style.xml"])?;
                emit(canceled, visit, rel.as_str(), &body)?;
            }
        }
        check(canceled)
    }

    pub fn visit_names_schedules(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            check(canceled)?;
            let Some(selected) = self.open.context().read_schedule(row.object.id)? else {
                continue;
            };
            check(canceled)?;
            let Some(schedule) = selected.value.as_ref() else {
                continue;
            };
            if let Some(body) = managed_schedule_xml(budget, schedule, document_limit)? {
                let dir = self.names_object_dir(ordinal, canceled)?;
                let rel = path(budget, &[dir.as_str(), "/Ext/Schedule.xml"])?;
                emit(canceled, visit, rel.as_str(), &body)?;
            }
        }
        check(canceled)
    }
    pub fn visit_names_exchange_content(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            check(canceled)?;
            let Some(selected) = self.open.context().read_exchange_content(row.object.id)? else {
                continue;
            };
            check(canceled)?;
            let Some(items) = selected.value.as_ref() else {
                continue;
            };
            let body = managed_content_xml(budget, items, document_limit)?;
            let dir = self.names_object_dir(ordinal, canceled)?;
            let rel = path(budget, &[dir.as_str(), "/Ext/Content.xml"])?;
            emit(canceled, visit, rel.as_str(), &body)?;
        }
        check(canceled)
    }
    pub fn visit_names_predefined(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self
            .open
            .objects()
            .iter()
            .enumerate()
            .filter(|(_, row)| row.parent.is_none())
        {
            check(canceled)?;
            let Some(selected) = self.open.predefined().read(row.object.id)? else {
                continue;
            };
            check(canceled)?;
            let Some(predefined) = selected.predefined.as_ref() else {
                continue;
            };
            if let Some(body) =
                managed_predefined_xml(budget, row.object.kind, predefined, document_limit)?
            {
                let dir = self.names_object_dir(ordinal, canceled)?;
                let rel = path(budget, &[dir.as_str(), "/Ext/Predefined.xml"])?;
                emit(canceled, visit, rel.as_str(), &body)?;
            }
        }
        check(canceled)
    }
    pub fn visit_names_rights(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        for (ordinal, row) in self
            .open
            .objects()
            .iter()
            .enumerate()
            .filter(|(_, row)| row.parent.is_none() && row.object.kind == mdconf::MdKind::Role)
        {
            check(canceled)?;
            let selected = self
                .open
                .context()
                .read_rights(row.object.id)?
                .ok_or(ManagedNamesMetadataError::Inventory)?;
            check(canceled)?;
            let Some(rights) = selected.rights.as_ref() else {
                continue;
            };
            if let Some(body) = managed_rights_xml(
                budget,
                self.open.context().index().cfg_id,
                rights,
                document_limit,
            )? {
                let dir = self.names_object_dir(ordinal, canceled)?;
                let rel = path(budget, &[dir.as_str(), "/Ext/Rights.xml"])?;
                emit(canceled, visit, rel.as_str(), &body)?;
            }
        }
        check(canceled)
    }
    pub fn visit_names_modules(
        &self,
        canceled: &dyn NamesCancellation,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        for (ordinal, descriptor) in self.open.modules().iter().enumerate() {
            check(canceled)?;
            let mut rel = if let Some(owner) = descriptor.object {
                self.names_object_dir(owner, canceled)?
            } else {
                path(budget, &[])?
            };
            let suffix = match descriptor.location {
                ModuleLocation::Configuration if descriptor.object.is_none() => "",
                ModuleLocation::Object if descriptor.object.is_some() => "/",
                location => {
                    let object = &self
                        .open
                        .objects()
                        .get(
                            descriptor
                                .object
                                .ok_or(ManagedNamesMetadataError::Inventory)?,
                        )
                        .ok_or(ManagedNamesMetadataError::Inventory)?
                        .object;
                    let (folder, name) = match location {
                        ModuleLocation::Command(at) => (
                            "/Commands/",
                            object.children.get(at).map(|row| row.name.as_str()),
                        ),
                        ModuleLocation::Form(at) => {
                            ("/Forms/", object.forms.get(at).map(|row| row.name.as_str()))
                        }
                        ModuleLocation::Recalculation(at) => (
                            "/Recalculations/",
                            object.recalculations.get(at).map(|row| row.name.as_str()),
                        ),
                        _ => return Err(ManagedNamesMetadataError::Inventory),
                    };
                    rel.try_push_str(folder)?;
                    rel.try_push_str(name.ok_or(ManagedNamesMetadataError::Inventory)?)?;
                    "/"
                }
            };
            rel.try_push_str(suffix)?;
            rel.try_push_str("Ext/")?;
            if descriptor.slot.kind == ModuleKind::FormModule {
                rel.try_push_str("Form/Module.bsl")?;
            } else {
                rel.try_push_str(crate::viewer::names::module_label(
                    descriptor.slot.kind,
                    crate::viewer::Lang::En,
                ))?;
                rel.try_push_str(".bsl")?;
            }
            let key = self
                .open
                .module_key(ordinal)
                .ok_or(ManagedNamesMetadataError::Inventory)?;
            let selected = self.open.read_selected_module(key)?;
            let module = selected
                .module
                .as_ref()
                .ok_or(ManagedNamesMetadataError::Inventory)?;
            check(canceled)?;
            visit(rel.as_str(), &module.source)?;
            check(canceled)?;
        }
        for (ordinal, row) in self.open.objects().iter().enumerate() {
            check(canceled)?;
            if !self.has_external(row.object.id) {
                continue;
            }
            let key = self
                .open
                .object_key(ordinal)
                .ok_or(ManagedNamesMetadataError::Inventory)?;
            let selected = self.open.read_selected_object(key)?;
            let dir = self.names_object_dir(ordinal, canceled)?;
            for value in &selected.object.external {
                self.visit_external_modules(value, dir.as_str(), canceled, visit)?;
            }
        }
        check(canceled)
    }
    fn has_external(&self, owner: Guid) -> bool {
        self.open
            .context()
            .index()
            .external
            .get(&owner)
            .is_some_and(|ids| !ids.is_empty())
    }
    fn visit_external_modules(
        &self,
        value: &ExternalObject,
        base: &str,
        canceled: &dyn NamesCancellation,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let budget = self.catalog.memory_budget();
        let dir = path(budget, &[base, "/", value.kind.folder(), "/", &value.name])?;
        let entries = self.open.context().entries()?;
        let mut emit_slot = |slot: ModuleSlot, rel: &[&str]| -> Result<(), S::Error> {
            check(canceled)?;
            let read = slot
                .read(&entries)
                .map_err(ManagedConfigurationError::from)?;
            if let Some(module) = &read.module {
                let rel = path(budget, rel)?;
                check(canceled)?;
                visit(rel.as_str(), &module.source)?;
            }
            check(canceled)
        };
        for (suffix, file) in modules(value.kind.as_str()) {
            if let Some(kind) = module_kind(file) {
                emit_slot(
                    ModuleSlot::separate(value.id, suffix, kind),
                    &[dir.as_str(), "/Ext/", file],
                )?;
            }
        }
        for child in value.children.iter().filter(|c| c.tag == ChildTag::Command) {
            emit_slot(
                ModuleSlot::separate(child.id, "2", ModuleKind::CommandModule),
                &[
                    dir.as_str(),
                    "/Commands/",
                    &child.name,
                    "/Ext/CommandModule.bsl",
                ],
            )?;
        }
        for form in &value.forms {
            emit_slot(
                ModuleSlot::form(form.id),
                &[dir.as_str(), "/Forms/", &form.name, "/Ext/Form/Module.bsl"],
            )?;
        }
        drop(emit_slot);
        drop(entries);
        for nested in &value.nested {
            self.visit_external_modules(nested, dir.as_str(), canceled, visit)?;
        }
        Ok(())
    }
    pub(super) fn names_external_dirs(
        &self,
        ordinal: usize,
        canceled: &dyn NamesCancellation,
    ) -> Result<BudgetedVec<(Guid, BudgetedText)>, S::Error> {
        let budget = self.catalog.memory_budget();
        let index = self.open.context().index();
        let mut out = BudgetedVec::new(budget, Category::Metadata)?;
        let owner = self
            .open
            .objects()
            .get(ordinal)
            .ok_or(ManagedNamesMetadataError::Inventory)?
            .object
            .id;
        if !self.has_external(owner) {
            return Ok(out);
        }
        let base = self.names_object_dir(ordinal, canceled)?;
        let name = |id: &Guid| -> Result<&str, S::Error> {
            index
                .names
                .get(id)
                .and_then(|full| full.as_str().rsplit('.').next())
                .ok_or(ManagedNamesMetadataError::Inventory)
        };
        for (kind, id) in index
            .external
            .get(&owner)
            .map(|ids| ids.as_slice())
            .unwrap_or_default()
        {
            check(canceled)?;
            let dir = path(budget, &[base.as_str(), "/", kind.folder(), "/", name(id)?])?;
            out.try_push((*id, dir))?;
            let at = out.len() - 1;
            for (nested_kind, nested) in index
                .external
                .get(id)
                .map(|ids| ids.as_slice())
                .unwrap_or_default()
            {
                check(canceled)?;
                let parent = &out.get(at).ok_or(ManagedNamesMetadataError::Inventory)?.1;
                let dir = path(
                    budget,
                    &[
                        parent.as_str(),
                        "/",
                        nested_kind.folder(),
                        "/",
                        name(nested)?,
                    ],
                )?;
                out.try_push((*nested, dir))?;
            }
        }
        Ok(out)
    }
    pub(super) fn names_external_subs(
        &self,
        owner: Guid,
        kind: SubKind,
    ) -> Result<impl Iterator<Item = (Guid, &str)>, S::Error> {
        let index = self.open.context().index();
        let subs = index
            .subs
            .get(&owner)
            .map(|subs| subs.as_slice())
            .unwrap_or_default();
        for (_, id) in subs {
            if index.names.get(id).is_none() {
                return Err(ManagedNamesMetadataError::Inventory);
            }
        }
        Ok(subs
            .iter()
            .filter(move |(sub, _)| *sub == kind)
            .filter_map(move |(_, id)| {
                let name = index.names.get(id)?.as_str().rsplit('.').next()?;
                Some((*id, name))
            }))
    }
    pub fn visit_names_metadata(
        &self,
        canceled: &dyn NamesCancellation,
        document_limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        for nested in [false, true] {
            for (ordinal, row) in self.open.objects().iter().enumerate() {
                check(canceled)?;
                if row.parent.is_some() == nested {
                    self.visit_names_owner(ordinal, canceled, document_limit, visit)?;
                }
            }
        }
        check(canceled)?;
        let settings = self.open.context().read_configuration()?;
        check(canceled)?;
        let names = self
            .open
            .objects()
            .iter()
            .filter(|row| row.parent.is_none())
            .map(|row| (row.object.kind.as_str(), row.object.name.as_str()));
        if let Some(body) = managed_configuration_xml(
            self.catalog.memory_budget(),
            self.open.context().index().cfg_id,
            &settings.props,
            &settings.contained_objects,
            names,
            document_limit,
        )? {
            emit(canceled, visit, "Configuration.xml", &body)?;
        }
        check(canceled)
    }
    fn visit_names_owner(
        &self,
        ordinal: usize,
        canceled: &dyn NamesCancellation,
        limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        let key = self
            .open
            .object_key(ordinal)
            .ok_or(ManagedNamesMetadataError::Inventory)?;
        let selected = self.open.read_selected_object(key)?;
        check(canceled)?;
        let object = &selected.object;
        let Some(spec) = kind_spec(object.kind.as_str()).filter(|spec| spec.header().is_some())
        else {
            return Ok(());
        };
        let budget = self.catalog.memory_budget();
        let dir = self.names_object_dir(ordinal, canceled)?;
        let tail = self
            .open
            .objects()
            .get(ordinal.checked_add(1).ok_or(BudgetError::Overflow)?..)
            .ok_or(ManagedNamesMetadataError::Inventory)?;
        let names = tail
            .iter()
            .take_while(|row| row.parent.is_some_and(|parent| parent >= ordinal))
            .filter(|row| row.parent == Some(ordinal))
            .map(|row| row.object.name.as_str());
        if let Some(body) = managed_object_xml(budget, object, spec, names, limit)? {
            let rel = path(budget, &[dir.as_str(), ".xml"])?;
            emit(canceled, visit, rel.as_str(), &body)?;
        } else {
            return Ok(());
        }
        for sub in object.forms.iter().chain(&object.templates) {
            check(canceled)?;
            if let Some(body) = managed_subobject_xml(budget, sub, limit)? {
                let folder = match sub.kind {
                    SubKind::Form => "/Forms/",
                    SubKind::Template => "/Templates/",
                };
                let rel = path(budget, &[dir.as_str(), folder, &sub.name, ".xml"])?;
                emit(canceled, visit, rel.as_str(), &body)?;
            }
        }
        for value in &object.external {
            self.visit_names_external(value, dir.as_str(), canceled, limit, visit)?;
        }
        for recalc in &object.recalculations {
            check(canceled)?;
            let body = managed_recalculation_xml(budget, &object.name, recalc, limit)?;
            let rel = path(
                budget,
                &[dir.as_str(), "/Recalculations/", &recalc.name, ".xml"],
            )?;
            emit(canceled, visit, rel.as_str(), &body)?;
        }
        check(canceled)
    }
    fn visit_names_external(
        &self,
        value: &ExternalObject,
        base: &str,
        canceled: &dyn NamesCancellation,
        limit: u64,
        visit: &mut impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> Result<(), S::Error> {
        check(canceled)?;
        let budget = self.catalog.memory_budget();
        let full = self
            .open
            .context()
            .index()
            .names
            .get(&value.id)
            .ok_or(ManagedNamesMetadataError::Inventory)?;
        let Some(body) = managed_external_xml(budget, value, full.as_str(), limit)? else {
            return Ok(());
        };
        let dir = path(budget, &[base, "/", value.kind.folder(), "/", &value.name])?;
        let rel = path(budget, &[dir.as_str(), ".xml"])?;
        emit(canceled, visit, rel.as_str(), &body)?;
        drop(body);
        for sub in value.forms.iter().chain(&value.templates) {
            check(canceled)?;
            if let Some(body) = managed_subobject_xml(budget, sub, limit)? {
                let folder = match sub.kind {
                    SubKind::Form => "/Forms/",
                    SubKind::Template => "/Templates/",
                };
                let rel = path(budget, &[dir.as_str(), folder, &sub.name, ".xml"])?;
                emit(canceled, visit, rel.as_str(), &body)?;
            }
        }
        for nested in &value.nested {
            self.visit_names_external(nested, dir.as_str(), canceled, limit, visit)?;
        }
        Ok(())
    }
    fn eager_names_ordinal(
        &self,
        ordinal: usize,
        canceled: &dyn NamesCancellation,
    ) -> Result<usize, S::Error> {
        let rows = self.open.objects();
        let wanted = &rows
            .get(ordinal)
            .ok_or(ManagedNamesMetadataError::Inventory)?
            .object;
        if wanted.kind != mdconf::MdKind::Subsystem {
            return Ok(ordinal);
        }
        let mut stack = BudgetedVec::new(self.catalog.memory_budget(), Category::Metadata)?;
        for (at, row) in rows.iter().enumerate() {
            check(canceled)?;
            if row.parent.is_none() {
                stack.try_push(at)?;
            }
        }
        let mut visited = 0usize;
        while let Some(at) = stack.pop() {
            check(canceled)?;
            if visited >= rows.len() {
                return Err(ManagedNamesMetadataError::Inventory);
            }
            visited += 1;
            let object = &rows
                .get(at)
                .ok_or(ManagedNamesMetadataError::Inventory)?
                .object;
            if object.id == wanted.id && object.kind == wanted.kind && object.name == wanted.name {
                return Ok(at);
            }
            for (relative, row) in rows
                .get(at.checked_add(1).ok_or(BudgetError::Overflow)?..)
                .ok_or(ManagedNamesMetadataError::Inventory)?
                .iter()
                .take_while(|row| row.parent.is_some_and(|parent| parent >= at))
                .enumerate()
            {
                check(canceled)?;
                if row.parent == Some(at) {
                    stack.try_push(
                        at.checked_add(1)
                            .and_then(|start| start.checked_add(relative))
                            .ok_or(BudgetError::Overflow)?,
                    )?;
                }
            }
        }
        Err(ManagedNamesMetadataError::Inventory)
    }
    pub(super) fn names_object_dir(
        &self,
        ordinal: usize,
        canceled: &dyn NamesCancellation,
    ) -> Result<BudgetedText, S::Error> {
        let rows = self.open.objects();
        let budget = self.catalog.memory_budget();
        let mut ancestors = BudgetedVec::new(budget, Category::Metadata)?;
        let mut at = Some(self.eager_names_ordinal(ordinal, canceled)?);
        while let Some(ordinal) = at {
            check(canceled)?;
            if ancestors.len() >= rows.len() {
                return Err(ManagedNamesMetadataError::Inventory);
            }
            let row = rows
                .get(ordinal)
                .ok_or(ManagedNamesMetadataError::Inventory)?;
            ancestors.try_push(ordinal)?;
            at = row.parent;
        }
        let mut out = path(budget, &[])?;
        for (position, ordinal) in ancestors.iter().rev().enumerate() {
            check(canceled)?;
            if position != 0 {
                out.try_push_str("/")?;
            }
            let object = &rows
                .get(*ordinal)
                .ok_or(ManagedNamesMetadataError::Inventory)?
                .object;
            if let Some(folder) = object.kind.folder() {
                out.try_push_str(folder)?;
                out.try_push_str("/")?;
                out.try_push_str(&object.name)?;
            }
        }
        Ok(out)
    }
}
