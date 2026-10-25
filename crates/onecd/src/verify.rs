use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Mutex, PoisonError};

use crate::blobstore::BlobStore;
use crate::catalog::{Catalog, CatalogEntry, ROOT_CONTENT_BLOCK};
use crate::collate::{Collate, Strength};
use crate::error::{Error, Location, Result, Unsupported};
use crate::format::bytes::u32le;
use crate::format::descr::{IndexDescr, TableDescr};
use crate::format::index_page::{
    BranchPage, DESCRIPTOR_FLAG, IndexPage, LeafPacking, LeafPage, NONE, leaf_flag,
    parse_descriptor, parse_index_file_header,
};
use crate::format::key::{KeyCtx, build_key, has_text, index_key_len, split_key};
use crate::format::record::{FLAG_FREE, FLAG_USED, decode_field, free_next};
use crate::format::value::{BlobVal, Value};
use crate::format::{Format, FormatInfo};
use crate::freelist::FreeList;
use crate::ifile::IFile;
use crate::index::IndexRead;
use crate::index::read::page_number;
use crate::pager::{PageBuf, PageRead};
use crate::table::TableRead;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub location: Location,
    pub what: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.what)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    pub violations: Vec<Violation>,
    pub tables: u32,
    pub live_rows: u64,
    pub index_entries: u64,
    pub keys_checked: u64,
    pub string_keys_checked: u64,
    pub table_rows: Vec<(String, u64)>,
}

const MAX_LISTED: usize = 100;
const NO_OWNER: u32 = 0;
const FREE_PAGES: u32 = 1;

pub(crate) fn run(
    pr: &dyn PageRead,
    fi: FormatInfo,
    freelist_page: u32,
    catalog: &Catalog,
    keys: KeyCtx,
    trailing_pages: u64,
) -> Result<VerifyReport> {
    let mut c = Checker::new(pr, fi);
    if trailing_pages > 0 {
        c.out.push(Violation {
            location: Location::File,
            what: format!("file has {trailing_pages} pages beyond the header length"),
        });
    }
    let header = c.owner("the file header".into());
    c.claim(0, header);
    c.check_free_list(freelist_page)?;
    c.check_root(catalog)?;
    c.check_descriptions(catalog);
    for entry in &catalog.entries {
        c.check_table(entry, keys)?;
    }
    c.check_coverage();
    Ok(c.finish())
}

fn damage(e: Error, at: impl FnOnce() -> Location) -> Result<Violation> {
    match e {
        e if e.is_fatal() => Err(e),
        Error::Corrupt { location, what } => Ok(Violation { location, what }),
        other => Ok(Violation {
            location: at(),
            what: other.to_string(),
        }),
    }
}

fn unless_fatal<T>(r: Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.is_fatal() => Err(e),
        Err(_) => Ok(None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Structure,
    Unused,
    Rows,
    Order,
    Keys,
}

const KINDS: [Kind; 5] = [
    Kind::Structure,
    Kind::Unused,
    Kind::Rows,
    Kind::Order,
    Kind::Keys,
];

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Structure => "structural problems",
            Kind::Unused => "unused or unlisted items",
            Kind::Rows => "row set problems",
            Kind::Order => "order problems",
            Kind::Keys => "key mismatches",
        }
    }
}

#[derive(Default)]
struct Listed {
    kept: [Vec<Violation>; KINDS.len()],
    more: [u64; KINDS.len()],
}

impl Listed {
    fn add(&mut self, location: Location, what: impl Into<String>) {
        self.add_kind(Kind::Structure, location, what);
    }

    fn add_kind(&mut self, kind: Kind, location: Location, what: impl Into<String>) {
        self.push_kind(
            kind,
            Violation {
                location,
                what: what.into(),
            },
        );
    }

    fn push(&mut self, v: Violation) {
        self.push_kind(Kind::Structure, v);
    }

    fn push_kind(&mut self, kind: Kind, v: Violation) {
        let i = kind as usize;
        match (self.kept.get_mut(i), self.more.get_mut(i)) {
            (Some(kept), _) if kept.len() < MAX_LISTED => kept.push(v),
            (_, Some(more)) => *more += 1,
            _ => {}
        }
    }

    fn damage(&mut self, e: Error, at: impl FnOnce() -> Location) -> Result<()> {
        let v = damage(e, at)?;
        self.push(v);
        Ok(())
    }

    fn flush(self, out: &mut Vec<Violation>, location: Location, what: &str) {
        let hidden: Vec<(Kind, u64)> = KINDS
            .iter()
            .zip(&self.more)
            .filter(|(_, n)| **n > 0)
            .map(|(k, n)| (*k, *n))
            .collect();
        out.extend(self.kept.into_iter().flatten());
        let total: u64 = hidden.iter().map(|(_, n)| n).sum();
        if total == 0 {
            return;
        }
        let text = if hidden.iter().all(|(k, _)| *k == Kind::Structure) {
            format!("{total} more {what} not listed")
        } else {
            let parts: Vec<String> = hidden
                .iter()
                .map(|(k, n)| format!("{n} {}", k.label()))
                .collect();
            format!("{total} more {what} not listed ({})", parts.join(", "))
        };
        out.push(Violation {
            location,
            what: text,
        });
    }
}

fn fmt_blocks(blocks: &[u32]) -> String {
    const SHOWN: usize = 8;
    let list: Vec<String> = blocks.iter().take(SHOWN).map(u32::to_string).collect();
    let more = if blocks.len() > SHOWN { ", ..." } else { "" };
    let noun = if blocks.len() == 1 { "block" } else { "blocks" };
    format!("{noun} {}{more}", list.join(", "))
}

fn hex(key: &[u8]) -> String {
    const SHOWN: usize = 48;
    let end = key.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let mut out: String = key
        .iter()
        .take(end.min(SHOWN))
        .map(|b| format!("{b:02x}"))
        .collect();
    if end > SHOWN {
        out.push_str("...");
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockUse {
    Free,
    Content,
    Descr(u32),
    Value { row: u32, field: u16 },
}

impl BlockUse {
    fn describe(self, d: Option<&TableDescr>) -> String {
        match self {
            BlockUse::Free => "the free blocks".into(),
            BlockUse::Content => "the root content".into(),
            BlockUse::Descr(r) => format!("description {r}"),
            BlockUse::Value { row, field } => {
                let name = d
                    .and_then(|d| d.fields.get(usize::from(field)))
                    .map_or("?", |f| f.name.as_str());
                format!("record {row} field {name}")
            }
        }
    }
}

struct Blocks {
    users: Vec<Option<BlockUse>>,
}

impl Blocks {
    fn new(n: u32) -> Blocks {
        Blocks {
            users: vec![None; n as usize],
        }
    }

    fn claim(&mut self, chain: &[u32], user: BlockUse) -> Vec<(BlockUse, Vec<u32>)> {
        let mut clashes: Vec<(BlockUse, Vec<u32>)> = Vec::new();
        for &b in chain {
            let Some(slot) = self.users.get_mut(b as usize) else {
                continue;
            };
            match *slot {
                None => *slot = Some(user),
                Some(prev) => match clashes.iter_mut().find(|(u, _)| *u == prev) {
                    Some((_, list)) => list.push(b),
                    None => clashes.push((prev, vec![b])),
                },
            }
        }
        clashes
    }

    fn unused(&self) -> impl Iterator<Item = u32> + '_ {
        self.users
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, u)| u.is_none())
            .map(|(b, _)| b as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IxUse {
    Header,
    Free,
    Descriptor(usize),
    Tree(usize),
}

impl IxUse {
    fn describe(self, d: &TableDescr) -> String {
        let name = |pos: usize| d.indexes.get(pos).map_or("?", |i| i.name.as_str());
        match self {
            IxUse::Header => "the header".into(),
            IxUse::Free => "a free page".into(),
            IxUse::Descriptor(pos) => format!("the descriptor of {}", name(pos)),
            IxUse::Tree(pos) => format!("a page of the tree of {}", name(pos)),
        }
    }
}

struct IndexPages {
    users: Vec<Option<IxUse>>,
}

impl IndexPages {
    fn claim(&mut self, no: u32, user: IxUse) -> Option<IxUse> {
        let slot = self.users.get_mut(no as usize)?;
        match *slot {
            None => {
                *slot = Some(user);
                None
            }
            prev => prev,
        }
    }

    fn unused(&self) -> impl Iterator<Item = u32> + '_ {
        self.users
            .iter()
            .enumerate()
            .filter(|(_, u)| u.is_none())
            .map(|(p, _)| p as u32)
    }
}

#[derive(Debug)]
pub(crate) enum TreeNode {
    Leaf,
    Branch(BranchPage),
}

pub(crate) type Level = Vec<(u32, TreeNode)>;

pub(crate) fn tree_levels(ix: &IndexRead) -> Result<(Vec<Level>, Vec<Error>)> {
    let mut levels: Vec<Level> = Vec::new();
    let mut bad = Vec::new();
    let mut seen = HashSet::new();
    let mut cur = vec![ix.root_page()];
    while !cur.is_empty() {
        let mut level = Vec::with_capacity(cur.len());
        for no in cur {
            if !seen.insert(no) {
                bad.push(ix.at(Some(no), "page is reached twice in the tree".into()));
                continue;
            }
            let is_leaf = match ix.raw_page(no) {
                Ok(raw) => leaf_flag(&raw) == Some(true),
                Err(e) if e.is_fatal() => return Err(e),
                Err(e) => {
                    bad.push(e);
                    continue;
                }
            };
            if is_leaf {
                level.push((no, TreeNode::Leaf));
                continue;
            }
            match ix.decode(no) {
                Ok(IndexPage::Branch(b)) => level.push((no, TreeNode::Branch(b))),
                Ok(IndexPage::Leaf(_)) => level.push((no, TreeNode::Leaf)),
                Err(e) if e.is_fatal() => return Err(e),
                Err(e) => bad.push(e),
            }
        }
        let leaves = level
            .iter()
            .filter(|(_, n)| matches!(n, TreeNode::Leaf))
            .count();
        if leaves != 0 && leaves != level.len() {
            bad.push(ix.at(
                None,
                format!(
                    "leaves at different depths: level {} has {leaves} leaves and {} branches",
                    levels.len(),
                    level.len() - leaves
                ),
            ));
        }
        let mut next = Vec::new();
        for (no, node) in &level {
            if let TreeNode::Branch(b) = node {
                for e in &b.entries {
                    match ix.page_of(e.child, *no) {
                        Ok(child) => next.push(child),
                        Err(e) => bad.push(e),
                    }
                }
            }
        }
        levels.push(level);
        cur = next;
    }
    Ok((levels, bad))
}

pub(crate) fn last_pair(p: &IndexPage) -> Option<(&[u8], u32)> {
    match p {
        IndexPage::Leaf(l) => leaf_last(l),
        IndexPage::Branch(b) => branch_last(b),
    }
}

fn leaf_last(l: &LeafPage) -> Option<(&[u8], u32)> {
    l.entries.last().map(|(row, key)| (key.as_slice(), *row))
}

fn branch_last(b: &BranchPage) -> Option<(&[u8], u32)> {
    b.entries.last().map(|e| (e.key.as_slice(), e.row))
}

pub(crate) fn link_ok(ix: &IndexRead, holder: u32, ptr: u32, want: Option<u32>) -> bool {
    match (ptr, want) {
        (NONE, None) => true,
        (NONE, Some(_)) | (_, None) => false,
        (p, Some(w)) => ix.page_of(p, holder).is_ok_and(|n| n == w),
    }
}

fn packing_ok(p: LeafPacking, key_len: u32) -> bool {
    let init = LeafPacking::initial(key_len);
    let bits = u32::from(p.nb) + u32::from(p.lb) + u32::from(p.rb);
    let bytes = u32::from(p.recbytes);
    p.lb == init.lb
        && p.rb == init.rb
        && p.nb >= init.nb
        && bits <= 8 * bytes
        && bytes == bits.div_ceil(8)
        && (bits == 8 * bytes || p.nb == 32)
}

fn ptr_text(p: u32) -> String {
    if p == NONE {
        "none".into()
    } else {
        format!("{p:#x}")
    }
}

fn page_text(p: Option<u32>) -> String {
    p.map_or_else(|| "none".into(), |p| p.to_string())
}

struct Watched<'a> {
    inner: Option<&'a dyn Collate>,
    failure: Mutex<Option<String>>,
}

impl Watched<'_> {
    fn take_failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl Collate for Watched<'_> {
    fn sort_key(&self, locale: &str, strength: Strength, text: &[u16]) -> Result<Vec<u8>> {
        let r = match self.inner {
            Some(c) => c.sort_key(locale, strength, text),
            None => Err(Error::Unsupported(Unsupported::NoCollator)),
        };
        if let Err(e) = &r {
            *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
        }
        r
    }
}

struct Blank;

impl Collate for Blank {
    fn sort_key(&self, _: &str, _: Strength, _: &[u16]) -> Result<Vec<u8>> {
        Ok(vec![0])
    }
}

struct TableIx<'x> {
    t: &'x TableRead<'x>,
    file: &'x IFile,
    live: &'x [bool],
    collate: &'x Watched<'x>,
    locale: &'x str,
}

struct IndexCx<'x> {
    t: &'x TableRead<'x>,
    ix: &'x IndexRead<'x>,
    idx: &'x IndexDescr,
    key_len: u32,
    live: &'x [bool],
    compare: bool,
    collate: &'x Watched<'x>,
    locale: &'x str,
}

impl IndexCx<'_> {
    fn at(&self, page: Option<u32>) -> Location {
        Location::Index {
            table: self.t.descr().name.clone(),
            index: self.idx.name.clone(),
            page,
        }
    }
}

struct Entries {
    seen: Vec<bool>,
    prev: Option<(Vec<u8>, u32)>,
    strings_failed: bool,
}

struct Separator<'a> {
    parent: u32,
    key: &'a [u8],
    row: u32,
}

struct Checker<'a> {
    pr: &'a dyn PageRead,
    fi: FormatInfo,
    owners: Vec<u32>,
    names: Vec<String>,
    twice: Listed,
    out: Vec<Violation>,
    tables: u32,
    live_rows: u64,
    index_entries: u64,
    keys_checked: u64,
    string_keys_checked: u64,
    table_rows: Vec<(String, u64)>,
}

impl<'a> Checker<'a> {
    fn new(pr: &'a dyn PageRead, fi: FormatInfo) -> Checker<'a> {
        Checker {
            pr,
            fi,
            owners: vec![NO_OWNER; pr.num_pages() as usize],
            names: vec![String::new(), "the free list".into()],
            twice: Listed::default(),
            out: Vec::new(),
            tables: 0,
            live_rows: 0,
            index_entries: 0,
            keys_checked: 0,
            string_keys_checked: 0,
            table_rows: Vec::new(),
        }
    }

    fn finish(self) -> VerifyReport {
        VerifyReport {
            violations: self.out,
            tables: self.tables,
            live_rows: self.live_rows,
            index_entries: self.index_entries,
            keys_checked: self.keys_checked,
            string_keys_checked: self.string_keys_checked,
            table_rows: self.table_rows,
        }
    }

    fn owner(&mut self, name: String) -> u32 {
        self.names.push(name);
        u32::try_from(self.names.len() - 1).unwrap_or(u32::MAX)
    }

    fn name(&self, owner: u32) -> &str {
        self.names.get(owner as usize).map_or("?", String::as_str)
    }

    fn check_coverage(&mut self) {
        let twice = std::mem::take(&mut self.twice);
        twice.flush(&mut self.out, Location::File, "pages owned twice");
        let mut lost = Listed::default();
        for (p, &o) in self.owners.iter().enumerate() {
            if o == NO_OWNER {
                lost.add(Location::Page(p as u32), format!("page {p} has no owner"));
            }
        }
        lost.flush(&mut self.out, Location::File, "pages without an owner");
    }

    fn claim(&mut self, page: u32, owner: u32) {
        let Some(&prev) = self.owners.get(page as usize) else {
            let what = format!(
                "page {page} of {} is beyond the end of the file",
                self.name(owner)
            );
            self.out.push(Violation {
                location: Location::Page(page),
                what,
            });
            return;
        };
        if prev == NO_OWNER {
            if let Some(slot) = self.owners.get_mut(page as usize) {
                *slot = owner;
            }
            return;
        }
        let what = if prev == FREE_PAGES && owner == FREE_PAGES {
            format!("page {page} is listed twice in the free list")
        } else {
            format!(
                "page {page} is owned twice: by {} and by {}",
                self.name(prev),
                self.name(owner)
            )
        };
        self.twice.add(Location::Page(page), what);
    }

    fn claim_file(&mut self, f: &IFile, owner: u32) {
        for p in f.owned_pages() {
            self.claim(p, owner);
        }
    }

    fn check_versions(&mut self, f: &IFile) {
        if self.fi.format == Format::V838 && f.versions[2] != u32::from(f.fatlevel) {
            self.out.push(Violation {
                location: Location::InternalFile(f.header_page),
                what: format!(
                    "version v3 is {} with fatlevel {} (v3 is 1 exactly with allocation pages)",
                    f.versions[2], f.fatlevel
                ),
            });
        }
    }

    fn open_file(&mut self, header: u32, name: String, report: bool) -> Result<Option<IFile>> {
        if header == 0 {
            return Ok(None);
        }
        let owner = self.owner(name);
        match IFile::open(self.pr, self.fi, header) {
            Ok(f) => {
                self.claim_file(&f, owner);
                self.check_versions(&f);
                Ok(Some(f))
            }
            Err(e) => {
                let v = damage(e, || Location::InternalFile(header))?;
                if report {
                    self.out.push(v);
                }
                if header < self.pr.num_pages() {
                    self.claim(header, owner);
                }
                Ok(None)
            }
        }
    }

    fn check_free_list(&mut self, header: u32) -> Result<()> {
        let owner = self.owner("the free-list header".into());
        self.claim(header, owner);
        let (list, problems) = match FreeList::load_lossy(self.pr, self.fi, header) {
            Ok(x) => x,
            Err(e) => {
                let v = damage(e, || Location::Page(header))?;
                self.out.push(v);
                return Ok(());
            }
        };
        let mut listed = Listed::default();
        for e in problems {
            listed.damage(e, || Location::Page(header))?;
        }
        let pages = self.owner("a free-list page".into());
        let prefix = list.slots.iter().take_while(|&&s| s != 0).count();
        for (i, &s) in list.slots.iter().enumerate().filter(|(_, s)| **s != 0) {
            if s < self.pr.num_pages() {
                self.claim(s, pages);
            } else if i >= prefix {
                listed.add(
                    Location::Page(header),
                    format!("list page {s} in slot {i} is out of range"),
                );
            }
        }
        for &p in &list.free_pages {
            self.claim(p, FREE_PAGES);
        }
        listed.flush(&mut self.out, Location::Page(header), "free-list problems");
        Ok(())
    }

    fn check_root(&mut self, catalog: &Catalog) -> Result<()> {
        let root = &catalog.root;
        let owner = self.owner("the root file".into());
        self.claim_file(root, owner);
        self.check_versions(root);
        let failed: HashSet<u32> = catalog.errors.iter().map(|(r, _)| *r).collect();
        let refs: Vec<u32> = catalog
            .entries
            .iter()
            .map(|e| e.descr_ref)
            .chain(catalog.errors.iter().map(|(r, _)| *r))
            .collect();
        match self.fi.format {
            Format::V838 => self.check_root_store(root, &refs, &failed),
            Format::V8214 => {
                for r in refs {
                    let name = format!("the description file {r}");
                    self.open_file(r, name, !failed.contains(&r))?;
                }
                Ok(())
            }
        }
    }

    fn check_root_store(
        &mut self,
        root: &IFile,
        refs: &[u32],
        failed: &HashSet<u32>,
    ) -> Result<()> {
        let store = match BlobStore::new(root.clone()) {
            Ok(s) => s,
            Err(e) => {
                let v = damage(e, || Location::Root)?;
                self.out.push(v);
                return Ok(());
            }
        };
        let mut listed = Listed::default();
        let mut blocks = Blocks::new(store.nblocks());
        match store.free_blocks(self.pr) {
            Ok(chain) => {
                blocks.claim(&chain, BlockUse::Free);
            }
            Err(e) => listed.damage(e, || Location::Root)?,
        }
        let chains = std::iter::once((ROOT_CONTENT_BLOCK, BlockUse::Content))
            .chain(refs.iter().map(|&r| (r, BlockUse::Descr(r))));
        for (start, user) in chains {
            let chain = match store.chain(self.pr, start) {
                Ok(c) => c,
                Err(e) if e.is_fatal() => return Err(e),
                Err(_) if matches!(user, BlockUse::Descr(r) if failed.contains(&r)) => continue,
                Err(e) => {
                    listed.damage(e, || Location::Root)?;
                    continue;
                }
            };
            for (prev, list) in blocks.claim(&chain, user) {
                listed.add(
                    Location::Root,
                    format!(
                        "{} shares root {} with {}",
                        user.describe(None),
                        fmt_blocks(&list),
                        prev.describe(None)
                    ),
                );
            }
        }
        for b in blocks.unused() {
            listed.add_kind(
                Kind::Unused,
                Location::Root,
                format!("root block {b} is neither used nor free"),
            );
        }
        listed.flush(&mut self.out, Location::Root, "root block problems");
        Ok(())
    }

    fn check_descriptions(&mut self, catalog: &Catalog) {
        for (r, e) in &catalog.errors {
            self.out.push(match e {
                Error::Corrupt { location, what } => Violation {
                    location: location.clone(),
                    what: what.clone(),
                },
                other => Violation {
                    location: Location::Root,
                    what: format!("description {r}: {other}"),
                },
            });
        }
    }

    fn check_table(&mut self, entry: &CatalogEntry, keys: KeyCtx) -> Result<()> {
        self.tables += 1;
        let d = &*entry.descr;
        let name = d.name.clone();
        let table = || Location::Table(name.clone());
        let [data_h, blob_h, index_h] = d.files;
        let data = self.open_file(data_h, format!("the data file of {name}"), true)?;
        let blob = self.open_file(blob_h, format!("the blob file of {name}"), true)?;
        let index = self.open_file(index_h, format!("the index file of {name}"), true)?;
        let mut sound = data.is_some() == (data_h != 0) && blob.is_some() == (blob_h != 0);
        if let Some(f) = &data {
            let record_len = u64::from(entry.layout.record_len.max(1));
            if !f.length.is_multiple_of(record_len) {
                self.out.push(Violation {
                    location: table(),
                    what: format!(
                        "data length {} is not a multiple of the record length {record_len}",
                        f.length
                    ),
                });
                sound = false;
            }
        }
        let store = match blob.map(BlobStore::new) {
            Some(Ok(s)) => Some(s.named(&name)),
            Some(Err(e)) => {
                let v = damage(e, table)?;
                self.out.push(v);
                sound = false;
                None
            }
            None => None,
        };
        let ps = u64::from(self.fi.page_size);
        let index = match index {
            Some(f) if !f.length.is_multiple_of(ps) => {
                self.out.push(Violation {
                    location: table(),
                    what: format!(
                        "index file length {} is not a multiple of the page size",
                        f.length
                    ),
                });
                None
            }
            other => other,
        };
        if !sound {
            return Ok(());
        }
        let t = match TableRead::open(self.pr, self.fi, entry, keys) {
            Ok(t) => t,
            Err(e) => {
                let v = damage(e, table)?;
                self.out.push(v);
                return Ok(());
            }
        };
        let live = self.check_records(&t)?;
        self.check_blobs(&t, store.as_ref(), &live)?;
        self.check_indexes(&t, index, &live, keys)
    }

    fn check_records(&mut self, t: &TableRead) -> Result<Vec<bool>> {
        let name = t.descr().name.clone();
        let at = |row: u32| Location::Record {
            table: name.clone(),
            row,
        };
        let n = t.num_slots();
        let mut listed = Listed::default();
        let mut flags: Vec<Option<u8>> = Vec::with_capacity(n as usize);
        for row in 0..n {
            match t.raw_record(row) {
                Ok(rec) => flags.push(rec.first().copied()),
                Err(e) => {
                    listed.damage(e, || at(row))?;
                    flags.push(None);
                }
            }
        }
        if let Some(Some(f)) = flags.first()
            && *f != FLAG_FREE
        {
            listed.add(
                at(0),
                format!("slot 0 has flag {f}; as the head of the free list it has {FLAG_FREE}"),
            );
        }
        let mut in_chain = vec![false; n as usize];
        let mut cur = 0u32;
        while cur < n {
            let Some(next) = unless_fatal(t.raw_record(cur))?.and_then(|r| free_next(&r)) else {
                break;
            };
            if next == 0 {
                break;
            }
            let Some(seen) = in_chain.get_mut(next as usize) else {
                listed.add(
                    at(cur),
                    format!("free record link {next} is beyond the {n} slots"),
                );
                break;
            };
            if *seen {
                listed.add(
                    at(cur),
                    format!("free record chain has a cycle at slot {next}"),
                );
                break;
            }
            *seen = true;
            match flags.get(next as usize).copied().flatten() {
                Some(FLAG_FREE) => cur = next,
                Some(f) => {
                    listed.add(
                        at(next),
                        format!("record {next} is in the free chain but has flag {f}"),
                    );
                    break;
                }
                None => break,
            }
        }
        let mut live = vec![false; n as usize];
        for (row, (flag, chained)) in flags.iter().zip(&in_chain).enumerate().skip(1) {
            match flag {
                Some(FLAG_USED) => {
                    if let Some(l) = live.get_mut(row) {
                        *l = true;
                    }
                }
                Some(f) if !chained => listed.add_kind(
                    Kind::Unused,
                    at(row as u32),
                    format!("record {row} is free (flag {f}) but not in the free chain"),
                ),
                _ => {}
            }
        }
        let count = live.iter().filter(|&&l| l).count() as u64;
        self.live_rows += count;
        self.table_rows.push((name.clone(), count));
        listed.flush(&mut self.out, Location::Table(name), "record problems");
        Ok(live)
    }

    fn check_blobs(
        &mut self,
        t: &TableRead,
        store: Option<&BlobStore>,
        live: &[bool],
    ) -> Result<()> {
        let d = t.descr();
        let l = t.layout();
        let name = &d.name;
        let fields: Vec<usize> = d
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.ty.is_blob())
            .map(|(i, _)| i)
            .collect();
        let mut listed = Listed::default();
        let mut blocks = Blocks::new(store.map_or(0, BlobStore::nblocks));
        let table = || Location::Table(name.clone());
        if let Some(s) = store {
            match s.free_blocks(self.pr) {
                Ok(chain) => {
                    blocks.claim(&chain, BlockUse::Free);
                }
                Err(e) => listed.damage(e, table)?,
            }
        }
        let rows = live
            .iter()
            .enumerate()
            .filter(|(_, l)| **l)
            .filter_map(|(row, _)| u32::try_from(row).ok());
        for row in rows.filter(|_| !fields.is_empty()) {
            let record = || Location::Record {
                table: name.clone(),
                row,
            };
            let rec = match t.raw_record(row) {
                Ok(r) => r,
                Err(e) => {
                    listed.damage(e, record)?;
                    continue;
                }
            };
            for &i in &fields {
                let fname = d.fields.get(i).map_or("?", |f| f.name.as_str());
                let r = match decode_field(d, l, i, &rec) {
                    Ok(Value::Null) => continue,
                    Ok(
                        Value::Text(BlobVal::Ref(r))
                        | Value::NText(BlobVal::Ref(r))
                        | Value::Image(BlobVal::Ref(r)),
                    ) => r,
                    Ok(_) => {
                        listed.add(
                            record(),
                            format!("field {fname} does not hold a blob reference"),
                        );
                        continue;
                    }
                    Err(e) => {
                        listed.damage(e, record)?;
                        continue;
                    }
                };
                if r.start == 0 && r.len == 0 {
                    continue;
                }
                let Some(s) = store else {
                    listed.add(
                        table(),
                        format!(
                            "record {row} field {fname} refers to block {} but the table has no blob file",
                            r.start
                        ),
                    );
                    continue;
                };
                if r.len == 0 {
                    listed.add(
                        Location::Blob {
                            table: name.clone(),
                            block: r.start,
                        },
                        format!(
                            "record {row} field {fname}: an empty value starts at block {}",
                            r.start
                        ),
                    );
                    continue;
                }
                let chain = match s.value_blocks(self.pr, r) {
                    Ok(v) => v.into_iter().map(|(b, _)| b).collect(),
                    Err(e) => {
                        let v = damage(e, record)?;
                        listed.add(
                            v.location,
                            format!("record {row} field {fname}: {}", v.what),
                        );
                        unless_fatal(s.chain(self.pr, r.start))?.unwrap_or_default()
                    }
                };
                let user = BlockUse::Value {
                    row,
                    field: u16::try_from(i).unwrap_or(u16::MAX),
                };
                for (prev, list) in blocks.claim(&chain, user) {
                    let block = list.first().copied().unwrap_or(r.start);
                    listed.add(
                        Location::Blob {
                            table: name.clone(),
                            block,
                        },
                        format!(
                            "{} shares {} with {}",
                            user.describe(Some(d)),
                            fmt_blocks(&list),
                            prev.describe(Some(d))
                        ),
                    );
                }
            }
        }
        for b in blocks.unused() {
            listed.add_kind(
                Kind::Unused,
                Location::Blob {
                    table: name.clone(),
                    block: b,
                },
                format!("block {b} is neither used by a value nor free"),
            );
        }
        listed.flush(&mut self.out, table(), "blob problems");
        Ok(())
    }

    fn index_page(&self, file: &IFile, no: u32) -> Result<PageBuf> {
        let page = file.data_pages.get(no as usize).ok_or_else(|| {
            Error::corrupt(
                Location::InternalFile(file.header_page),
                format!("page {no} is beyond the index file"),
            )
        })?;
        self.pr.read_page(*page)
    }

    fn check_indexes(
        &mut self,
        t: &TableRead,
        file: Option<IFile>,
        live: &[bool],
        keys: KeyCtx,
    ) -> Result<()> {
        let d = t.descr();
        let name = d.name.clone();
        let table = || Location::Table(name.clone());
        let Some(file) = file else {
            if !d.indexes.is_empty() && d.files[2] == 0 {
                self.out.push(Violation {
                    location: table(),
                    what: format!(
                        "{} indexes are described but the table has no index file",
                        d.indexes.len()
                    ),
                });
            }
            return Ok(());
        };
        let npages = u32::try_from(file.length / u64::from(self.fi.page_size)).unwrap_or(u32::MAX);
        if npages == 0 {
            self.out.push(Violation {
                location: table(),
                what: "the index file is empty".into(),
            });
            return Ok(());
        }
        let header = match self
            .index_page(&file, 0)
            .and_then(|p| parse_index_file_header(&p, d.indexes.len()))
        {
            Ok(h) => h,
            Err(e) => {
                let v = damage(e, table)?;
                let v = match v.location {
                    Location::File => Violation {
                        location: table(),
                        what: format!("index file header: {}", v.what),
                    },
                    _ => v,
                };
                self.out.push(v);
                return Ok(());
            }
        };
        let mut pages = IndexPages {
            users: vec![None; npages as usize],
        };
        pages.claim(0, IxUse::Header);
        let mut listed = Listed::default();

        let mut cur = header.free_head;
        while cur != 0 {
            if cur >= npages {
                listed.add(
                    table(),
                    format!("index file: free page link {cur} is beyond the {npages} pages"),
                );
                break;
            }
            if let Some(prev) = pages.claim(cur, IxUse::Free) {
                listed.add(
                    table(),
                    if prev == IxUse::Free {
                        format!("index file: the free page chain has a cycle at page {cur}")
                    } else {
                        format!("index file page {cur} is free and {}", prev.describe(d))
                    },
                );
                break;
            }
            match self.index_page(&file, cur) {
                Ok(p) => cur = u32le(&p, 0).unwrap_or(0),
                Err(e) => {
                    listed.damage(e, table)?;
                    break;
                }
            }
        }

        let collate = Watched {
            inner: keys.collate,
            failure: Mutex::new(None),
        };
        let tx = TableIx {
            t,
            file: &file,
            live,
            collate: &collate,
            locale: keys.locale,
        };
        for (pos, ptr) in header.descr_ptrs.iter().enumerate() {
            self.check_index(&tx, pos, *ptr, &mut pages)?;
        }
        for p in pages.unused() {
            listed.add_kind(
                Kind::Unused,
                table(),
                format!("index file page {p} is not used"),
            );
        }
        listed.flush(&mut self.out, table(), "index file problems");
        Ok(())
    }

    fn check_index(
        &mut self,
        tx: &TableIx,
        pos: usize,
        descr_ptr: u32,
        pages: &mut IndexPages,
    ) -> Result<()> {
        let Some(idx) = tx.t.descr().indexes.get(pos) else {
            return Ok(());
        };
        let mut listed = Listed::default();
        self.index_tree(tx, pos, idx, descr_ptr, pages, &mut listed)?;
        let at = Location::Index {
            table: tx.t.descr().name.clone(),
            index: idx.name.clone(),
            page: None,
        };
        listed.flush(&mut self.out, at, "problems of the index");
        Ok(())
    }

    fn index_tree(
        &mut self,
        tx: &TableIx,
        pos: usize,
        idx: &IndexDescr,
        descr_ptr: u32,
        pages: &mut IndexPages,
        listed: &mut Listed,
    ) -> Result<()> {
        let d = tx.t.descr();
        let at = |page| Location::Index {
            table: d.name.clone(),
            index: idx.name.clone(),
            page,
        };
        let relocate = |e: Error, page: u32| -> Result<Violation> {
            let v = damage(e, || at(Some(page)))?;
            Ok(match v.location {
                Location::File => Violation {
                    location: at(Some(page)),
                    what: v.what,
                },
                _ => v,
            })
        };

        let npages = u32::try_from(pages.users.len()).unwrap_or(u32::MAX);
        let ps = u64::from(self.fi.page_size);
        let unit = self.fi.index_ptr_unit();
        let Some(dp) = page_number(descr_ptr, unit, ps, npages) else {
            listed.add(
                at(Some(0)),
                format!("descriptor pointer {descr_ptr:#x} does not refer to a page"),
            );
            return Ok(());
        };
        if let Some(prev) = pages.claim(dp, IxUse::Descriptor(pos)) {
            listed.add(
                at(Some(dp)),
                format!("descriptor page {dp} is also {}", prev.describe(d)),
            );
            return Ok(());
        }
        let descriptor = match self
            .index_page(tx.file, dp)
            .and_then(|p| parse_descriptor(&p))
        {
            Ok(x) => x,
            Err(e) => {
                listed.push(relocate(e, dp)?);
                return Ok(());
            }
        };
        if descriptor.flag != DESCRIPTOR_FLAG {
            listed.add(
                at(Some(dp)),
                format!(
                    "descriptor flag {:#04x}, {DESCRIPTOR_FLAG:#04x} expected",
                    descriptor.flag
                ),
            );
        }
        let key_len = u32::from(descriptor.key_len);
        let compare = match index_key_len(d, idx) {
            Ok(want) if want == key_len => true,
            Ok(want) => {
                listed.add(
                    at(Some(dp)),
                    format!(
                        "descriptor key length {key_len} differs from the {want} of the description"
                    ),
                );
                false
            }
            Err(e) => {
                listed.add(at(None), format!("the key length cannot be computed: {e}"));
                false
            }
        };
        let Some(root) = page_number(descriptor.root, unit, ps, npages) else {
            listed.add(
                at(Some(dp)),
                format!(
                    "root pointer {:#x} does not refer to a page",
                    descriptor.root
                ),
            );
            return Ok(());
        };
        let ix = match IndexRead::from_parts(tx.t, idx, tx.file.clone(), key_len) {
            Ok(ix) => ix.with_root(root),
            Err(e) => {
                listed.push(relocate(e, dp)?);
                return Ok(());
            }
        };
        let (levels, problems) = tree_levels(&ix)?;
        for e in problems {
            listed.damage(e, || at(None))?;
        }
        for (no, _) in levels.iter().flatten() {
            if let Some(prev) = pages.claim(*no, IxUse::Tree(pos)) {
                listed.add(
                    at(Some(*no)),
                    format!("page {no} of the tree is also {}", prev.describe(d)),
                );
            }
        }
        let cx = IndexCx {
            t: tx.t,
            ix: &ix,
            idx,
            key_len,
            live: tx.live,
            compare,
            collate: tx.collate,
            locale: tx.locale,
        };
        self.check_tree(&cx, &levels, listed)
    }

    fn check_tree(&mut self, cx: &IndexCx, levels: &[Level], listed: &mut Listed) -> Result<()> {
        let ix = cx.ix;
        let root = ix.root_page();
        let nleaves = levels
            .iter()
            .flatten()
            .filter(|(_, n)| matches!(n, TreeNode::Leaf))
            .count();
        let mut st = Entries {
            seen: vec![false; cx.live.len()],
            prev: None,
            strings_failed: false,
        };
        let mut separators: HashMap<u32, Separator> = HashMap::new();
        for (no, node) in levels.iter().flatten() {
            let TreeNode::Branch(b) = node else { continue };
            for e in &b.entries {
                if let Ok(child) = ix.page_of(e.child, *no) {
                    separators.entry(child).or_insert(Separator {
                        parent: *no,
                        key: &e.key,
                        row: e.row,
                    });
                }
            }
        }
        let close = |no: u32, last: Option<(&[u8], u32)>, listed: &mut Listed| {
            let Some(sep) = separators.get(&no) else {
                return;
            };
            if last == Some((sep.key, sep.row)) {
                return;
            }
            listed.add(
                cx.at(Some(sep.parent)),
                match last {
                    Some((_, r)) => format!(
                        "separator of row {} does not close its child page {no}, whose last entry is row {r}",
                        sep.row
                    ),
                    None => format!(
                        "separator of row {} points at page {no}, which is empty or unreadable",
                        sep.row
                    ),
                },
            );
        };
        for level in levels {
            for (i, (no, node)) in level.iter().enumerate() {
                let no = *no;
                let before = i.checked_sub(1).and_then(|j| level.get(j)).map(|x| x.0);
                let after = level.get(i + 1).map(|x| x.0);
                let header = |is_root: bool, prev: u32, next: u32, listed: &mut Listed| {
                    if is_root != (no == root) {
                        listed.add(
                            cx.at(Some(no)),
                            if is_root {
                                "root flag on a page below the root"
                            } else {
                                "the root page has no root flag"
                            },
                        );
                    }
                    let linked = link_ok(ix, no, prev, before) && link_ok(ix, no, next, after);
                    if !linked {
                        listed.add(
                            cx.at(Some(no)),
                            format!(
                                "links prev {} / next {} do not match the pages beside it on its level ({} / {})",
                                ptr_text(prev),
                                ptr_text(next),
                                page_text(before),
                                page_text(after)
                            ),
                        );
                    }
                };
                match node {
                    TreeNode::Branch(b) => {
                        header(b.root, b.prev, b.next, listed);
                        if b.entries.is_empty() {
                            listed.add(cx.at(Some(no)), "a branch has no entries");
                        }
                        close(no, branch_last(b), listed);
                    }
                    TreeNode::Leaf => {
                        let page = match ix.decode(no) {
                            Ok(p) => p,
                            Err(e) => {
                                listed.damage(e, || cx.at(Some(no)))?;
                                close(no, None, listed);
                                continue;
                            }
                        };
                        let sole_empty = nleaves == 1
                            && matches!(&page, IndexPage::Leaf(l) if l.entries.is_empty());
                        if !sole_empty {
                            close(no, last_pair(&page), listed);
                        }
                        let IndexPage::Leaf(leaf) = page else {
                            listed.add(cx.at(Some(no)), "a leaf was expected");
                            continue;
                        };
                        if sole_empty && no != root {
                            listed.add(
                                cx.at(Some(no)),
                                "the only leaf of the tree is empty under branches",
                            );
                        }
                        header(leaf.root, leaf.prev, leaf.next, listed);
                        if leaf.entries.is_empty() && nleaves > 1 {
                            listed.add(
                                cx.at(Some(no)),
                                format!("an empty leaf in a tree of {nleaves} leaves"),
                            );
                        }
                        let p = leaf.packing;
                        if !packing_ok(p, cx.key_len) {
                            listed.add(
                                cx.at(Some(no)),
                                format!(
                                    "leaf packing {}/{}/{} in {} bytes does not suit the key length {}",
                                    p.nb, p.lb, p.rb, p.recbytes, cx.key_len
                                ),
                            );
                        }
                        self.leaf_entries(cx, &mut st, no, leaf, listed)?;
                    }
                }
            }
        }

        for (row, (&l, &s)) in cx.live.iter().zip(&st.seen).enumerate() {
            if l && !s {
                listed.add_kind(Kind::Rows, cx.at(None), format!("row {row} not indexed"));
            }
        }
        Ok(())
    }

    fn leaf_entries(
        &mut self,
        cx: &IndexCx,
        st: &mut Entries,
        no: u32,
        leaf: LeafPage,
        listed: &mut Listed,
    ) -> Result<()> {
        for (i, (row, key)) in leaf.entries.iter().enumerate() {
            self.index_entries += 1;
            let prev = match i.checked_sub(1) {
                None => st.prev.as_ref().map(|(k, r)| (k.as_slice(), *r)),
                Some(j) => leaf.entries.get(j).map(|(r, k)| (k.as_slice(), *r)),
            };
            if let Some((pk, pr)) = prev {
                if (key.as_slice(), *row) <= (pk, pr) {
                    listed.add_kind(
                        Kind::Order,
                        cx.at(Some(no)),
                        format!("entry of row {row} is out of (key, row) order after row {pr}"),
                    );
                }
                if cx.idx.unique && key.as_slice() == pk {
                    listed.add_kind(
                        Kind::Order,
                        cx.at(Some(no)),
                        format!("rows {pr} and {row} have the same key in a unique index"),
                    );
                }
            }
            let r = *row as usize;
            if !cx.live.get(r).copied().unwrap_or(false) {
                listed.add_kind(
                    Kind::Rows,
                    cx.at(Some(no)),
                    format!("entry refers to row {row}, which is not a live record"),
                );
                continue;
            }
            match st.seen.get_mut(r) {
                Some(s) if *s => {
                    listed.add_kind(
                        Kind::Rows,
                        cx.at(Some(no)),
                        format!("row {row} is indexed twice"),
                    );
                    continue;
                }
                Some(s) => *s = true,
                None => continue,
            }
            if !cx.compare {
                continue;
            }
            let rec = match cx.t.raw_record(*row) {
                Ok(r) => r,
                Err(e) => {
                    listed.damage(e, || cx.at(Some(no)))?;
                    continue;
                }
            };
            self.compare_key(cx, st, no, *row, key, &rec, listed)?;
        }
        if let Some((r, k)) = leaf.entries.into_iter().last() {
            st.prev = Some((k, r));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn compare_key(
        &mut self,
        cx: &IndexCx,
        st: &mut Entries,
        no: u32,
        row: u32,
        key: &[u8],
        rec: &[u8],
        listed: &mut Listed,
    ) -> Result<()> {
        let d = cx.t.descr();
        let l = cx.t.layout();
        let at_row = |e: Error| -> Result<Violation> {
            let v = damage(e, || cx.at(Some(no)))?;
            Ok(Violation {
                location: cx.at(Some(no)),
                what: format!("row {row}: {}", v.what),
            })
        };
        let keys = KeyCtx {
            collate: Some(cx.collate),
            locale: cx.locale,
        };
        match build_key(d, l, cx.idx, rec, cx.key_len, &keys) {
            Ok(built) => {
                self.keys_checked += 1;
                if has_text(d, l, cx.idx, rec).unwrap_or(false) {
                    self.string_keys_checked += 1;
                }
                if built != key {
                    listed.add_kind(
                        Kind::Keys,
                        cx.at(Some(no)),
                        format!(
                            "row {row}: key mismatch: stored {}, built {}",
                            hex(key),
                            hex(&built)
                        ),
                    );
                }
                return Ok(());
            }
            Err(e) => match cx.collate.take_failure() {
                Some(why) => {
                    if !st.strings_failed {
                        st.strings_failed = true;
                        listed.add_kind(
                            Kind::Keys,
                            cx.at(None),
                            format!("string parts not checked: {why}"),
                        );
                    }
                }
                None => {
                    listed.push_kind(Kind::Keys, at_row(e)?);
                    return Ok(());
                }
            },
        }
        let blank = KeyCtx {
            collate: Some(&Blank),
            locale: cx.locale,
        };
        let parts = build_key(d, l, cx.idx, rec, cx.key_len, &blank).and_then(|built| {
            Ok((
                split_key(d, cx.idx, key)?,
                split_key(d, cx.idx, &built)?,
                built,
            ))
        });
        let (stored, expected, built) = match parts {
            Ok(x) => x,
            Err(e) => {
                listed.push_kind(Kind::Keys, at_row(e)?);
                return Ok(());
            }
        };
        for ((fld, s), b) in cx.idx.fields.iter().zip(stored).zip(expected) {
            let is_string = d.field(&fld.name).is_some_and(|(_, f)| f.ty.is_string());
            if !is_string && key.get(s) != built.get(b) {
                listed.add_kind(
                    Kind::Keys,
                    cx.at(Some(no)),
                    format!("row {row}: key mismatch in field {}", fld.name),
                );
            }
        }
        Ok(())
    }
}
