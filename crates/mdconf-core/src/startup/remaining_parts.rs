use crate::startup::{
    ManagedConfigurationGraph, ManagedIndex, ManagedObjectsError, ManagedPredefined,
    index::{GuidText, text},
    remaining_cmdi, remaining_configdump, remaining_predefined, remaining_rights,
    source_copy::copy_node,
    values::empty,
};
use crate::{ConfigSettings, EntryData, ManagedEntries, ManagedEntrySource, MdKind};
use memory_budget::{BudgetError, Charged};
impl ManagedConfigurationGraph {
    pub fn load_remaining_parts<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        predef: &ManagedPredefined,
        settings: &Charged<ConfigSettings>,
        keep: bool,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        if self.remaining_loaded
            || !self.graph.modules.reservation().budget().same_scope(budget)
            || !self.graph.diagnostics.memory_budget().same_scope(budget)
            || !index.owns_scope(budget)
            || !self
                .graph
                .objects
                .objects
                .reservation()
                .budget()
                .same_scope(budget)
            || !self.parts.reservation().budget().same_scope(budget)
            || !self.props.reservation().budget().same_scope(budget)
            || !index.memory_budget().same_scope(budget)
            || !predef.memory_budget().same_scope(budget)
            || !settings.reservation().budget().same_scope(budget)
        {
            return Err(BudgetError::Overflow.into());
        }
        if self.parts.command_interface.is_some()
            || self.parts.main_section.is_some()
            || self.parts.home_page.is_some()
            || self.parts.client_interface.is_some()
            || self.parts.dump_info.is_some()
            || self.graph.objects.all_objects().any(|o| {
                o.parts.rights.is_some()
                    || o.parts.predefined.is_some()
                    || o.parts.command_interface.is_some()
            })
        {
            return Err(BudgetError::Overflow.into());
        }
        self = self.install_rights(entries, index, settings, keep)?;
        self = self.install_predefined(entries, index, predef, keep)?;
        self = self.install_command_interfaces(entries, index, keep)?;
        self = self.install_configdump(entries, index, settings, keep)?;
        self.remaining_loaded = true;
        Ok(self)
    }
    pub(super) fn install_rights<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        settings: &Charged<ConfigSettings>,
        keep: bool,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        for at in 0..self.graph.objects.locations.len() {
            let object = self
                .graph
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?;
            if object.kind != MdKind::Role
                || self.graph.objects.objects.iter().all(|o| o.id != object.id)
            {
                continue;
            }
            let id = GuidText::new(object.id);
            let name = text(budget, &[id.as_str()?, ".0"])?;
            let data = entries.get(name.as_str())?;
            if !keep {
                entries.forget(name.as_str());
            }
            let node = match data.as_deref() {
                None => continue,
                Some(EntryData::Brace(doc)) => &doc.root,
                Some(_) => {
                    self.graph.diagnostics.miss(&["rights:not_brace"])?;
                    continue;
                }
            };
            let actual = match remaining_rights::decode(index, settings, node, keep) {
                Ok(value) => value,
                Err(remaining_rights::Error::Memory(error)) => return Err(error.into()),
                Err(remaining_rights::Error::Semantic(why, detail)) => {
                    self.graph
                        .diagnostics
                        .miss(&["rights:", why, detail.unwrap_or("")])?;
                    continue;
                }
            };
            self.graph.objects = self.graph.objects.install(
                at,
                actual,
                |o| o.parts.rights.is_none(),
                |o, p| {
                    o.parts.rights = Some(p);
                    None
                },
            )?;
        }
        Ok(self)
    }
    pub(super) fn install_predefined<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        predef: &ManagedPredefined,
        keep: bool,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        let mut tables =
            memory_budget::BudgetedVec::new(budget, memory_budget::Category::Metadata)?;
        for (at, object) in self
            .graph
            .objects
            .locations
            .iter()
            .enumerate()
            .filter_map(|(at, _)| self.graph.objects.resolve(at).map(|o| (at, o)))
        {
            if self.graph.objects.objects.iter().all(|o| o.id != object.id) {
                continue;
            }
            let Some(suffix) = crate::predefined::table_suffix(object.kind) else {
                continue;
            };
            let id = GuidText::new(object.id);
            let name = text(budget, &[id.as_str()?, ".", suffix])?;
            if !entries.contains(name.as_str()) {
                continue;
            }
            let data = entries.get(name.as_str())?;
            if !keep {
                entries.forget(name.as_str());
            }
            match data {
                None => {}
                Some(data) if matches!(&*data, EntryData::Brace(_)) => {
                    tables.try_push((at, object.kind, data))?
                }
                Some(_) => self.graph.diagnostics.miss(&["predef:not_brace"])?,
            }
        }
        let mut full = crate::startup::GuidMap::new(budget)?;
        for (at, kind, data) in tables.iter() {
            let EntryData::Brace(doc) = &**data else {
                return Err(BudgetError::Overflow.into());
            };
            let mut local = ManagedPredefined::new(budget)?;
            local.index_table(*kind, &doc.root)?;
            let owner = self
                .graph
                .objects
                .resolve(*at)
                .ok_or(BudgetError::Overflow)?;
            for (id, item) in local.iter() {
                full.insert(
                    *id,
                    text(budget, &[kind.as_str(), ".", &owner.name, ".", item])?,
                )?;
            }
        }
        let mut predefined = memory_budget::BudgetedVec::try_with_capacity(
            budget,
            memory_budget::Category::Metadata,
            tables.len(),
        )?;
        for (at, _, data) in tables.iter() {
            let EntryData::Brace(doc) = &**data else {
                return Err(BudgetError::Overflow.into());
            };
            match remaining_predefined::decode(index, predef, &full, &doc.root, keep) {
                Ok(value) => predefined.try_push((*at, value))?,
                Err(remaining_predefined::Error::Memory(error)) => return Err(error.into()),
                Err(remaining_predefined::Error::Semantic(why, detail)) => {
                    self.graph
                        .diagnostics
                        .miss(&["predef:", why, detail.unwrap_or("")])?
                }
            }
        }
        predefined.reverse();
        while let Some((at, value)) = predefined.pop() {
            self.graph.objects = self.graph.objects.install(
                at,
                value,
                |o| o.parts.predefined.is_none(),
                |o, p| {
                    o.parts.predefined = Some(p);
                    None
                },
            )?;
        }
        Ok(self)
    }
    pub(super) fn install_command_interfaces<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,

        keep: bool,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        for at in 0..self.graph.objects.locations.len() {
            let object = self
                .graph
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?;
            if object.kind != MdKind::Subsystem {
                continue;
            }
            let id = GuidText::new(object.id);
            let name = text(budget, &[id.as_str()?, ".1"])?;
            let data = entries.get(name.as_str())?;
            if !keep {
                entries.forget(name.as_str());
            }
            let node = match data.as_deref() {
                None => continue,
                Some(EntryData::Brace(doc)) => &doc.root,
                Some(_) => {
                    self.graph.diagnostics.miss(&["cmdi:ci:not_brace"])?;
                    continue;
                }
            };
            if let Err(why) = remaining_cmdi::command_interface(index, node) {
                self.graph.diagnostics.miss(&["cmdi:ci:", why])?;
                continue;
            }
            let actual =
                copy_node(budget, node)?.map(|source| crate::parts::CommandInterface { source });
            self.graph.objects = self.graph.objects.install(
                at,
                actual,
                |o| o.parts.command_interface.is_none(),
                |o, p| {
                    o.parts.command_interface = Some(p);
                    None
                },
            )?;
        }
        for (suffix, file) in [("a", "ci"), ("9", "main_ci"), ("8", "hp"), ("b", "cai")] {
            let id = GuidText::new(index.header_id);
            let name = text(budget, &[id.as_str()?, ".", suffix])?;
            let data = entries.get(name.as_str())?;
            if !keep {
                entries.forget(name.as_str());
            }
            let node = match data.as_deref() {
                None => continue,
                Some(EntryData::Brace(doc)) => &doc.root,
                Some(_) => {
                    self.graph
                        .diagnostics
                        .miss(&["cmdi:", file, ":not_brace"])?;
                    continue;
                }
            };
            let validated = match file {
                "hp" => remaining_cmdi::home_page(index, node),
                "cai" => remaining_cmdi::client_interface(node),
                _ => remaining_cmdi::command_interface(index, node),
            };
            if let Err(why) = validated {
                self.graph.diagnostics.miss(&["cmdi:", file, ":", why])?;
                continue;
            }
            let source = copy_node(budget, node)?;
            let parts = std::mem::replace(
                &mut self.parts,
                empty(budget, crate::parts::ConfigParts::default())?,
            );
            self.parts = parts.try_zip(source)?.map(|(mut parts, source)| {
                match file {
                    "ci" => {
                        parts.command_interface = Some(crate::parts::CommandInterface { source })
                    }
                    "main_ci" => {
                        parts.main_section = Some(crate::parts::CommandInterface { source })
                    }
                    "hp" => parts.home_page = Some(crate::parts::RawPart { node: source }),
                    _ => parts.client_interface = Some(crate::parts::RawPart { node: source }),
                }
                parts
            });
        }
        Ok(self)
    }
    pub(super) fn install_configdump<S: ManagedEntrySource + ?Sized>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        settings: &Charged<ConfigSettings>,
        keep: bool,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        let data = entries.get("versions")?;
        if !keep {
            entries.forget("versions");
        }
        match data.as_deref() {
            Some(EntryData::Brace(doc)) => {
                if let Some(actual) = remaining_configdump::decode(
                    index,
                    settings,
                    &doc.root,
                    keep,
                    &mut self.graph.diagnostics,
                )? {
                    let parts = std::mem::replace(
                        &mut self.parts,
                        empty(budget, crate::parts::ConfigParts::default())?,
                    );
                    self.parts = parts.try_zip(actual)?.map(|(mut parts, actual)| {
                        parts.dump_info = Some(actual);
                        parts
                    });
                }
            }
            _ => self
                .graph
                .diagnostics
                .miss(&[if entries.contains("versions") {
                    "configdump:not_brace"
                } else {
                    "configdump:no_entry"
                }])?,
        }
        Ok(self)
    }
}
