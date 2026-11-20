use super::ExternalDocument;
use crate::{ModuleKind, Result, SubKind, SubObject};
use mdconf_core::external::xml_out::{
    check, document_xml, error, memory, subobject_xml, text, validate,
};
use memory_budget::{BudgetedVec, BudgetedWriter, Category, Charged, MemoryBudget};
use std::{io::Write, path::Path, sync::atomic::AtomicBool};

pub trait FormRenderer {
    fn render(
        &self,
        document: &ExternalDocument,
        form: &SubObject,
        options: &XmlDumpOptions<'_>,
        budget: &MemoryBudget,
    ) -> Result<XmlDump>;
}
pub struct XmlDumpOptions<'a> {
    pub root_name: &'a str,
    pub max_output_bytes: u64,
    pub cancel: Option<&'a AtomicBool>,
    pub form_renderer: Option<&'a dyn FormRenderer>,
}
impl<'a> XmlDumpOptions<'a> {
    pub fn new(root_name: &'a str) -> Self {
        Self {
            root_name,
            max_output_bytes: u64::MAX,
            cancel: None,
            form_renderer: None,
        }
    }
}
#[derive(Debug)]
struct File {
    path: Charged<String>,
    data: Charged<Vec<u8>>,
}
#[derive(Debug)]
pub struct XmlDump {
    files: BudgetedVec<File>,
    used: u64,
    limit: u64,
}
impl XmlDump {
    pub fn new(budget: &MemoryBudget, max_output_bytes: u64) -> Result<Self> {
        Ok(Self {
            files: BudgetedVec::new(budget, Category::Metadata).map_err(memory)?,
            used: 0,
            limit: max_output_bytes,
        })
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.files
            .iter()
            .map(|file| (file.path.as_str(), file.data.as_slice()))
    }
    pub fn get(&self, path: &str) -> Option<&[u8]> {
        self.iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| bytes)
    }
    pub fn contains_key(&self, path: &str) -> bool {
        self.get(path).is_some()
    }
    pub fn len(&self) -> usize {
        self.files.len()
    }
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
    pub fn output_bytes(&self) -> u64 {
        self.used
    }
    pub fn memory_budget(&self) -> &MemoryBudget {
        self.files.reservation().budget()
    }
    pub fn put(&mut self, path: &str, bytes: &[u8]) -> Result<()> {
        mdconf_core::external::xml_out::check_path(path)?;
        self.check_new_path(path)?;
        let cost = (path.len() as u64)
            .checked_add(bytes.len() as u64)
            .and_then(|n| self.used.checked_add(n))
            .ok_or_else(|| error(path, "output size overflow"))?;
        if cost > self.limit {
            return Err(error(path, "external XML output byte limit exceeded"));
        }
        let budget = self.files.reservation().budget();
        let path = text(budget, &[path])?;
        let mut out = BudgetedWriter::new(budget, self.limit).map_err(memory)?;
        out.try_write(bytes).map_err(memory)?;
        self.files
            .try_push(File {
                path,
                data: out.into_buffer().into_charged_vec(),
            })
            .map_err(memory)?;
        self.used = cost;
        Ok(())
    }
    fn check_new_path(&self, path: &str) -> Result<()> {
        let budget = self.memory_budget();
        let key = mdconf_core::external::xml_out::path_key(budget, path)?;
        for file in self.files.iter() {
            let seen = mdconf_core::external::xml_out::path_key(budget, &file.path)?;
            if *seen == *key
                || seen
                    .strip_prefix(key.as_str())
                    .is_some_and(|s| s.starts_with('/'))
                || key
                    .strip_prefix(seen.as_str())
                    .is_some_and(|s| s.starts_with('/'))
            {
                return Err(error(path, "duplicate or colliding output path"));
            }
            for (at, _) in path.match_indices('/') {
                let dir = path
                    .get(..at)
                    .ok_or_else(|| error(path, "invalid directory prefix"))?;
                let folded = mdconf_core::external::xml_out::path_key(budget, dir)?;
                for (end, _) in file.path.match_indices('/') {
                    let other = file
                        .path
                        .get(..end)
                        .ok_or_else(|| error(path, "invalid directory prefix"))?;
                    if dir != other
                        && *folded == *mdconf_core::external::xml_out::path_key(budget, other)?
                    {
                        return Err(error(path, "directory spelling collides by case or NFC"));
                    }
                }
            }
        }
        Ok(())
    }
}
impl std::ops::Index<&str> for XmlDump {
    type Output = [u8];
    #[allow(
        clippy::panic,
        reason = "Index contract; fallible callers use XmlDump::get"
    )]
    fn index(&self, path: &str) -> &[u8] {
        match self.get(path) {
            Some(bytes) => bytes,
            None => panic!("missing XML dump path: {path}"),
        }
    }
}
fn help(
    dump: &mut XmlDump,
    doc: &ExternalDocument,
    dir: &str,
    help: &md_model::parts::Help,
    options: &XmlDumpOptions<'_>,
) -> Result<()> {
    let budget = dump.memory_budget().clone();
    let xml = md_xmlout::managed_external_help_xml(&budget, help, options.max_output_bytes)
        .map_err(memory)?;
    dump.put(&text(&budget, &[dir, "/Ext/Help.xml"])?, xml.as_bytes())?;
    for (language, html) in &help.pages {
        check(options.cancel)?;
        mdconf_core::external::xml_out::check_basename(language)?;
        let page = md_xmlout::managed_external_help_page(
            &budget,
            html,
            &help.files,
            |id| doc.name(id),
            options.max_output_bytes,
        )
        .map_err(memory)?;
        dump.put(
            &text(&budget, &[dir, "/Ext/Help/", language, ".html"])?,
            &page,
        )?;
    }
    for (name, data) in &help.files {
        check(options.cancel)?;
        dump.put(&text(&budget, &[dir, "/Ext/Help/_files/", name])?, data)?;
    }
    Ok(())
}
pub fn dump_xml_to_memory(
    doc: &ExternalDocument,
    options: &XmlDumpOptions<'_>,
    budget: &MemoryBudget,
) -> Result<XmlDump> {
    validate(doc, budget, options.cancel)?;
    mdconf_core::external::xml_out::check_basename(options.root_name)?;
    let mut dump = XmlDump::new(budget, options.max_output_bytes)?;
    let root = document_xml(doc, budget, options.max_output_bytes, options.cancel)?;
    let root_path = text(budget, &[options.root_name, ".xml"])?;
    dump.put(&root_path, root.as_bytes())?;
    let object = &doc.object;
    let parts = &object.parts;
    if parts.rights.is_some()
        || parts.predefined.is_some()
        || parts.command_interface.is_some()
        || parts.picture.is_some()
        || parts.schedule.is_some()
        || parts.exchange_content.is_some()
        || parts.style.is_some()
        || !parts.binary_modules.is_empty()
        || !parts.ws_definition.is_empty()
        || parts.xdto_package.is_some()
    {
        return Err(error(&object.name, "unsupported external object part"));
    }
    for module in &object.modules {
        check(options.cancel)?;
        if module.kind != ModuleKind::ObjectModule {
            return Err(error(&object.name, "unsupported external module kind"));
        }
        dump.put(
            &text(budget, &[options.root_name, "/Ext/ObjectModule.bsl"])?,
            &module.source,
        )?;
    }
    if let Some(value) = &parts.help {
        help(&mut dump, doc, options.root_name, value, options)?;
    }
    for (position, (id, _)) in parts.form_help.iter().enumerate() {
        check(options.cancel)?;
        if parts
            .form_help
            .iter()
            .take(position)
            .any(|(previous, _)| previous == id)
        {
            return Err(error(&object.name, "duplicate form help"));
        }
        if !object.forms.iter().any(|form| form.id == *id) {
            return Err(error(&object.name, "help for an absent form"));
        }
    }
    for sub in object.forms.iter().chain(&object.templates) {
        check(options.cancel)?;
        mdconf_core::external::xml_out::check_basename(&sub.name)?;
        let (folder, kind) = if object.forms.iter().any(|form| std::ptr::eq(form, sub)) {
            ("Forms", SubKind::Form)
        } else {
            ("Templates", SubKind::Template)
        };
        if sub.kind != kind {
            return Err(error(&sub.name, "subobject collection kind mismatch"));
        }
        let dir = text(budget, &[options.root_name, "/", folder, "/", &sub.name])?;
        let description = subobject_xml(sub, budget, options.max_output_bytes, options.cancel)?;
        dump.put(&text(budget, &[&dir, ".xml"])?, description.as_bytes())?;
        match kind {
            SubKind::Template => {
                if sub.form.is_some() || sub.module.is_some() {
                    return Err(error(&sub.name, "template contains form content"));
                }
                let value = sub
                    .template
                    .as_ref()
                    .filter(|value| {
                        value.kind.is_some()
                            && (!value.files.is_empty()
                                || value.kind
                                    == Some(md_model::templates::TemplateType::SpreadsheetDocument))
                    })
                    .ok_or_else(|| error(&sub.name, "missing or unsupported template content"))?;
                for (path, bytes) in &value.files {
                    check(options.cancel)?;
                    mdconf_core::external::xml_out::check_path(path)?;
                    dump.put(&text(budget, &[&dir, "/Ext/", path])?, bytes)?;
                }
            }
            SubKind::Form => {
                if sub.template.is_some() || sub.form.is_none() {
                    return Err(error(&sub.name, "missing or unsupported form content"));
                }
                let renderer = options
                    .form_renderer
                    .ok_or_else(|| error(&sub.name, "an admitted form renderer is required"))?;
                let files = renderer.render(doc, sub, options, budget)?;
                if !files.memory_budget().same_scope(budget) || files.get("Form.xml").is_none() {
                    return Err(error(
                        &sub.name,
                        "form renderer returned incomplete or foreign-budget content",
                    ));
                }
                for (path, data) in files.iter() {
                    check(options.cancel)?;
                    dump.put(&text(budget, &[&dir, "/Ext/", path])?, data)?;
                }
                if let Some(module) = &sub.module {
                    if module.kind != ModuleKind::FormModule {
                        return Err(error(&sub.name, "unsupported form module kind"));
                    }
                    dump.put(
                        &text(budget, &[&dir, "/Ext/Form/Module.bsl"])?,
                        &module.source,
                    )?;
                }
                if let Some((_, value)) = parts.form_help.iter().find(|(id, _)| *id == sub.id) {
                    help(&mut dump, doc, &dir, value, options)?;
                }
            }
        }
    }
    check(options.cancel)?;
    Ok(dump)
}
struct OutputGuard<'a> {
    path: &'a Path,
    keep: bool,
}
impl Drop for OutputGuard<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(self.path);
        }
    }
}
pub fn dump_xml(
    doc: &ExternalDocument,
    out_directory: &Path,
    options: &XmlDumpOptions<'_>,
    budget: &MemoryBudget,
) -> Result<usize> {
    let dump = dump_xml_to_memory(doc, options, budget)?;
    check(options.cancel)?;
    std::fs::create_dir(out_directory)?;
    let mut guard = OutputGuard {
        path: out_directory,
        keep: false,
    };
    let root = text(budget, &[options.root_name, ".xml"])?;
    {
        let _path_owner = budget
            .try_reserve(
                Category::Metadata,
                (out_directory.as_os_str().len() as u64)
                    .saturating_add(options.root_name.len() as u64)
                    .saturating_add(1)
                    .saturating_mul(4),
            )
            .map_err(crate::Error::Memory)?;
        std::fs::create_dir(out_directory.join(options.root_name))?;
    }
    visit_publication_files(&dump, &root, |path, bytes| {
        check(options.cancel)?;
        let _path_owner = budget
            .try_reserve(
                Category::Metadata,
                (out_directory.as_os_str().len() as u64)
                    .saturating_add(path.len() as u64)
                    .saturating_add(1)
                    .saturating_mul(4),
            )
            .map_err(crate::Error::Memory)?;
        let target = out_directory.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        for chunk in bytes.chunks(64 << 10) {
            check(options.cancel)?;
            file.write_all(chunk)?;
        }
        file.flush()?;
        Ok(())
    })?;
    check(options.cancel)?;
    guard.keep = true;
    Ok(dump.len())
}

fn visit_publication_files(
    dump: &XmlDump,
    root: &str,
    mut write: impl FnMut(&str, &[u8]) -> Result<()>,
) -> Result<()> {
    for is_root in [false, true] {
        for (path, bytes) in dump.iter().filter(|(path, _)| (*path == root) == is_root) {
            write(path, bytes)?;
        }
    }
    Ok(())
}
