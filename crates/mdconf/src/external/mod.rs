use crate::{EntrySource, LoadOptions, Result, Value};
pub use mdconf_core::external::*;
mod xml_build;
use memory_budget::MemoryBudget;
use std::sync::Arc;
pub use xml_build::{build, load_xml, prepare, write};

pub fn load(
    source: &dyn EntrySource,
    options: &LoadOptions,
    budget: &MemoryBudget,
) -> Result<LoadedExternal> {
    load_with(source, options, budget, &FullContent { budget })
}
struct FullContent<'a> {
    budget: &'a MemoryBudget,
}
fn allocation_error(error: memory_budget::BudgetError) -> crate::Error {
    match error {
        memory_budget::BudgetError::Limit(limit) => crate::Error::Memory(limit),
        other => crate::Error::Io(std::io::Error::new(std::io::ErrorKind::OutOfMemory, other)),
    }
}
impl ContentCodecs for FullContent<'_> {
    fn load(
        &self,
        _: &mdconf_core::codec::LoadCtx<'_>,
        _: &brace::Node,
        _: &mut ExternalDocument,
    ) -> Result<()> {
        Err(failure("external content", "owned conversion is required"))
    }
    fn load_owned(
        &self,
        context: &mdconf_core::codec::LoadCtx<'_>,
        tree: &brace::Node,
        document: memory_budget::Charged<ExternalDocument>,
    ) -> Result<memory_budget::Charged<ExternalDocument>> {
        let mut converted = None;
        let document = document.map(|mut document| {
            converted = Some(self.prepare(context, tree, &mut document));
            document
        });
        let templates =
            converted.ok_or_else(|| failure("external content", "missing conversion result"))??;
        if templates.len() != document.object.templates.len() {
            return Err(failure("external content", "template count changed"));
        }
        let document = document.try_zip(templates).map_err(allocation_error)?;
        Ok(document.map(|(mut document, templates)| {
            for (sub, template) in document.object.templates.iter_mut().zip(templates) {
                sub.template = Some(template);
            }
            clear_nondumped_diagnostics(context, tree, &mut document);
            document
        }))
    }
}
impl FullContent<'_> {
    fn prepare(
        &self,
        context: &mdconf_core::codec::LoadCtx<'_>,
        tree: &brace::Node,
        document: &mut ExternalDocument,
    ) -> Result<memory_budget::Charged<Vec<crate::templates::Template>>> {
        use mdconf_core::codec::base::allocation::{Allocation, Owned};
        if !document.object.forms.is_empty() {
            let shared = Arc::new(md_forms::forms::shared::external(context, tree, document)?);
            for form in &mut document.object.forms {
                form.form = Some(md_forms::forms::read_external(context, form.id, &shared)?);
            }
        }
        let allocation = Allocation::Managed(self.budget);
        let mut templates = allocation
            .vector(document.object.templates.len())
            .map_err(allocation_error)?
            .finish();
        for sub in &document.object.templates {
            if let Some(meter) = &context.opts.meter {
                meter.checkpoint()?;
            }
            let name = format!("{}.0", sub.id);
            let kind = sub
                .props
                .iter()
                .find(|p| p.name == "TemplateType")
                .and_then(|p| match &p.value {
                    Value::Text(n) => crate::templates::TemplateType::from_name(n),
                    _ => None,
                })
                .ok_or_else(|| failure(&name, "unsupported template kind"))?;
            let template = if !context.entries.contains(&name)
                && kind == crate::templates::TemplateType::SpreadsheetDocument
            {
                allocation
                    .vector::<(String, Vec<u8>)>(0)
                    .map_err(allocation_error)?
                    .finish()
                    .map(|files| crate::templates::Template {
                        kind: Some(kind),
                        files,
                    })
                    .managed()
                    .map_err(allocation_error)?
            } else {
                let data = context
                    .entries
                    .get(&name)?
                    .ok_or_else(|| crate::Error::MissingEntry(name.clone()))?;
                let raw = context
                    .entries
                    .raw(&name)?
                    .ok_or_else(|| crate::Error::MissingEntry(name.clone()))?;
                md_graphics::templates::read_external_owned(
                    document,
                    &data,
                    &raw,
                    kind,
                    self.budget,
                )?
            };
            if let Some(meter) = &context.opts.meter {
                meter.checkpoint()?;
            }
            templates = templates
                .push(Owned::Managed(template))
                .map_err(allocation_error)?;
        }
        templates.managed().map_err(allocation_error)
    }
}

fn clear_nondumped_diagnostics(
    context: &mdconf_core::codec::LoadCtx<'_>,
    tree: &brace::Node,
    document: &mut ExternalDocument,
) {
    if document.object.forms.iter().all(|form| form.form.is_some()) {
        for key in [
            "unknown:FormProperties.ExtendedConfigurationObject",
            "unknown:FormProperties.Form",
            "unknown:FormProperties.Help",
            "unknown:FormProperties.ObjectBelonging",
        ] {
            document.diagnostics.misses.remove(key);
        }
    }
    if document
        .object
        .templates
        .iter()
        .all(|template| template.template.is_some())
    {
        for key in [
            "unknown:TemplateProperties.ExtendedConfigurationObject",
            "unknown:TemplateProperties.ObjectBelonging",
            "unknown:TemplateProperties.Template",
        ] {
            document.diagnostics.misses.remove(key);
        }
    }
    if document.kind == ExternalKind::Report {
        let sections_are_default = context
            .schema
            .kind(document.kind.base_kind().as_str())
            .is_some_and(|spec| {
                spec.children
                    .iter()
                    .filter(|(_, child)| child.tag == "TabularSection")
                    .all(|(class, _)| {
                        mdconf_core::codec::base::objects::path::find_collection(tree, class)
                            .unwrap_or_default()
                            .iter()
                            .all(|section| {
                                section
                                    .at(0)
                                    .and_then(brace::Node::as_list)
                                    .is_some_and(|metadata| metadata.len() == 2)
                                    && section
                                        .get(&[0, 1])
                                        .and_then(brace::Node::as_list)
                                        .is_some_and(|body| {
                                            body.len() == 9
                                                && body.first().and_then(brace::Node::as_atom)
                                                    == Some("11")
                                        })
                            })
                    })
            });
        if sections_are_default {
            for key in [
                "unknown:TabularSectionProperties.ExtendedConfigurationObject",
                "unknown:TabularSectionProperties.LineNumberLength",
                "unknown:TabularSectionProperties.ObjectBelonging",
                "unknown:TabularSectionProperties.Use",
            ] {
                document.diagnostics.misses.remove(key);
            }
        }
    }
}
fn failure(entry: &str, what: &str) -> crate::Error {
    crate::Error::Format {
        entry: entry.into(),
        what: what.into(),
    }
}

pub mod xml_out;
pub use xml_out::{FormRenderer, XmlDump, XmlDumpOptions, dump_xml, dump_xml_to_memory};
