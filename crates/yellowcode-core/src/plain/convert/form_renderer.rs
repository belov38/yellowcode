use crate::viewer::analysis_parent::{AnalysisError, Supervisor};
use mdconf::{
    SubObject,
    external::{ExternalDocument, FormRenderer, XmlDump, XmlDumpOptions},
    forms::Form,
};
use memory_budget::{BudgetError, BudgetedWriter, Category, Charged, MemoryBudget};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub struct SupervisedFormRenderer<'a> {
    pub executable: &'a Path,
    pub spill_dir: &'a Path,
    pub platform: &'a str,
    pub platform_resources: Option<&'a Path>,
}
fn failure(entry: &str, what: impl Into<String>) -> mdconf::Error {
    mdconf::Error::Format {
        entry: entry.into(),
        what: what.into(),
    }
}
fn memory(error: BudgetError) -> mdconf::Error {
    match error {
        BudgetError::Limit(limit) => mdconf::Error::Memory(limit),
        other => failure("external form", other.to_string()),
    }
}
fn analysis(entry: &str, error: AnalysisError) -> mdconf::Error {
    match error {
        AnalysisError::Memory(error) => memory(error),
        AnalysisError::Canceled => mdconf::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "external form conversion cancelled",
        )),
        AnalysisError::Timeout => mdconf::Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "external form conversion deadline expired",
        )),
        other => failure(entry, other.to_string()),
    }
}
fn check(cancel: Option<&AtomicBool>) -> mdconf::Result<()> {
    if cancel.is_some_and(|value| value.load(Ordering::Relaxed)) {
        return Err(mdconf::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "external form conversion cancelled",
        )));
    }
    Ok(())
}
impl FormRenderer for SupervisedFormRenderer<'_> {
    fn render(
        &self,
        document: &ExternalDocument,
        sub: &SubObject,
        options: &XmlDumpOptions<'_>,
        budget: &MemoryBudget,
    ) -> mdconf::Result<XmlDump> {
        check(options.cancel)?;
        let form = sub
            .form
            .as_ref()
            .ok_or_else(|| failure(&sub.name, "missing form content"))?;
        let facts = facts(document, form, options.cancel, budget)?;
        let source = form
            .source
            .as_ref()
            .ok_or_else(|| failure(&sub.name, "missing form source"))?;
        let serialized;
        let source = if let Some(bytes) = source.bytes() {
            bytes
        } else {
            let Some(mdconf::EntryData::Brace(doc)) = source.entry() else {
                return Err(failure(&sub.name, "form source is not brace text"));
            };
            serialized = brace::write_node_budgeted(&doc.root, budget, Category::Response)
                .map_err(memory)?;
            &serialized
        };
        let mut owner = BudgetedWriter::new(budget, 64 << 10).map_err(memory)?;
        for part in [document.kind.xml_name(), ".", document.object.name.as_str()] {
            owner.try_write(part.as_bytes()).map_err(memory)?;
        }
        let owner = std::str::from_utf8(owner.as_slice())
            .map_err(|_| failure(&sub.name, "invalid form owner"))?;
        #[derive(serde::Serialize)]
        struct Metadata<'a> {
            op: &'static str,
            platform: &'a str,
            snapshot: u64,
            form_ordinal: u64,
            owner: &'a str,
            shared: bool,
        }
        let metadata = Metadata {
            op: "form_xml",
            platform: self.platform,
            snapshot: 0,
            form_ordinal: 0,
            owner,
            shared: form.shared.is_some(),
        };
        let not_cancelled = AtomicBool::new(false);
        let supervisor = Supervisor::new(
            self.executable,
            self.spill_dir,
            budget,
            options.cancel.unwrap_or(&not_cancelled),
        )
        .with_platform_resources(self.platform_resources);
        let lease = supervisor
            .form_xml(&metadata, source, &facts)
            .map_err(|error| analysis(&sub.name, error))?;
        check(options.cancel)?;
        let projection = lease
            .view_checked(|| {
                if options
                    .cancel
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
                {
                    return Err(AnalysisError::Canceled);
                }
                Ok(())
            })
            .map_err(|error| analysis(&sub.name, error))?;
        let present = match projection.outcome {
            mdconf::forms::wire::Outcome::Present(value) if !value.deferred => value,
            mdconf::forms::wire::Outcome::Present(_) => {
                return Err(failure(&sub.name, "form renderer deferred metadata"));
            }
            mdconf::forms::wire::Outcome::Refused(reason) => {
                return Err(failure(
                    &sub.name,
                    format!(
                        "form conversion refused: {}",
                        reason.unwrap_or("unsupported form")
                    ),
                ));
            }
            mdconf::forms::wire::Outcome::Absent => {
                return Err(failure(&sub.name, "form XML is absent"));
            }
        };
        let mut dump = XmlDump::new(budget, options.max_output_bytes)?;
        dump.put("Form.xml", present.xml.as_bytes())?;
        for (name, bytes) in present.pictures() {
            check(options.cancel)?;
            let mut path = BudgetedWriter::new(budget, options.max_output_bytes).map_err(memory)?;
            path.try_write(b"Form/").map_err(memory)?;
            path.try_write(name.as_bytes()).map_err(memory)?;
            let path = std::str::from_utf8(path.as_slice())
                .map_err(|_| failure(&sub.name, "invalid picture path"))?;
            dump.put(path, bytes)?;
        }
        check(options.cancel)?;
        Ok(dump)
    }
}
struct Encoder<'a> {
    writer: BudgetedWriter,
    cancel: Option<&'a AtomicBool>,
}
impl Encoder<'_> {
    fn raw(&mut self, bytes: &[u8]) -> mdconf::Result<()> {
        check(self.cancel)?;
        for chunk in bytes.chunks(64 << 10) {
            check(self.cancel)?;
            self.writer.try_write(chunk).map_err(memory)?;
        }
        Ok(())
    }
    fn number(&mut self, value: usize) -> mdconf::Result<()> {
        self.raw(
            &u64::try_from(value)
                .map_err(|_| failure("form facts", "size overflow"))?
                .to_le_bytes(),
        )
    }
    fn blob(&mut self, bytes: &[u8]) -> mdconf::Result<()> {
        self.number(bytes.len())?;
        self.raw(bytes)
    }
    fn guid(&mut self, id: brace::Guid) -> mdconf::Result<()> {
        self.raw(&id.to_bytes_le())
    }
}
fn facts(
    document: &ExternalDocument,
    form: &Form,
    cancel: Option<&AtomicBool>,
    budget: &MemoryBudget,
) -> mdconf::Result<Charged<Vec<u8>>> {
    let shared = mdconf::forms::shared_facts(form);
    if form.shared.is_some() && shared.is_none() {
        return Err(failure("form facts", "unrecognized shared form state"));
    }
    let mut out = Encoder {
        writer: BudgetedWriter::new(
            budget,
            memory_budget::sealed_heap::ANALYSIS_HEAP_BYTES as u64,
        )
        .map_err(memory)?,
        cancel,
    };
    let v85 = shared.is_some_and(|value| value.v85());
    let compat = match (document.compat, v85) {
        (Some(80312), false) => 1,
        (Some(80501), false) => 2,
        _ => 0,
    };
    out.raw(b"YCF1")?;
    out.raw(&[u8::from(v85), compat, 0, 0])?;
    out.number(document.names().count())?;
    for (id, name) in document.names() {
        out.guid(id)?;
        out.blob(name.as_bytes())?;
    }
    out.number(document.type_names().count())?;
    for (id, name) in document.type_names() {
        out.guid(id)?;
        out.blob(name.as_bytes())?;
    }
    out.number(0)?;
    out.number(document.index.sub_hdr.len())?;
    for (id, path) in &document.index.sub_hdr {
        out.guid(*id)?;
        out.number(path.len())?;
        for at in path {
            out.number(*at)?;
        }
    }
    let order = shared.map(|value| value.names_order()).unwrap_or_default();
    out.number(order.len())?;
    for id in order {
        out.guid(*id)?;
    }
    out.number(shared.map_or(0, |value| value.child_patterns().count()))?;
    if let Some(shared) = shared {
        for (id, pattern) in shared.child_patterns() {
            check(cancel)?;
            out.guid(*id)?;
            let bytes =
                brace::write_node_budgeted(pattern, budget, Category::Response).map_err(memory)?;
            out.blob(&bytes)?;
        }
    }
    check(cancel)?;
    Ok(out.writer.into_buffer().into_charged_vec())
}
