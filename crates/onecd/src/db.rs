use crate::catalog::Metadata;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};

use crate::blobstore::BlobStore;
use crate::catalog::{
    Catalog, CatalogEntry, ROOT_CONTENT_BLOCK, ROOT_PAGE, check_descr_kept_name, check_new_descr,
};
use crate::collate::Collate;
use crate::error::{Constraint, Error, Location, LockedBy, NotFound, Result, Unsupported};
use crate::format::PAGE_SIZES_838;
use crate::format::blob::{BLOCK, BLOCK_DATA, BlobBlock, encode_block};
use crate::format::bytes::OrCorrupt;
use crate::format::descr::{TableDescr, names_eq};
use crate::format::freelist::{FREELIST_PAGE, FreeListHeader, encode_freelist_header_838};
use crate::format::header::{DbHeader, encode_header};
use crate::format::ifile::encode_ifile_header_838;
use crate::format::key::{KeyCtx, index_key_len};
use crate::format::layout::RecordLayout;
use crate::format::root::{
    RootContent, encode_root_838, parse_root, replace_table_refs_838, valid_new_locale,
};
use crate::format::value::{BlobRef, Row};
use crate::format::{Format, FormatInfo};
use crate::fsinfo::is_network_fs;
use crate::ifile::IFile;
use crate::index::write::create_index_file;
use crate::pager::txn::TxnPages;
use crate::pager::versioned::{POISONED, Pager, Snapshot};
use crate::pager::{FileStorage, PageRead, Storage};
use crate::table::{RowSource, TableFiles, TableRead, TableWrite, check_writable, has_string_keys};
use crate::verify::{self, VerifyReport};

pub type RowId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    Full,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locking {
    Exclusive,
    None,
}

const MIB: usize = 1 << 20;

#[derive(Clone)]
pub struct OpenOptions {
    pub mode: Mode,
    pub memory_budget: Option<memory_budget::MemoryBudget>,
    pub locking: Locking,
    pub cache_bytes: usize,
    pub max_dirty_bytes: usize,
    pub max_retained_bytes: usize,
    pub sync: SyncMode,
    pub collate: Option<Arc<dyn Collate>>,
}

impl OpenOptions {
    fn with_mode(mode: Mode) -> OpenOptions {
        OpenOptions {
            mode,
            memory_budget: None,
            locking: Locking::Exclusive,
            cache_bytes: 256 * MIB,
            max_dirty_bytes: 512 * MIB,
            max_retained_bytes: 512 * MIB,
            sync: SyncMode::Full,
            collate: None,
        }
    }

    pub fn read_only() -> OpenOptions {
        OpenOptions::with_mode(Mode::ReadOnly)
    }

    pub fn read_write() -> OpenOptions {
        OpenOptions::with_mode(Mode::ReadWrite)
    }
}

impl fmt::Debug for OpenOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenOptions")
            .field("mode", &self.mode)
            .field("locking", &self.locking)
            .field("cache_bytes", &self.cache_bytes)
            .field("max_dirty_bytes", &self.max_dirty_bytes)
            .field("max_retained_bytes", &self.max_retained_bytes)
            .field("sync", &self.sync)
            .field("collate", &self.collate.as_ref().map(|_| "<collator>"))
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub format: Format,
    pub page_size: u32,
    pub locale: String,
    pub open: OpenOptions,
}

const NEW_DB_PAGES: u32 = 4;
const NEW_ROOT_DATA_PAGE: u32 = 3;

fn new_database_image(opts: &CreateOptions) -> Result<Vec<u8>> {
    if opts.format != Format::V838 {
        return Err(Error::Unsupported(Unsupported::WriteLegacyFormat));
    }
    let invalid = |what: String| Error::Io(io::Error::new(io::ErrorKind::InvalidInput, what));
    let page_size = opts.page_size;
    if !PAGE_SIZES_838.contains(&page_size) {
        return Err(invalid(format!(
            "page size {page_size} is not one of {PAGE_SIZES_838:?}"
        )));
    }
    if !valid_new_locale(&opts.locale) {
        return Err(invalid(format!(
            "locale {:?} is not 1 to 31 printable ASCII characters",
            opts.locale
        )));
    }
    let [mut header, mut free, mut root_header, mut root_data] =
        [(); NEW_DB_PAGES as usize].map(|_| vec![0u8; page_size as usize]);
    let h = DbHeader {
        version: [8, 3, 8, 0],
        format: Format::V838,
        length_pages: NEW_DB_PAGES,
        freelist_page: FREELIST_PAGE,
        page_size,
    };
    encode_header(&h, &mut header)?;
    let fl = FreeListHeader {
        count: 0,
        counter: 0,
        slots: Vec::new(),
    };
    encode_freelist_header_838(&fl, &mut free)?;

    let content = encode_root_838(&RootContent {
        locale: opts.locale.clone(),
        table_refs: Vec::new(),
    });
    let at_root = || Location::Root;
    let mut data = [0u8; BLOCK_DATA];
    data.get_mut(..content.len())
        .or_corrupt(at_root, "root content does not fit a block")?
        .copy_from_slice(&content);
    let used =
        (u16::try_from(content.len()).ok()).or_corrupt(at_root, "root content is too long")?;
    let blocks = root_data
        .get_mut(..2 * BLOCK)
        .or_corrupt(at_root, "page is shorter than two blocks")?;
    let content_block = blocks
        .get_mut(BLOCK..)
        .or_corrupt(at_root, "page is shorter than two blocks")?;
    encode_block(
        &BlobBlock {
            next: 0,
            used,
            data,
        },
        content_block,
    );
    let root_len = 2 * BLOCK as u64;
    encode_ifile_header_838(
        0,
        [1, 1, 0],
        root_len,
        &[NEW_ROOT_DATA_PAGE],
        &mut root_header,
    )?;
    Ok([header, free, root_header, root_data].concat())
}

fn write_new_file(path: &Path, dir: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let written = f.write_all(bytes).and_then(|()| f.sync_all());
    if let Err(e) = written {
        drop(f);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    sync_dir(dir);
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbInfo {
    pub format: Format,
    pub version: [u8; 4],
    pub page_size: u32,
    pub pages: u32,
    pub locale: String,
    pub tables: Vec<String>,
    pub free_pages: u32,
}

pub(crate) struct Inner {
    pub pager: Metadata<Pager>,
    pub fi: FormatInfo,
    pub opts: OpenOptions,
}

pub struct Database {
    inner: Metadata<Inner>,
    writer: Mutex<()>,
    writer_thread: Mutex<Option<ThreadId>>,
}

struct WriterSlot<'db> {
    thread: &'db Mutex<Option<ThreadId>>,
    _guard: MutexGuard<'db, ()>,
}

impl Drop for WriterSlot<'_> {
    fn drop(&mut self) {
        *self.thread.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

impl fmt::Debug for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.inner.pager.snapshot();
        f.debug_struct("Database")
            .field("format", &s.header.format)
            .field("page_size", &s.header.page_size)
            .field("pages", &s.num_pages)
            .field("epoch", &s.epoch)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "icu")]
fn default_collator() -> Option<&'static dyn Collate> {
    static ICU: crate::collate::IcuCollate = crate::collate::IcuCollate;
    Some(&ICU)
}
#[cfg(not(feature = "icu"))]
fn default_collator() -> Option<&'static dyn Collate> {
    None
}
impl OpenOptions {
    fn collator(&self) -> Option<&dyn Collate> {
        match self.collate.as_deref() {
            Some(collator) => Some(collator),
            None => default_collator(),
        }
    }
}

impl Database {
    pub fn file_close_observer(&self) -> Option<crate::FileCloseObserver> {
        self.inner.pager.file_close_observer()
    }

    pub fn open(path: &Path, opts: OpenOptions) -> Result<Database> {
        if opts.memory_budget.is_some() && opts.locking != Locking::None {
            return Err(Error::Unsupported(Unsupported::ManagedLocking));
        }
        let write = opts.mode == Mode::ReadWrite;
        if write && is_network_fs(path)? {
            return Err(Error::Unsupported(Unsupported::NetworkFs));
        }
        let storage = match opts.locking {
            Locking::Exclusive => FileStorage::open_locked(path, opts.mode)?,
            Locking::None => FileStorage::open(path, write)?,
        };
        let storage = crate::pager::storage::StorageOwner::File(Metadata::try_new(
            storage,
            opts.memory_budget.as_ref(),
        )?);
        Database::open_storage_owner(storage, opts)
    }

    pub fn create(path: &Path, opts: CreateOptions) -> Result<Database> {
        let image = new_database_image(&opts)?;
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        if is_network_fs(dir)? {
            return Err(Error::Unsupported(Unsupported::NetworkFs));
        }
        write_new_file(path, dir, &image)?;
        Database::open(path, opts.open).inspect_err(|_| {
            let _ = std::fs::remove_file(path);
        })
    }

    #[doc(hidden)]
    pub fn create_storage(storage: Arc<dyn Storage>, opts: CreateOptions) -> Result<Database> {
        let image = new_database_image(&opts)?;
        if storage.len()? != 0 {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the storage is not empty",
            )));
        }
        storage.write_at(0, &image)?;
        storage.sync()?;
        Database::open_storage(storage, opts.open)
    }

    #[doc(hidden)]
    pub fn open_storage(storage: Arc<dyn Storage>, opts: OpenOptions) -> Result<Database> {
        Self::open_storage_owner(storage.into(), opts)
    }
    fn open_storage_owner(
        storage: crate::pager::storage::StorageOwner,
        opts: OpenOptions,
    ) -> Result<Database> {
        let (pager, header) = Pager::open(storage, &opts)?;
        if opts.mode == Mode::ReadWrite && header.format != Format::V838 {
            return Err(Error::Unsupported(Unsupported::WriteLegacyFormat));
        }
        let fi = FormatInfo {
            format: header.format,
            page_size: header.page_size,
        };
        let (free, catalog) = {
            let snap = pager.snapshot();
            let free = crate::format::freelist::parse_freelist_count(
                fi,
                &snap.read_page(header.freelist_page)?,
            )?;
            (free, Catalog::load_shared(&snap, fi, header.version)?)
        };
        pager.set_opened(catalog, free);
        let budget = pager.memory_budget().cloned();
        let inner = Metadata::try_new(Inner { pager, fi, opts }, budget.as_ref())?;
        Ok(Database {
            inner,
            writer: Mutex::new(()),
            writer_thread: Mutex::new(None),
        })
    }

    pub fn memory_budget(&self) -> Option<&memory_budget::MemoryBudget> {
        self.inner.pager.memory_budget()
    }

    pub fn reader_stats(&self) -> Option<crate::ReaderStats> {
        self.inner.pager.reader_stats()
    }

    pub fn keep_opened_files(&self) {
        self.inner.pager.keep_opened_files();
    }

    pub fn evict_clean_cache(&self) {
        self.inner.pager.evict_clean_cache();
    }

    pub fn info(&self) -> DbInfo {
        let s = self.inner.pager.snapshot();
        DbInfo {
            format: s.header.format,
            version: s.header.version,
            page_size: s.header.page_size,
            pages: s.num_pages,
            locale: s.catalog.locale.clone(),
            tables: s.catalog.table_names(),
            free_pages: s.free_pages,
        }
    }

    pub fn table_errors(&self) -> Vec<(u32, Error)> {
        let s = self.inner.pager.snapshot();
        s.catalog
            .errors
            .iter()
            .map(|(r, e)| (*r, e.duplicate()))
            .collect()
    }

    pub fn read(&self) -> ReadTxn {
        ReadTxn {
            inner: self.inner.clone(),
            snap: self.inner.pager.snapshot(),
        }
    }

    pub fn write(&self) -> Result<WriteTxn<'_>> {
        if self.inner.opts.mode == Mode::ReadOnly {
            return Err(Error::Unsupported(Unsupported::ReadOnly));
        }
        if self.inner.fi.format != Format::V838 {
            return Err(Error::Unsupported(Unsupported::WriteLegacyFormat));
        }
        let me = thread::current().id();
        let holder = || {
            self.writer_thread
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        };
        if *holder() == Some(me) {
            return Err(Error::Locked { by: LockedBy::Ours });
        }
        let guard = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        *holder() = Some(me);
        let slot = WriterSlot {
            thread: &self.writer_thread,
            _guard: guard,
        };
        if self.inner.pager.poisoned() {
            return Err(Error::corrupt(Location::File, POISONED));
        }
        let pages = TxnPages::new(self.inner.pager.snapshot(), self.inner.opts.max_dirty_bytes)?;
        let catalog = pages.base().catalog.clone();
        Ok(WriteTxn {
            db: self,
            _slot: slot,
            pages,
            catalog,
            root: None,
            tables: HashMap::new(),
            rv_next: 0,
            last_table: None,
        })
    }

    pub fn verify(&self) -> Result<VerifyReport> {
        self.read().verify()
    }
}

pub struct WriteTxn<'db> {
    db: &'db Database,
    _slot: WriterSlot<'db>,
    pages: TxnPages,
    catalog: Metadata<Catalog>,
    root: Option<BlobStore>,
    tables: HashMap<String, TableFiles>,
    rv_next: u32,
    last_table: Option<usize>,
}

impl fmt::Debug for WriteTxn<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteTxn")
            .field("pages", &self.pages)
            .finish_non_exhaustive()
    }
}

impl WriteTxn<'_> {
    pub fn abort(&mut self) {
        self.pages.abort();
    }

    pub fn pending_bytes(&self) -> usize {
        self.pages.pending_bytes()
    }

    pub fn table(&mut self, name: &str) -> Result<TableWrite<'_>> {
        self.pages.check_alive()?;
        let _unwind = self.pages.abort_on_panic();
        let db = self.db;
        let fi = db.inner.fi;
        let index = match self
            .last_table
            .filter(|&i| self.catalog.at_named(i, name).is_some())
        {
            Some(i) => i,
            None => self
                .catalog
                .position(name)
                .ok_or_else(|| Error::NotFound(NotFound::Table(name.to_string())))?,
        };
        self.last_table = Some(index);
        let entry = self
            .catalog
            .at_named(index, name)
            .ok_or_else(|| Error::NotFound(NotFound::Table(name.to_string())))?;
        check_writable(&entry.descr)?;
        let files = match self.tables.entry(entry.descr.name.to_uppercase()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(TableFiles::open(&self.pages, fi, entry)?),
        };
        files.settle(&mut self.pages)?;
        let keys = KeyCtx {
            collate: db.inner.opts.collator(),
            locale: &self.catalog.locale,
        };
        Ok(TableWrite::new(
            &mut self.pages,
            fi,
            entry,
            files,
            &mut self.rv_next,
            keys,
        ))
    }

    pub fn read_table(&self, name: &str) -> Result<TableRead<'_>> {
        let entry = self
            .catalog
            .find(name)
            .ok_or_else(|| Error::NotFound(NotFound::Table(name.to_string())))?;
        let keys = KeyCtx {
            collate: self.db.inner.opts.collator(),
            locale: &self.catalog.locale,
        };
        TableRead::open(&self.pages, self.db.inner.fi, entry, keys)
    }

    pub fn create_table(&mut self, d: &TableDescr) -> Result<()> {
        self.pages.check_alive()?;
        let _unwind = self.pages.abort_on_panic();
        let fi = self.db.inner.fi;
        let (mut d, layout, key_lens) = self.check_new_table(d, None)?;
        let root = root_store(&mut self.root, &self.pages, fi)?;
        let RootRead {
            content,
            content_ref,
            mut refs,
        } = read_root(root, &self.pages)?;
        check_root_room(root, fi, &d, content.len().saturating_add(4))?;

        let (descr_ref, files) = self.pages.run(|tx| {
            let files = create_files(tx, fi, &mut d, &key_lens)?;
            let descr_ref = root.write_new_chain(tx, d.to_text().as_bytes())?.start;
            refs.push(descr_ref);
            write_root(root, tx, &content, content_ref, &refs)?;
            Ok((descr_ref, files))
        })?;
        self.tables.insert(d.name.to_uppercase(), files);
        Metadata::make_mut(&mut self.catalog)?.add_table(descr_ref, d, layout);
        Ok(())
    }

    pub fn drop_table(&mut self, name: &str) -> Result<()> {
        self.pages.check_alive()?;
        let _unwind = self.pages.abort_on_panic();
        let fi = self.db.inner.fi;
        let entry = self.find_table(name)?;
        let files = self.files_to_free(&entry)?;
        let root = root_store(&mut self.root, &self.pages, fi)?;
        let RootRead {
            content,
            content_ref,
            mut refs,
        } = read_root(root, &self.pages)?;
        unlist(&mut refs, &entry)?;
        check_descr_not_free(root, &self.pages, &entry)?;

        self.pages.run(|tx| {
            for f in files {
                f.free_all(tx)?;
            }
            root.free_chain(tx, entry.descr_ref)?;
            write_root(root, tx, &content, content_ref, &refs)
        })?;
        self.tables.remove(&entry.descr.name.to_uppercase());
        Metadata::make_mut(&mut self.catalog)?.remove_table(entry.descr_ref);
        Ok(())
    }

    pub fn rebuild_table(
        &mut self,
        name: &str,
        new: &TableDescr,
        convert: &mut dyn FnMut(&Row) -> Result<Row>,
    ) -> Result<()> {
        self.pages.check_alive()?;
        let _unwind = self.pages.abort_on_panic();
        let db = self.db;
        let fi = db.inner.fi;
        let old = self.find_table(name)?;
        let (mut d, layout, key_lens) = self.check_new_table(new, Some(&old))?;
        let collate = db.inner.opts.collator();
        if has_string_keys(&d) && collate.is_none() {
            return Err(Error::Unsupported(Unsupported::NoCollator));
        }
        let old_files = self.files_to_free(&old)?;
        let rows = RowSource::open(&self.pages, fi, &old)?;
        let root = root_store(&mut self.root, &self.pages, fi)?;
        let RootRead {
            content,
            content_ref,
            mut refs,
        } = read_root(root, &self.pages)?;
        unlist(&mut refs, &old)?;
        check_descr_not_free(root, &self.pages, &old)?;
        check_root_room(root, fi, &d, content.len())?;

        let keys = KeyCtx {
            collate,
            locale: &self.catalog.locale,
        };
        let rv_next = &mut self.rv_next;
        let (entry, files) = self.pages.run(|tx| {
            let mut files = create_files(tx, fi, &mut d, &key_lens)?;
            let mut entry = CatalogEntry {
                descr_ref: 0,
                descr: Metadata::new(d),
                layout: Metadata::new(layout),
            };
            TableWrite::new(tx, fi, &entry, &mut files, rv_next, keys).move_from(&rows, convert)?;
            entry.descr_ref = root
                .write_new_chain(tx, entry.descr.to_text().as_bytes())?
                .start;
            refs.push(entry.descr_ref);
            write_root(root, tx, &content, content_ref, &refs)?;
            root.free_chain(tx, old.descr_ref)?;
            for f in old_files {
                f.free_all(tx)?;
            }
            Ok((entry, files))
        })?;
        self.tables.remove(&old.descr.name.to_uppercase());
        self.tables.insert(entry.descr.name.to_uppercase(), files);
        let catalog = Metadata::make_mut(&mut self.catalog)?;
        catalog.remove_table(old.descr_ref);
        catalog.add_table(
            entry.descr_ref,
            entry.descr.unwrap_or_clone()?,
            entry.layout.unwrap_or_clone()?,
        );
        Ok(())
    }

    pub fn commit(self) -> Result<()> {
        let WriteTxn {
            db,
            _slot,
            mut pages,
            mut catalog,
            mut tables,
            ..
        } = self;
        for files in tables.values_mut() {
            files.settle(&mut pages)?;
        }
        pages.apply_versions()?;
        let same_catalog = Metadata::ptr_eq(&catalog, &pages.base().catalog);
        if !same_catalog {
            let root = IFile::open(&pages, db.inner.fi, ROOT_PAGE)?;
            Metadata::make_mut(&mut catalog)?.root = root;
        }
        let header = pages.header();
        let free_pages = pages.free_count();
        let (dirty, num_pages) = pages.into_dirty()?;
        if dirty.is_empty() && same_catalog {
            return Ok(());
        }
        db.inner
            .pager
            .commit(dirty, num_pages, catalog, header, free_pages)?;
        Ok(())
    }
}

impl WriteTxn<'_> {
    fn check_new_table(
        &self,
        d: &TableDescr,
        replaces: Option<&CatalogEntry>,
    ) -> Result<(TableDescr, RecordLayout, Vec<u32>)> {
        let d = TableDescr {
            files: [0; 3],
            ..d.clone()
        };
        let kept = replaces.is_some_and(|e| names_eq(&e.descr.name, &d.name));
        let layout = match kept {
            true => check_descr_kept_name(&d)?,
            false => check_new_descr(&d)?,
        };
        check_writable(&d)?;
        let replaced = replaces.map(|e| e.descr_ref);
        let taken = (self.catalog.find(&d.name)).filter(|e| Some(e.descr_ref) != replaced);
        if let Some(e) = taken {
            return Err(Error::Constraint(Constraint::BadDescr(format!(
                "table {} already exists",
                e.descr.name
            ))));
        }
        let fi = self.db.inner.fi;
        let key_lens = (d.indexes.iter())
            .map(|ix| index_key_len(&d, ix))
            .collect::<Result<Vec<u32>>>()?;
        if key_lens.len() >= (fi.page_size / 4) as usize {
            return Err(Error::Constraint(Constraint::BadDescr(format!(
                "table {}: {} indexes do not fit the index file header",
                d.name,
                key_lens.len()
            ))));
        }
        Ok((d, layout, key_lens))
    }

    fn find_table(&self, name: &str) -> Result<CatalogEntry> {
        (self.catalog.find(name).cloned())
            .ok_or_else(|| Error::NotFound(NotFound::Table(name.to_string())))
    }

    fn files_to_free(&self, entry: &CatalogEntry) -> Result<Vec<IFile>> {
        let d = &entry.descr;
        let at_table = || Location::Table(d.name.clone());
        let mut in_use = vec![ROOT_PAGE];
        for other in &self.catalog.entries {
            if other.descr_ref != entry.descr_ref {
                in_use.extend(other.descr.files.iter().filter(|&&p| p != 0));
            }
        }
        let mut files = Vec::new();
        for (&h, role) in d.files.iter().zip(["data", "blob", "index"]) {
            if h == 0 {
                continue;
            }
            if in_use.contains(&h) {
                return Err(Error::corrupt(
                    at_table(),
                    format!("its {role} file {h} is also the root file or another table file"),
                ));
            }
            in_use.push(h);
            let f = IFile::open(&self.pages, self.db.inner.fi, h)?;
            if let Some(p) = f.owned_pages().find(|&p| self.pages.is_free(p)) {
                return Err(Error::corrupt(
                    Location::Page(p),
                    format!(
                        "page is free, but the {role} file of table {} has it",
                        d.name
                    ),
                ));
            }
            files.push(f);
        }
        Ok(files)
    }
}

fn root_store<'a>(
    root: &'a mut Option<BlobStore>,
    pages: &TxnPages,
    fi: FormatInfo,
) -> Result<&'a mut BlobStore> {
    Ok(match root {
        Some(r) => r,
        None => root.insert(BlobStore::new(IFile::open(pages, fi, ROOT_PAGE)?)?),
    })
}

fn check_root_room(
    root: &BlobStore,
    fi: FormatInfo,
    d: &TableDescr,
    content_len: usize,
) -> Result<()> {
    let blocks = |n: usize| n.div_ceil(BLOCK_DATA) as u64;
    let widest = TableDescr {
        files: [u32::MAX; 3],
        ..d.clone()
    };
    let worst = u64::from(root.nblocks())
        .saturating_add(blocks(widest.to_text().len()))
        .saturating_add(blocks(content_len))
        .saturating_mul(BLOCK as u64);
    root.file.check_len(fi, worst)
}

fn create_files(
    tx: &mut TxnPages,
    fi: FormatInfo,
    d: &mut TableDescr,
    key_lens: &[u32],
) -> Result<TableFiles> {
    let data = IFile::create(tx)?.owned_by(&d.name, "data");
    let blob = match d.fields.iter().any(|f| f.ty.is_blob()) {
        true => Some(BlobStore::new(IFile::create(tx)?)?.named(&d.name)),
        false => None,
    };
    let index = match key_lens.is_empty() {
        true => None,
        false => {
            let mut f = IFile::create(tx)?.owned_by(&d.name, "index");
            create_index_file(tx, &mut f, fi, key_lens)?;
            Some(f)
        }
    };
    d.files = [
        data.header_page,
        blob.as_ref().map_or(0, |b| b.file.header_page),
        index.as_ref().map_or(0, |f| f.header_page),
    ];
    Ok(TableFiles::created(data, blob, index, key_lens.len()))
}

fn unlist(refs: &mut Vec<u32>, entry: &CatalogEntry) -> Result<()> {
    let n = refs.len();
    refs.retain(|&r| r != entry.descr_ref);
    let times = n - refs.len();
    if times != 1 {
        return Err(Error::corrupt(
            Location::Root,
            format!(
                "description {} of table {} is listed {times} times",
                entry.descr_ref, entry.descr.name
            ),
        ));
    }
    Ok(())
}

fn check_descr_not_free(root: &BlobStore, pages: &TxnPages, entry: &CatalogEntry) -> Result<()> {
    let free: HashSet<u32> = root.free_blocks(pages)?.into_iter().collect();
    let chain = root.chain(pages, entry.descr_ref)?;
    if let Some(b) = chain.into_iter().find(|b| free.contains(b)) {
        return Err(Error::corrupt(
            Location::Root,
            format!(
                "block {b} of the description of table {} is on the free block list",
                entry.descr.name
            ),
        ));
    }
    Ok(())
}

struct RootRead {
    content: Vec<u8>,
    content_ref: BlobRef,
    refs: Vec<u32>,
}

fn read_root(root: &BlobStore, pages: &TxnPages) -> Result<RootRead> {
    let content = root.read_chain(pages, ROOT_CONTENT_BLOCK)?;
    let refs = parse_root(pages.header().version, &content)?.table_refs;
    let content_ref = BlobRef {
        start: ROOT_CONTENT_BLOCK,
        len: (u32::try_from(content.len()).ok())
            .or_corrupt(|| Location::Root, "root content is too long")?,
    };
    Ok(RootRead {
        content,
        content_ref,
        refs,
    })
}

fn write_root(
    root: &mut BlobStore,
    tx: &mut TxnPages,
    content: &[u8],
    content_ref: BlobRef,
    refs: &[u32],
) -> Result<()> {
    let new_content = replace_table_refs_838(content, refs)?;
    let at = root.rewrite_chain(tx, content_ref, &new_content)?;
    if at.start != ROOT_CONTENT_BLOCK {
        return Err(Error::corrupt(
            Location::Root,
            format!("root content moved to block {}", at.start),
        ));
    }
    Ok(())
}

pub struct ReadTxn {
    inner: Metadata<Inner>,
    snap: Snapshot,
}

impl fmt::Debug for ReadTxn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadTxn")
            .field("epoch", &self.snap.epoch)
            .finish_non_exhaustive()
    }
}

impl ReadTxn {
    pub fn memory_budget(&self) -> Option<&memory_budget::MemoryBudget> {
        self.inner.pager.memory_budget()
    }
    pub fn table_names(&self) -> Vec<String> {
        self.snap.catalog.table_names()
    }

    pub fn table(&self, name: &str) -> Result<TableRead<'_>> {
        let entry = self
            .snap
            .catalog
            .find(name)
            .ok_or_else(|| Error::NotFound(NotFound::Table(name.to_string())))?;
        TableRead::open(&self.snap, self.inner.fi, entry, self.keys())
    }

    pub fn find_table(&self, name: &str) -> Result<Option<TableRead<'_>>> {
        self.snap
            .catalog
            .find(name)
            .map(|entry| TableRead::open(&self.snap, self.inner.fi, entry, self.keys()))
            .transpose()
    }

    pub fn verify(&self) -> Result<VerifyReport> {
        let s = &self.snap;
        verify::run(
            s,
            self.inner.fi,
            s.header.freelist_page,
            &s.catalog,
            self.keys(),
            s.trailing_pages,
        )
    }

    fn keys(&self) -> KeyCtx<'_> {
        KeyCtx {
            collate: self.inner.opts.collator(),
            locale: &self.snap.catalog.locale,
        }
    }
}

impl memory_budget::Evictor for Database {
    fn evict_clean(&self) {
        self.evict_clean_cache();
    }
}
