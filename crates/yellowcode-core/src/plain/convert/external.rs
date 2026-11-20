use super::{Format, Request};
use crate::Result;
use mdconf::external::{self, ExternalKind, FormRenderer};
use memory_budget::MemoryBudget;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct Options<'a> {
    pub budget: &'a MemoryBudget,
    pub cancelled: Arc<AtomicBool>,
    pub form_renderer: &'a dyn FormRenderer,
}
struct Cancel(Arc<AtomicBool>);
impl mdconf::LoadMeter for Cancel {
    fn checkpoint(&self) -> mdconf::Result<()> {
        check(&self.0)
    }
    fn admit(&self, _: &str, _: u64) -> std::result::Result<(), mdconf::MeterRefusal> {
        Ok(())
    }
}
fn check(cancel: &AtomicBool) -> mdconf::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(mdconf::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "external conversion cancelled",
        )));
    }
    Ok(())
}
fn publication<T>(result: mdconf::Result<T>, output: &std::path::Path) -> Result<T> {
    result.map_err(|error| match error {
        mdconf::Error::Io(ref io) if io.kind() == std::io::ErrorKind::AlreadyExists => {
            format!("output already exists: {}", super::display_path(output)).into()
        }
        other => Box::new(other) as crate::Error,
    })
}

pub(super) fn validate(request: &Request) -> Result<()> {
    if request.users_layout.is_some() {
        return Err("--users-layout applies only to 1CD to DT".into());
    }
    if request.without_users {
        return Err("--without-users applies only to DT to 1CD".into());
    }
    Ok(())
}

fn load_binary(
    source: &dyn mdconf::EntrySource,
    options: &Options<'_>,
) -> mdconf::Result<external::LoadedExternal> {
    let loading = mdconf::LoadOptions {
        keep_source: true,
        meter: Some(Arc::new(Cancel(options.cancelled.clone()))),
        ..Default::default()
    };
    external::load(source, &loading, options.budget)
}

fn load_xml(
    source: &std::path::Path,
    options: &Options<'_>,
) -> mdconf::Result<external::LoadedXmlExternal> {
    let loading = external::XmlLoadOptions {
        cancelled: Some(options.cancelled.clone()),
        ..Default::default()
    };
    external::load_xml(source, &loading, options.budget)
}

fn prepare(
    document: &external::ExternalDocument,
    options: &Options<'_>,
) -> mdconf::Result<external::PreparedExternal> {
    let building = external::ExternalBuildOptions {
        cancelled: Some(options.cancelled.clone()),
        ..Default::default()
    };
    external::prepare(document, &building, options.budget)
}

pub fn run(request: &Request, options: &Options<'_>) -> Result<()> {
    check(&options.cancelled)?;
    validate(request)?;
    match request.to {
        Format::Xml => {
            let source = mdconf::ManagedCfFile::open(&request.source, options.budget)?;
            let doc = load_binary(&source, options)?;
            check(&options.cancelled)?;
            let mut dumping = external::XmlDumpOptions::new(&doc.object.name);
            dumping.cancel = Some(&options.cancelled);
            dumping.form_renderer = Some(options.form_renderer);
            publication(
                external::dump_xml(&doc, &request.output, &dumping, options.budget),
                &request.output,
            )?;
        }
        Format::Epf | Format::Erf => {
            let doc = load_xml(&request.source, options)?;
            let expected = if matches!(request.to, Format::Epf) {
                ExternalKind::DataProcessor
            } else {
                ExternalKind::Report
            };
            if doc.kind != expected {
                return Err(format!(
                    "XML root {} does not match requested output {}",
                    doc.kind.xml_name(),
                    expected.xml_name(),
                )
                .into());
            }
            let prepared = prepare(&doc, options)?;
            check(&options.cancelled)?;
            publication(prepared.write(&request.output), &request.output)?;
        }
        _ => return Err("unsupported external conversion direction".into()),
    }
    Ok(())
}

pub(super) struct InterruptBridge {
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl InterruptBridge {
    pub(super) fn new(cancelled: Arc<AtomicBool>, budget: &MemoryBudget) -> Result<Self> {
        let reservation = budget.try_reserve(memory_budget::Category::Execution, 1 << 20)?;
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = done.clone();
        let thread = std::thread::Builder::new()
            .name("external-cancellation".into())
            .stack_size(256 << 10)
            .spawn(move || {
                let _reservation = reservation;
                while !worker_done.load(Ordering::Acquire) {
                    if process_containment::interrupt::requested().is_some() {
                        cancelled.store(true, Ordering::Release);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            })?;
        Ok(Self {
            done,
            thread: Some(thread),
        })
    }
}
impl Drop for InterruptBridge {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("external conversion cancelled by {}", .kind.name())]
pub(super) struct Interrupted {
    pub(super) kind: process_containment::interrupt::Kind,
    #[source]
    pub(super) source: crate::Error,
}
