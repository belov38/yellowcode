use super::names_metadata::NamesCancellation;
use super::{ManagedNamesMetadataError, ManagedSnapshot, SelectedFormError};
use crate::viewer::analysis_parent::AnalysisError;
use mdconf::{ManagedEntrySource, startup::ViewerDataIdentity};
use memory_budget::{BudgetError, Category, Reservation};
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const CONTROL_BYTES: u64 = 8 << 20;
const MAX_PATH_BYTES: usize = 32 << 10;
const MAX_COMPONENTS: usize = 128;
const DOCUMENT_LIMIT: u64 = 256 << 20;
const DONE: &str = ".onec-done";

pub struct ManagedNamesDirectory {
    directory: tempfile::TempDir,
    journal: Journal,
    identity: ViewerDataIdentity,
    _control: Reservation,
}
impl ManagedNamesDirectory {
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
    pub fn data_identity(&self) -> ViewerDataIdentity {
        self.identity
    }
    pub fn warnings(&self) -> &'static [&'static str] {
        &[]
    }
}

impl Drop for ManagedNamesDirectory {
    fn drop(&mut self) {
        let _ = self.journal.cleanup(self.directory.path());
    }
}

const FILE_RECORD: u8 = 1;
const DIRECTORY_RECORD: u8 = 2;
const RECORD_OVERHEAD: usize = 9;
struct Journal {
    file: tempfile::NamedTempFile,
    committed: u64,
    failed: bool,
}
impl Journal {
    fn new(spill: &Path) -> io::Result<Self> {
        Ok(Self {
            file: tempfile::Builder::new()
                .prefix("names-journal-")
                .tempfile_in(spill)?,
            committed: 0,
            failed: false,
        })
    }
    fn append(&mut self, tag: u8, key: &str) -> io::Result<()> {
        if self.failed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let result = append_record(self.file.as_file_mut(), &mut self.committed, tag, key);
        self.failed = result.is_err();
        result
    }
    fn cleanup(&mut self, root: &Path) -> io::Result<()> {
        replay(root, self.file.as_file_mut(), self.committed)
    }
}
fn plain_key(key: &str) -> bool {
    !key.is_empty()
        && key.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':', '\0'])
        })
}
fn path_fits(root: &Path, key: &str) -> bool {
    key.split('/').count() <= MAX_COMPONENTS
        && root
            .as_os_str()
            .as_encoded_bytes()
            .len()
            .checked_add(key.len())
            .and_then(|n| n.checked_add(1))
            .is_some_and(|n| n <= MAX_PATH_BYTES)
}
fn append_record(out: &mut impl Write, committed: &mut u64, tag: u8, key: &str) -> io::Result<()> {
    if !matches!(tag, FILE_RECORD | DIRECTORY_RECORD)
        || !plain_key(key)
        || key.len() > MAX_PATH_BYTES
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let length = u32::try_from(key.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let record = length
        .checked_add(RECORD_OVERHEAD as u32)
        .ok_or(io::ErrorKind::InvalidInput)?;
    let end = committed
        .checked_add(u64::from(record))
        .ok_or(io::ErrorKind::InvalidInput)?;
    out.write_all(&[tag])?;
    out.write_all(&length.to_le_bytes())?;
    out.write_all(key.as_bytes())?;
    out.write_all(&record.to_le_bytes())?;
    *committed = end;
    Ok(())
}
fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}
fn replay(root: &Path, input: &mut (impl Read + Seek), mut end: u64) -> io::Result<()> {
    let mut key_bytes = [0u8; MAX_PATH_BYTES];
    let mut path = PathBuf::with_capacity(MAX_PATH_BYTES);
    while end != 0 {
        let footer = end.checked_sub(4).ok_or(io::ErrorKind::InvalidData)?;
        input.seek(SeekFrom::Start(footer))?;
        let record = read_u32(input)?;
        let start = end
            .checked_sub(u64::from(record))
            .ok_or(io::ErrorKind::InvalidData)?;
        if record < RECORD_OVERHEAD as u32 || record as usize > MAX_PATH_BYTES + RECORD_OVERHEAD {
            return Err(io::ErrorKind::InvalidData.into());
        }
        input.seek(SeekFrom::Start(start))?;
        let mut tag = [0];
        input.read_exact(&mut tag)?;
        let length = read_u32(input)?;
        if !matches!(tag[0], FILE_RECORD | DIRECTORY_RECORD)
            || length.checked_add(RECORD_OVERHEAD as u32) != Some(record)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let key_bytes = key_bytes
            .get_mut(..length as usize)
            .ok_or(io::ErrorKind::InvalidData)?;
        input.read_exact(key_bytes)?;
        let key = std::str::from_utf8(key_bytes).map_err(|_| io::ErrorKind::InvalidData)?;
        if !plain_key(key) || !path_fits(root, key) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        path.clear();
        path.push(root);
        for part in key.split('/') {
            path.push(part);
        }
        let _ = match tag[0] {
            FILE_RECORD => fs::remove_file(&path),
            _ => fs::remove_dir(&path),
        };
        end = start;
    }
    fs::remove_dir(root)
}

type Result<T, E> = std::result::Result<T, ManagedNamesMetadataError<E>>;
impl<S: ManagedEntrySource> ManagedSnapshot<S> {
    pub fn stage_check_names(
        &self,
        executable: &Path,
        spill_dir: &Path,
        canceled: &dyn NamesCancellation,
    ) -> Result<ManagedNamesDirectory, S::Error> {
        let default_deadline = Instant::now() + Duration::from_secs(120);
        let deadline = canceled
            .deadline()
            .map_or(default_deadline, |at| at.min(default_deadline));
        super::names_metadata::check(canceled)?;
        let control = self
            .catalog
            .memory_budget()
            .try_reserve(Category::Metadata, CONTROL_BYTES)
            .map_err(BudgetError::from)?;
        if !spill_dir.is_absolute() {
            return Err(io::Error::from(io::ErrorKind::InvalidInput).into());
        }
        if spill_dir.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES / 2
            || spill_dir.components().count() > MAX_COMPONENTS
        {
            return Err(BudgetError::Overflow.into());
        }
        let journal = Journal::new(spill_dir)?;
        let mut directory = tempfile::Builder::new()
            .prefix("names-")
            .tempdir_in(spill_dir)?;
        directory.disable_cleanup(true);
        let mut lease = ManagedNamesDirectory {
            directory,
            journal,
            identity: self.open.data_identity(),
            _control: control,
        };
        let mut sink = Sink {
            root: lease.directory.path(),
            journal: &mut lease.journal,
            canceled,
            deadline,
            stopped: None,
        };
        let result = self.visit_names_metadata(canceled, DOCUMENT_LIMIT, &mut |path, bytes| {
            sink.put(path, bytes)
        });
        sink.finish(result)?;
        let result = self.visit_names_configdump(canceled, DOCUMENT_LIMIT, &mut |path, bytes| {
            sink.put(path, bytes)
        });
        sink.finish(result)?;
        let result = self.visit_names_modules(canceled, &mut |path, bytes| sink.put(path, bytes));
        sink.finish(result)?;
        let result = self.visit_names_forms_before(
            executable,
            spill_dir,
            canceled,
            deadline,
            &mut |path, bytes, deferred| {
                if deferred {
                    sink.check()
                } else {
                    sink.put(path, bytes)
                }
            },
        );
        sink.finish(result)?;
        let result = self.visit_names_predefined(canceled, DOCUMENT_LIMIT, &mut |path, bytes| {
            sink.put(path, bytes)
        });
        sink.finish(result)?;
        let result = sink.put(DONE, b"").map_err(ManagedNamesMetadataError::from);
        sink.finish(result)?;
        Ok(lease)
    }
}

enum Stopped {
    Canceled,
    Timeout,
    Size,
}
struct Sink<'a> {
    root: &'a Path,
    journal: &'a mut Journal,
    canceled: &'a dyn NamesCancellation,
    deadline: Instant,
    stopped: Option<Stopped>,
}
impl Sink<'_> {
    fn check(&mut self) -> io::Result<()> {
        if self.stopped.is_none() {
            if self.canceled.is_canceled() {
                self.stopped = Some(Stopped::Canceled);
            } else if Instant::now() >= self.deadline {
                self.stopped = Some(Stopped::Timeout);
            }
        }
        if self.stopped.is_some() {
            Err(io::ErrorKind::Interrupted.into())
        } else {
            Ok(())
        }
    }
    fn put(&mut self, relative: &str, bytes: &[u8]) -> io::Result<()> {
        self.check()?;
        if !plain_key(relative) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if !path_fits(self.root, relative) {
            self.stopped = Some(Stopped::Size);
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut path = self.root.to_path_buf();
        let mut components = relative.split('/').peekable();
        let mut prefix_bytes = 0usize;
        while let Some(part) = components.next() {
            self.check()?;
            path.push(part);
            prefix_bytes += part.len();
            if components.peek().is_some() {
                self.journal
                    .append(DIRECTORY_RECORD, &relative[..prefix_bytes])?;
                prefix_bytes += 1;
                match fs::create_dir(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let metadata = fs::symlink_metadata(&path)?;
                        if !metadata.is_dir() || metadata.file_type().is_symlink() {
                            return Err(io::ErrorKind::InvalidInput.into());
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        self.journal.append(FILE_RECORD, relative)?;
        let mut file = File::create(&path)?;
        for chunk in bytes.chunks(64 << 10) {
            self.check()?;
            file.write_all(chunk)?;
        }
        drop(file);
        self.check()
    }
    fn finish<T, E>(&mut self, result: Result<T, E>) -> Result<T, E> {
        if let Some(stopped) = self.stopped.take() {
            return Err(match stopped {
                Stopped::Canceled => ManagedNamesMetadataError::Canceled,
                Stopped::Timeout => SelectedFormError::Native(AnalysisError::Timeout).into(),
                Stopped::Size => BudgetError::Overflow.into(),
            });
        }
        let result = result?;
        super::names_metadata::check(self.canceled)?;
        if Instant::now() >= self.deadline {
            return Err(SelectedFormError::Native(AnalysisError::Timeout).into());
        }
        Ok(result)
    }
}
