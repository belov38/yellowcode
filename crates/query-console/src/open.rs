use md_model::LoadProfile;
use query_ir::schema::TableKind;
use query_testhost::TestBase;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FILE_NAME: &str = "1Cv8.1CD";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Info {
    pub path: String,
    pub kind: String,
    pub tables: usize,
    pub objects: usize,
    pub lang: String,
    pub notes: Vec<String>,
}

pub struct Console {
    pub(crate) cursor_generation: String,
    pub(crate) base: Arc<TestBase>,
    pub(crate) path: PathBuf,
    pub(crate) info: Info,
    pub(crate) presentation: std::sync::OnceLock<bool>,
    pub(crate) explain_busy: std::sync::atomic::AtomicBool,
    pub(crate) tables: std::sync::OnceLock<crate::data::DataTableList>,
    pub(crate) table_builds: std::sync::atomic::AtomicUsize,
    pub(crate) profile: LoadProfile,
    pub(crate) filter_scan_rows: u64,
}

impl Console {
    pub fn open(path: &Path) -> Result<Console, String> {
        Self::open_options(path, None, LoadProfile::Full).map_err(|e| e.to_string())
    }

    pub fn open_profile(path: &Path, profile: LoadProfile) -> Result<Console, String> {
        Self::open_options(path, None, profile).map_err(|e| e.to_string())
    }

    pub fn open_with_memory(
        path: &Path,
        budget: memory_budget::MemoryBudget,
        cache_bytes: usize,
    ) -> Result<Console, Box<dyn std::error::Error + Send + Sync>> {
        Self::open_options(path, Some((budget, cache_bytes)), LoadProfile::Query)
    }

    fn open_options(
        path: &Path,
        memory: Option<(memory_budget::MemoryBudget, usize)>,
        profile: LoadProfile,
    ) -> Result<Console, Box<dyn std::error::Error + Send + Sync>> {
        let (dir, file) = if path.is_dir() {
            (path.to_path_buf(), path.join(FILE_NAME))
        } else if path.is_file() {
            let is_base = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(FILE_NAME));
            if !is_base {
                return Err(format!(
                    "{}: expected a file named {FILE_NAME} or a directory with it",
                    path.display()
                )
                .into());
            }
            let dir = path
                .parent()
                .map(|p| {
                    if p.as_os_str().is_empty() {
                        Path::new(".")
                    } else {
                        p
                    }
                })
                .unwrap_or(Path::new("."))
                .to_path_buf();
            (dir, path.to_path_buf())
        } else {
            return Err(format!("{}: no such file or directory", path.display()).into());
        };
        if !file.is_file() {
            return Err(format!("{}: no {FILE_NAME} in the directory", path.display()).into());
        }
        let (base, profile) = match memory {
            Some((budget, cache)) => (
                TestBase::open_with_memory(&dir, budget, cache)?,
                LoadProfile::Query,
            ),
            None => (TestBase::open_profile(&dir, profile)?, profile),
        };
        let schema = base.schema();
        let objects = schema
            .tables()
            .iter()
            .filter(|t| matches!(t.kind, TableKind::Object(_)))
            .count();
        let info = Info {
            path: file.display().to_string(),
            kind: "infobase".to_string(),
            tables: schema.tables().len(),
            objects,
            lang: "ru".to_string(),
            notes: base.schema_notes.clone(),
        };
        Ok(Console {
            cursor_generation: crate::table::generation(),
            base: Arc::new(base),
            path: file,
            info,
            presentation: std::sync::OnceLock::new(),
            explain_busy: std::sync::atomic::AtomicBool::new(false),
            tables: std::sync::OnceLock::new(),
            table_builds: std::sync::atomic::AtomicUsize::new(0),
            profile,
            filter_scan_rows: crate::table::FILTER_SCAN_ROWS,
        })
    }

    #[doc(hidden)]
    pub fn set_filter_scan_rows(&mut self, rows: u64) {
        self.filter_scan_rows = rows.max(1);
    }

    #[doc(hidden)]
    pub fn load_profile(&self) -> LoadProfile {
        self.profile
    }

    pub fn bind_cursors_to_file(&mut self) -> std::io::Result<bool> {
        let meta = std::fs::metadata(&self.path)?;
        match file_generation(meta.len(), meta.modified()) {
            Some(generation) => {
                self.cursor_generation = generation;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn reader_stats(&self) -> query_testhost::ReaderStats {
        self.base.reader_stats()
    }

    pub fn evict_clean_cache(&self) {
        self.base.evict_clean_cache();
    }

    pub fn cache_evictor(&self) -> Arc<onecd::Database> {
        self.base.evictor()
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn file_generation(len: u64, modified: std::io::Result<std::time::SystemTime>) -> Option<String> {
    let mtime = modified
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("f{len:x}-{mtime:x}"))
}
