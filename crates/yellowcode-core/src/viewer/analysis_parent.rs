mod assets;
pub use assets::PictureAssets;
mod process;
use super::{analysis_worker, body::ViewerBody};
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, Write},
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const MAX_METADATA: u64 = 64 << 10;
const MAX_OUTPUT: u64 = memory_budget::sealed_heap::ANALYSIS_HEAP_BYTES as u64;
const CONTROL_BYTES: u64 = 1 << 20;
const MAX_PATH_BYTES: usize = 32 << 10;
const DEADLINE: Duration = Duration::from_secs(120);
#[derive(Clone, Copy)]
enum OutputKind {
    Json,
    FormXml,
    PreparedPreview,
    LayoutPreview,
    PictureResource,
    RenderedPreview,
}

impl OutputKind {
    fn maximum(self) -> u64 {
        match self {
            Self::PictureResource => (ui_style::package::MAX_FILE as u64) + 14,
            _ => MAX_OUTPUT,
        }
    }
}

#[derive(Clone, Copy)]
enum Payload<'a> {
    Raw(&'a [u8]),
    Preview(form_model::preview_layout::Input<'a>),
}
impl Payload<'_> {
    fn len(self) -> Result<usize, AnalysisError> {
        match self {
            Self::Raw(bytes) => Ok(bytes.len()),
            Self::Preview(input) => 28usize
                .checked_add(input.source.len())
                .and_then(|n| n.checked_add(input.requests.len()))
                .and_then(|n| n.checked_add(input.answers.len()))
                .ok_or(AnalysisError::Resource),
        }
    }
}
#[derive(Clone, Copy)]
enum Facts<'a> {
    Raw(&'a [u8]),
    Assets(&'a PictureAssets),
}
impl Facts<'_> {
    fn len(self, check: impl FnMut() -> Result<(), AnalysisError>) -> Result<usize, AnalysisError> {
        match self {
            Self::Raw(bytes) => Ok(bytes.len()),
            Self::Assets(assets) => assets.encoded_len(check),
        }
    }
}
fn preview_encoding(error: form_model::preview_layout::WriteError<AnalysisError>) -> AnalysisError {
    match error {
        form_model::preview_layout::WriteError::Sink(error) => error,
        form_model::preview_layout::WriteError::Overflow => AnalysisError::Resource,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("{0}")]
    Memory(#[from] BudgetError),
    #[error("native analysis exhausted its admitted heap")]
    Resource,
    #[error("native analysis IO failed")]
    Io,
    #[error("native analysis protocol failed")]
    Protocol,
    #[error("native analysis worker failed")]
    Failed,
    #[error("native analysis canceled")]
    Canceled,
    #[error("native analysis deadline expired")]
    Timeout,
}

impl From<mdconf::forms::wire::ProtocolError> for AnalysisError {
    fn from(_: mdconf::forms::wire::ProtocolError) -> Self {
        Self::Protocol
    }
}

#[derive(Debug)]
pub struct FormXmlLease {
    body: ViewerBody,
}
impl FormXmlLease {
    pub fn view(&self) -> Result<mdconf::forms::wire::Projection<'_>, AnalysisError> {
        mdconf::forms::wire::decode_result(self.body.as_ref()).map_err(|_| AnalysisError::Protocol)
    }
}

#[derive(Debug)]
pub struct PreparedPreviewLease {
    body: ViewerBody,
}
impl PreparedPreviewLease {
    pub(crate) fn bytes(&self) -> &[u8] {
        self.body.as_ref()
    }
    pub fn view(&self) -> Result<form_model::preview_wire::Prepared<'_>, AnalysisError> {
        form_model::preview_wire::decode_result(self.body.as_ref())
            .map_err(|_| AnalysisError::Protocol)
    }
}

#[derive(Debug)]
pub struct LayoutPreviewLease {
    body: ViewerBody,
}
impl LayoutPreviewLease {
    pub fn view(&self) -> Result<form_model::preview_layout::Outcome<'_>, AnalysisError> {
        form_model::preview_layout::decode_result_checked(self.body.as_ref(), || {
            Ok::<_, AnalysisError>(())
        })
        .map_err(|_| AnalysisError::Protocol)
    }
}

#[derive(Debug)]
pub struct PictureLease {
    body: ViewerBody,
}
impl PictureLease {
    pub fn view(&self) -> Result<Option<form_model::preview_picture::Image<'_>>, AnalysisError> {
        form_model::preview_picture::decode_checked(
            self.body.as_ref(),
            ui_style::package::MAX_FILE,
            || Ok::<_, AnalysisError>(()),
        )
        .map_err(|_| AnalysisError::Protocol)
    }
}

#[derive(Debug)]
pub struct RenderedPreviewLease {
    body: ViewerBody,
}
impl RenderedPreviewLease {
    pub fn view(&self) -> Result<form_model::preview_render::Outcome<'_>, AnalysisError> {
        form_model::preview_render::decode_result_checked(
            self.body.as_ref(),
            MAX_OUTPUT as usize,
            || Ok::<_, AnalysisError>(()),
        )
        .map_err(|_| AnalysisError::Protocol)
    }
    pub fn into_body_checked(
        self,
        mut check: impl FnMut() -> Result<(), AnalysisError>,
    ) -> Result<ViewerBody, AnalysisError> {
        use form_model::preview_render::Outcome;
        let size = match form_model::preview_render::decode_result_checked(
            self.body.as_ref(),
            MAX_OUTPUT as usize,
            &mut check,
        )
        .map_err(|error| match error {
            form_model::preview_render::DecodeError::Protocol => AnalysisError::Protocol,
            form_model::preview_render::DecodeError::Check(error) => error,
        })? {
            Outcome::Body(text)
            | Outcome::ModelError(text)
            | Outcome::AutoformError(text)
            | Outcome::LayoutError(text) => text.len(),
        };
        let ViewerBody::Owned(mut bytes) = self.body else {
            return Err(AnalysisError::Protocol);
        };
        for start in (0..size).step_by(64 << 10) {
            check()?;
            let end = start.saturating_add(64 << 10).min(size);
            bytes
                .as_mut_slice()
                .copy_within(start + 13..end + 13, start);
        }
        bytes.truncate(size);
        check()?;
        Ok(ViewerBody::Owned(bytes))
    }
}

pub struct Supervisor<'a> {
    executable: &'a Path,
    spill_dir: &'a Path,
    budget: &'a MemoryBudget,
    canceled: &'a AtomicBool,
    timeout: Duration,
    absolute_deadline: Option<Instant>,
    json_limit: u64,
    picture_answer: bool,
    interrupted: Option<&'a dyn Fn() -> bool>,
}
impl<'a> Supervisor<'a> {
    pub fn new(
        executable: &'a Path,
        spill_dir: &'a Path,
        budget: &'a MemoryBudget,
        canceled: &'a AtomicBool,
    ) -> Self {
        Self {
            executable,
            spill_dir,
            budget,
            canceled,
            timeout: DEADLINE,
            absolute_deadline: None,
            json_limit: MAX_OUTPUT,
            picture_answer: false,
            interrupted: None,
        }
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.min(DEADLINE);
        self
    }
    pub(crate) fn with_json_limit(mut self, limit: u64) -> Self {
        self.json_limit = limit.min(MAX_OUTPUT);
        self
    }
    pub(crate) fn for_picture_answer(&self) -> Self {
        Self {
            executable: self.executable,
            spill_dir: self.spill_dir,
            budget: self.budget,
            canceled: self.canceled,
            timeout: self.timeout,
            absolute_deadline: self.absolute_deadline,
            json_limit: self.json_limit,
            picture_answer: true,
            interrupted: self.interrupted,
        }
    }
    fn check_json_size(&self, kind: OutputKind, length: u64) -> Result<(), AnalysisError> {
        if length > kind.maximum() + 8 {
            return Err(AnalysisError::Protocol);
        }
        let framing = match kind {
            OutputKind::Json => 0,
            OutputKind::PictureResource if self.picture_answer => 14,
            OutputKind::RenderedPreview => 13,
            _ => return Ok(()),
        };
        if length.saturating_sub(8 + framing) > self.json_limit {
            return Err(BudgetError::Limit(memory_budget::MemoryLimit {
                category: Category::Response,
                requested: length.saturating_sub(8 + framing),
                reason: memory_budget::LimitReason::Category,
            })
            .into());
        }
        Ok(())
    }
    pub(crate) fn with_control(
        mut self,
        deadline: Option<Instant>,
        interrupted: &'a dyn Fn() -> bool,
    ) -> Self {
        self.absolute_deadline = deadline;
        self.interrupted = Some(interrupted);
        self
    }
    pub(crate) fn check(&self, deadline: Instant) -> Result<(), AnalysisError> {
        let deadline = self
            .absolute_deadline
            .map_or(deadline, |absolute| absolute.min(deadline));
        if self.canceled.load(Ordering::Acquire) || self.interrupted.is_some_and(|check| check()) {
            return Err(AnalysisError::Canceled);
        }
        if Instant::now() >= deadline {
            return Err(AnalysisError::Timeout);
        }
        Ok(())
    }
    pub fn run<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        source: &[u8],
    ) -> Result<ViewerBody, AnalysisError> {
        self.run_before(metadata, source, Instant::now() + self.timeout)
    }
    pub(crate) fn run_before<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        source: &[u8],
        deadline: Instant,
    ) -> Result<ViewerBody, AnalysisError> {
        self.run_payloads(
            metadata,
            Payload::Raw(source),
            None,
            deadline,
            OutputKind::Json,
        )
    }
    pub fn prepare_preview<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        source: &[u8],
    ) -> Result<PreparedPreviewLease, AnalysisError> {
        self.prepare_preview_before(metadata, source, Instant::now() + self.timeout)
    }
    pub(crate) fn prepare_preview_before<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        source: &[u8],
        deadline: Instant,
    ) -> Result<PreparedPreviewLease, AnalysisError> {
        let body = self.run_payloads(
            metadata,
            Payload::Raw(source),
            None,
            deadline,
            OutputKind::PreparedPreview,
        )?;
        Ok(PreparedPreviewLease { body })
    }
    pub fn form_xml<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        source: &[u8],
        facts: &[u8],
    ) -> Result<FormXmlLease, AnalysisError> {
        self.form_xml_before(metadata, source, facts, Instant::now() + self.timeout)
    }
    pub(crate) fn form_xml_before<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        source: &[u8],
        facts: &[u8],
        deadline: Instant,
    ) -> Result<FormXmlLease, AnalysisError> {
        let body = self.run_payloads(
            metadata,
            Payload::Raw(source),
            Some(Facts::Raw(facts)),
            deadline,
            OutputKind::FormXml,
        )?;
        Ok(FormXmlLease { body })
    }
    pub fn layout_preview<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        input: form_model::preview_layout::Input<'_>,
    ) -> Result<LayoutPreviewLease, AnalysisError> {
        self.layout_preview_before(metadata, input, Instant::now() + self.timeout)
    }
    pub(crate) fn layout_preview_before<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        input: form_model::preview_layout::Input<'_>,
        deadline: Instant,
    ) -> Result<LayoutPreviewLease, AnalysisError> {
        let body = self.run_payloads(
            metadata,
            Payload::Preview(input),
            None,
            deadline,
            OutputKind::LayoutPreview,
        )?;
        Ok(LayoutPreviewLease { body })
    }
    pub fn picture_resource<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        source: &[u8],
    ) -> Result<PictureLease, AnalysisError> {
        self.picture_resource_before(metadata, source, Instant::now() + self.timeout)
    }
    pub(crate) fn picture_resource_before<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        source: &[u8],
        deadline: Instant,
    ) -> Result<PictureLease, AnalysisError> {
        let body = self.run_payloads(
            metadata,
            Payload::Raw(source),
            None,
            deadline,
            OutputKind::PictureResource,
        )?;
        Ok(PictureLease { body })
    }
    pub fn render_preview<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        input: form_model::preview_layout::Input<'_>,
        assets: &PictureAssets,
    ) -> Result<RenderedPreviewLease, AnalysisError> {
        self.render_preview_before(metadata, input, assets, Instant::now() + self.timeout)
    }
    pub(crate) fn render_preview_before<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        input: form_model::preview_layout::Input<'_>,
        assets: &PictureAssets,
        deadline: Instant,
    ) -> Result<RenderedPreviewLease, AnalysisError> {
        let body = self.run_payloads(
            metadata,
            Payload::Preview(input),
            Some(Facts::Assets(assets)),
            deadline,
            OutputKind::RenderedPreview,
        )?;
        Ok(RenderedPreviewLease { body })
    }
    fn run_payloads<T: Serialize + ?Sized>(
        &self,
        metadata: &T,
        source: Payload<'_>,
        facts: Option<Facts<'_>>,
        deadline: Instant,
        kind: OutputKind,
    ) -> Result<ViewerBody, AnalysisError> {
        let deadline = deadline.min(Instant::now() + self.timeout);
        self.check(deadline)?;
        let source_length = source.len()?;
        let facts_length = facts
            .map(|facts| facts.len(|| self.check(deadline)))
            .transpose()?
            .unwrap_or(0);
        let _worker = self
            .budget
            .try_reserve(
                Category::Execution,
                memory_budget::sealed_heap::ANALYSIS_ADMISSION_BYTES,
            )
            .map_err(BudgetError::from)?;
        let _control = self
            .budget
            .try_reserve(Category::Execution, CONTROL_BYTES)
            .map_err(BudgetError::from)?;
        if source_length
            .checked_add(facts_length)
            .is_none_or(|length| length as u64 > MAX_OUTPUT)
            || self.executable.as_os_str().len() > MAX_PATH_BYTES
            || self.spill_dir.as_os_str().len() > MAX_PATH_BYTES
        {
            return Err(AnalysisError::Resource);
        }
        if !self.executable.is_absolute() || !self.spill_dir.is_absolute() {
            return Err(AnalysisError::Io);
        }
        let metadata = super::body::json(self.budget, MAX_METADATA, metadata)?;
        let mut input = tempfile::tempfile_in(self.spill_dir).map_err(|_| AnalysisError::Io)?;
        let mut output = tempfile::tempfile_in(self.spill_dir).map_err(|_| AnalysisError::Io)?;
        self.write(
            &mut input,
            &(metadata.as_ref().len() as u64).to_le_bytes(),
            deadline,
        )?;
        self.write(&mut input, metadata.as_ref(), deadline)?;
        self.write(&mut input, &(source_length as u64).to_le_bytes(), deadline)?;
        match source {
            Payload::Raw(bytes) => self.write(&mut input, bytes, deadline)?,
            Payload::Preview(parts) => form_model::preview_layout::encode_input(parts, |bytes| {
                self.write(&mut input, bytes, deadline)
            })
            .map_err(preview_encoding)?,
        }
        if let Some(facts) = facts {
            self.write(&mut input, &(facts_length as u64).to_le_bytes(), deadline)?;
            match facts {
                Facts::Raw(bytes) => self.write(&mut input, bytes, deadline)?,
                Facts::Assets(assets) => {
                    assets.encode(|bytes| self.write(&mut input, bytes, deadline))?
                }
            }
        }
        input.rewind().map_err(|_| AnalysisError::Io)?;
        self.check(deadline)?;
        let mut command = Command::new(self.executable);
        command.env_clear();
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            if root.len() > MAX_PATH_BYTES {
                return Err(AnalysisError::Resource);
            }
            command.env("SystemRoot", root);
        }
        command
            .arg(analysis_worker::INTERNAL_SWITCH)
            .stdin(input)
            .stdout(output.try_clone().map_err(|_| AnalysisError::Io)?)
            .stderr(Stdio::null());
        let mut child = process::OwnedChild::spawn(&mut command)?;
        loop {
            self.check(deadline)?;
            let length = output.metadata().map_err(|_| AnalysisError::Io)?.len();
            self.check_json_size(kind, length)?;
            if child.exited()? {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let status = child.finish()?;
        drop(_worker);
        match status.code() {
            Some(0) => {}
            Some(72) => return Err(AnalysisError::Resource),
            Some(75 | 76) => return Err(AnalysisError::Io),
            Some(74) => return Err(AnalysisError::Protocol),
            _ => return Err(AnalysisError::Failed),
        }
        self.check(deadline)?;
        self.check_json_size(
            kind,
            output.metadata().map_err(|_| AnalysisError::Io)?.len(),
        )?;
        match kind {
            OutputKind::Json => read_body(&mut output, self.budget, || self.check(deadline)),
            kind => read_body_kind(&mut output, self.budget, || self.check(deadline), kind),
        }
    }
    fn write(&self, file: &mut File, bytes: &[u8], deadline: Instant) -> Result<(), AnalysisError> {
        for chunk in bytes.chunks(64 << 10) {
            self.check(deadline)?;
            file.write_all(chunk).map_err(|_| AnalysisError::Io)?;
        }
        Ok(())
    }
}
fn read_body(
    file: &mut File,
    budget: &MemoryBudget,
    check: impl Fn() -> Result<(), AnalysisError>,
) -> Result<ViewerBody, AnalysisError> {
    read_body_kind(file, budget, check, OutputKind::Json)
}
fn read_body_kind(
    file: &mut File,
    budget: &MemoryBudget,
    check: impl Fn() -> Result<(), AnalysisError>,
    kind: OutputKind,
) -> Result<ViewerBody, AnalysisError> {
    let length = file.metadata().map_err(|_| AnalysisError::Io)?.len();
    file.rewind().map_err(|_| AnalysisError::Io)?;
    let mut header = [0u8; 8];
    file.read_exact(&mut header)
        .map_err(|_| AnalysisError::Protocol)?;
    let declared = u64::from_le_bytes(header);
    if declared > kind.maximum() || declared.checked_add(8) != Some(length) {
        return Err(AnalysisError::Protocol);
    }
    let declared = usize::try_from(declared).map_err(|_| AnalysisError::Protocol)?;
    let mut bytes = BudgetedVec::try_with_capacity(budget, Category::Response, declared)?;
    bytes.try_resize(declared, 0)?;
    for chunk in bytes.as_mut_slice().chunks_mut(64 << 10) {
        check()?;
        file.read_exact(chunk).map_err(|_| AnalysisError::Io)?;
    }
    check()?;
    match kind {
        OutputKind::RenderedPreview => {
            form_model::preview_render::decode_result_checked(
                bytes.as_slice(),
                MAX_OUTPUT as usize,
                &check,
            )
            .map_err(|error| match error {
                form_model::preview_render::DecodeError::Protocol => AnalysisError::Protocol,
                form_model::preview_render::DecodeError::Check(error) => error,
            })?;
            check()?;
            return Ok(ViewerBody::Owned(bytes));
        }
        OutputKind::FormXml => {
            mdconf::forms::wire::decode_result_checked(bytes.as_slice(), &check)?;
            check()?;
            return Ok(ViewerBody::Owned(bytes));
        }
        OutputKind::PreparedPreview => {
            form_model::preview_wire::decode_result_checked(bytes.as_slice(), &check).map_err(
                |error| match error {
                    form_model::preview_wire::DecodeError::Protocol => AnalysisError::Protocol,
                    form_model::preview_wire::DecodeError::Check(error) => error,
                },
            )?;
            check()?;
            return Ok(ViewerBody::Owned(bytes));
        }
        OutputKind::LayoutPreview => {
            form_model::preview_layout::decode_result_checked(bytes.as_slice(), &check).map_err(
                |error| match error {
                    form_model::preview_layout::DecodeError::Protocol => AnalysisError::Protocol,
                    form_model::preview_layout::DecodeError::Check(error) => error,
                },
            )?;
            check()?;
            return Ok(ViewerBody::Owned(bytes));
        }
        OutputKind::PictureResource => {
            form_model::preview_picture::decode_checked(
                bytes.as_slice(),
                ui_style::package::MAX_FILE,
                &check,
            )
            .map_err(|error| match error {
                form_model::preview_picture::DecodeError::Protocol => AnalysisError::Protocol,
                form_model::preview_picture::DecodeError::Check(error) => error,
            })?;
            check()?;
            return Ok(ViewerBody::Owned(bytes));
        }
        OutputKind::Json => {}
    }
    let mut at = 0;
    while at < bytes.len() {
        check()?;
        let end = (at + (64 << 10)).min(bytes.len());
        let chunk = bytes
            .as_slice()
            .get(at..end)
            .ok_or(AnalysisError::Protocol)?;
        match std::str::from_utf8(chunk) {
            Ok(_) => at = end,
            Err(error) if error.error_len().is_none() && end < bytes.len() => {
                at += error.valid_up_to();
            }
            Err(_) => return Err(AnalysisError::Protocol),
        }
    }
    let mut depth = 0usize;
    let mut string = false;
    let mut escaped = false;
    for chunk in bytes.as_slice().chunks(64 << 10) {
        check()?;
        for &byte in chunk {
            if string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    string = false;
                }
            } else {
                match byte {
                    b'"' => string = true,
                    b'[' | b'{' => {
                        depth += 1;
                        if depth > 128 {
                            return Err(AnalysisError::Protocol);
                        }
                    }
                    b']' | b'}' => depth = depth.checked_sub(1).ok_or(AnalysisError::Protocol)?,
                    _ => {}
                }
            }
        }
    }
    let mut reader = ValidationReader {
        bytes: bytes.as_slice(),
        check: &check,
        until_check: 0,
        failure: None,
    };
    let valid = {
        let mut json = serde_json::Deserializer::from_reader(&mut reader);
        serde::de::IgnoredAny::deserialize(&mut json).and_then(|_| json.end())
    };
    if let Some(error) = reader.failure {
        return Err(error);
    }
    valid.map_err(|_| AnalysisError::Protocol)?;
    check()?;
    Ok(ViewerBody::Owned(bytes))
}

struct ValidationReader<'a, F> {
    bytes: &'a [u8],
    check: F,
    until_check: usize,
    failure: Option<AnalysisError>,
}
impl<F: Fn() -> Result<(), AnalysisError>> Read for ValidationReader<'_, F> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.until_check == 0 {
            if let Err(error) = (self.check)() {
                self.failure = Some(error);
                return Err(std::io::ErrorKind::Other.into());
            }
            self.until_check = 64 << 10;
        }
        let count = out.len().min(self.bytes.len()).min(self.until_check);
        let (head, tail) = self.bytes.split_at(count);
        if let Some(output) = out.get_mut(..count) {
            output.copy_from_slice(head);
        }
        self.bytes = tail;
        self.until_check -= count;
        Ok(count)
    }
}
