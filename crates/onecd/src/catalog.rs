use std::collections::HashSet;
use std::sync::Arc;

use crate::blobstore::BlobStore;
use crate::error::{Constraint, Error, Location, Result};
use crate::format::brace::bad_descr;
use crate::format::descr::{FieldDescr, FieldType, TableDescr, names_eq};
use crate::format::key::index_key_len;
use crate::format::layout::RecordLayout;
use crate::format::root::parse_root;
use crate::format::{Format, FormatInfo};
use crate::ifile::IFile;
use crate::pager::PageRead;
use crate::read_buffer::ReadBytes;

#[derive(Debug)]
pub(crate) enum Metadata<T> {
    Legacy(Arc<T>),
    Managed(memory_budget::Shared<T>),
}
impl<T> Metadata<T> {
    pub(crate) fn new(value: T) -> Self {
        Self::Legacy(Arc::new(value))
    }
    pub(crate) fn try_new(value: T, budget: Option<&memory_budget::MemoryBudget>) -> Result<Self> {
        match budget {
            Some(budget) => {
                let lease = budget
                    .try_reserve(memory_budget::Category::Metadata, 0)
                    .map_err(memory_budget::BudgetError::from)?;
                Ok(Self::Managed(memory_budget::Shared::try_new(value, lease)?))
            }
            None => Ok(Self::new(value)),
        }
    }
    pub(crate) fn ptr_eq(a: &Self, b: &Self) -> bool {
        match (a, b) {
            (Self::Legacy(a), Self::Legacy(b)) => Arc::ptr_eq(a, b),
            (Self::Managed(a), Self::Managed(b)) => memory_budget::Shared::ptr_eq(a, b),
            _ => false,
        }
    }
}
impl<T: Clone> Metadata<T> {
    pub(crate) fn unwrap_or_clone(self) -> Result<T> {
        match self {
            Self::Legacy(v) => Ok(Arc::unwrap_or_clone(v)),
            Self::Managed(_) => Err(bad_descr("managed metadata is read-only")),
        }
    }
    pub(crate) fn make_mut(this: &mut Self) -> Result<&mut T> {
        match this {
            Self::Legacy(v) => Ok(Arc::make_mut(v)),
            Self::Managed(_) => Err(bad_descr("managed metadata is read-only")),
        }
    }
}
impl<T> Clone for Metadata<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Legacy(v) => Self::Legacy(v.clone()),
            Self::Managed(v) => Self::Managed(v.clone()),
        }
    }
}
impl<T> std::ops::Deref for Metadata<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self {
            Self::Legacy(v) => v,
            Self::Managed(v) => v,
        }
    }
}

pub(crate) const ROOT_PAGE: u32 = 2;
pub(crate) const ROOT_CONTENT_BLOCK: u32 = 1;

#[derive(Debug, Clone)]
pub(crate) struct CatalogEntry {
    pub descr_ref: u32,
    pub descr: Metadata<TableDescr>,
    pub layout: Metadata<RecordLayout>,
}

#[derive(Debug)]
pub(crate) struct Catalog {
    pub locale: String,
    pub root: IFile,
    pub entries: Vec<CatalogEntry>,
    pub errors: Vec<(u32, Error)>,
}

impl Catalog {
    pub fn load(pr: &dyn PageRead, fi: FormatInfo, version: [u8; 4]) -> Result<Catalog> {
        let root = IFile::open(pr, fi, ROOT_PAGE)?;
        let store = match fi.format {
            Format::V838 => Some(BlobStore::new(root.clone())?),
            Format::V8214 => None,
        };
        let content = match &store {
            Some(s) => read_chain(s, pr, ROOT_CONTENT_BLOCK)?,
            None => read_file(&root, pr)?,
        };
        let parsed = parse_root(version, &content)?;

        let mut entries = Vec::with_capacity(parsed.table_refs.len());
        let mut errors = Vec::new();
        for &r in &parsed.table_refs {
            match load_entry(pr, fi, store.as_ref(), r) {
                Ok(e) => entries.push(e),
                Err(e) if e.is_fatal() => return Err(e),
                Err(e) => errors.push((r, e)),
            }
        }
        Ok(Catalog {
            locale: parsed.locale,
            root,
            entries,
            errors,
        })
    }

    pub fn load_shared(
        pr: &dyn PageRead,
        fi: FormatInfo,
        version: [u8; 4],
    ) -> Result<Metadata<Catalog>> {
        use memory_budget::{BudgetedVec, Category};
        let Some(budget) = pr.memory_budget() else {
            return Self::load(pr, fi, version).map(Metadata::new);
        };
        let root = IFile::open(pr, fi, ROOT_PAGE)?;
        let store = match fi.format {
            Format::V838 => Some(BlobStore::new(root.clone())?),
            Format::V8214 => None,
        };
        let content = match &store {
            Some(s) => read_chain(s, pr, ROOT_CONTENT_BLOCK)?,
            None => read_file(&root, pr)?,
        };
        let parsed = crate::format::managed::parse_root(version, &content, budget)?;
        let count = parsed.table_refs.len();
        let mut entries = BudgetedVec::try_with_capacity(budget, Category::Metadata, count)?;
        let mut errors =
            BudgetedVec::try_with_capacity(budget, Category::Metadata, count)?.into_charged_vec();
        for &r in &parsed.table_refs {
            match load_entry(pr, fi, store.as_ref(), r) {
                Ok(entry) => entries.try_push(entry)?,
                Err(e) if e.is_fatal() => return Err(e),
                Err(_) => {
                    let diagnostic =
                        crate::format::managed::copy_string("invalid table description", budget)?
                            .map(|what| Error::Corrupt {
                                location: Location::Root,
                                what,
                            });
                    errors = errors.try_zip(diagnostic)?.map(|(mut errors, error)| {
                        errors.push((r, error));
                        errors
                    });
                }
            }
        }
        let result = parsed
            .try_zip(entries.into_charged_vec())?
            .try_zip(errors)?
            .map(|((parsed, entries), errors)| Catalog {
                locale: parsed.locale,
                root,
                entries,
                errors,
            });
        Ok(Metadata::Managed(result.try_into_shared()?))
    }

    pub fn empty() -> Catalog {
        Catalog {
            locale: String::new(),
            root: IFile {
                header_page: ROOT_PAGE,
                fatlevel: 0,
                versions: [0; 3],
                length: 0,
                alloc_pages: Vec::new().into(),
                data_pages: Vec::new().into(),
                owner: None,
            },
            entries: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn find(&self, name: &str) -> Option<&CatalogEntry> {
        self.entries.iter().find(|e| names_eq(&e.descr.name, name))
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| names_eq(&e.descr.name, name))
    }

    pub fn at_named(&self, index: usize, name: &str) -> Option<&CatalogEntry> {
        self.entries
            .get(index)
            .filter(|e| names_eq(&e.descr.name, name))
    }

    pub fn table_names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.descr.name.clone()).collect()
    }

    pub fn add_table(&mut self, descr_ref: u32, descr: TableDescr, layout: RecordLayout) {
        self.entries.push(CatalogEntry {
            descr_ref,
            descr: Metadata::new(descr),
            layout: Metadata::new(layout),
        });
    }

    pub fn remove_table(&mut self, descr_ref: u32) {
        self.entries.retain(|e| e.descr_ref != descr_ref);
    }
}

impl Clone for Catalog {
    fn clone(&self) -> Catalog {
        Catalog {
            locale: self.locale.clone(),
            root: self.root.clone(),
            entries: self.entries.clone(),
            errors: self
                .errors
                .iter()
                .map(|(r, e)| (*r, e.duplicate()))
                .collect(),
        }
    }
}

const MAX_NUMERIC_DIGITS: u32 = 38;

fn is_identifier(s: &str) -> bool {
    let mut b = s.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn is_index_name(s: &str) -> bool {
    let mut b = s.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'@')
}

fn check_field_size(t: &str, f: &FieldDescr) -> Result<()> {
    let (len, prec) = (f.length, f.precision);
    let why = match f.ty {
        FieldType::N if !(1..=MAX_NUMERIC_DIGITS).contains(&len) => {
            format!("length {len} is not within 1..={MAX_NUMERIC_DIGITS}")
        }
        FieldType::N if prec > len => format!("precision {prec} exceeds the length {len}"),
        FieldType::N => return Ok(()),
        FieldType::C | FieldType::NC | FieldType::B if len == 0 => "length 0".into(),
        FieldType::VC | FieldType::NVC | FieldType::VB if !(1..=0xFFFF).contains(&len) => {
            format!("length {len} is not within 1..=65535")
        }
        FieldType::C
        | FieldType::NC
        | FieldType::B
        | FieldType::VC
        | FieldType::NVC
        | FieldType::VB
            if prec != 0 =>
        {
            format!("precision {prec} for a type without one")
        }
        FieldType::T
        | FieldType::NT
        | FieldType::I
        | FieldType::L
        | FieldType::D
        | FieldType::DT
        | FieldType::RV
            if len != 0 || prec != 0 =>
        {
            format!("length {len} and precision {prec} for a type of fixed size (0 and 0)")
        }
        _ => return Ok(()),
    };
    Err(bad_descr(format!(
        "table {t}: field {} of type {}: {why}",
        f.name,
        f.ty.code()
    )))
}

pub(crate) fn check_new_descr(d: &TableDescr) -> Result<RecordLayout> {
    let t = &d.name;
    if !is_identifier(t) {
        return Err(bad_descr(format!(
            "table name {t:?} is not an identifier ([A-Za-z_][A-Za-z0-9_]*)"
        )));
    }
    check_descr_kept_name(d)
}

pub(crate) fn check_descr_kept_name(d: &TableDescr) -> Result<RecordLayout> {
    let t = &d.name;
    let bad = |why: String| Err(bad_descr(format!("table {t}: {why}")));
    if d.fields.is_empty() {
        return bad("no fields".into());
    }
    let mut names = HashSet::new();
    for f in &d.fields {
        if !is_identifier(&f.name) {
            return bad(format!("field name {:?} is not an identifier", f.name));
        }
        if !names.insert(f.name.to_ascii_uppercase()) {
            return bad(format!("field {} is listed twice", f.name));
        }
        check_field_size(t, f)?;
    }
    if d.fields.iter().filter(|f| f.ty == FieldType::RV).count() > 1 {
        return bad("more than one row version field".into());
    }
    let layout = RecordLayout::new(d)?;
    let mut indexes = HashSet::new();
    for ix in &d.indexes {
        if !is_index_name(&ix.name) {
            return bad(format!(
                "index name {:?} is not an identifier ([A-Za-z_][A-Za-z0-9_@]*)",
                ix.name
            ));
        }
        if !indexes.insert(ix.name.to_ascii_uppercase()) {
            return bad(format!("index {} is listed twice", ix.name));
        }
        if ix.fields.is_empty() {
            return bad(format!("index {} has no fields", ix.name));
        }
        let mut in_index = HashSet::new();
        for k in &ix.fields {
            let Some((_, f)) = d.field(&k.name) else {
                return bad(format!(
                    "index {} refers to the unknown field {}",
                    ix.name, k.name
                ));
            };
            if !in_index.insert(f.name.to_ascii_uppercase()) {
                return bad(format!("index {} lists field {} twice", ix.name, f.name));
            }
            let sized = f.ty.is_string() || matches!(f.ty, FieldType::B | FieldType::VB);
            let fits = if sized {
                k.prefix_len <= f.length
            } else {
                k.prefix_len == 0
            };
            if !fits {
                return bad(format!(
                    "index {}: prefix length {} for field {} of type {}({})",
                    ix.name,
                    k.prefix_len,
                    f.name,
                    f.ty.code(),
                    f.length
                ));
            }
        }
        index_key_len(d, ix).map_err(|e| match e {
            Error::Constraint(Constraint::BadDescr(why)) => bad_descr(format!("table {t}: {why}")),
            other => other,
        })?;
    }
    if TableDescr::parse(&d.to_text()).ok().as_ref() != Some(d) {
        return bad("the description text does not read back as the description".into());
    }
    Ok(layout)
}

fn load_entry(
    pr: &dyn PageRead,
    fi: FormatInfo,
    store: Option<&BlobStore>,
    r: u32,
) -> Result<CatalogEntry> {
    let at_root = |what: String| Error::corrupt(Location::Root, format!("description {r}: {what}"));
    let relocate = |e: Error| match e {
        e if pr.memory_budget().is_some() && !e.is_fatal() => Error::Corrupt {
            location: Location::Root,
            what: String::new(),
        },
        Error::Corrupt {
            location: Location::Root,
            what,
        } => at_root(what),
        Error::Corrupt { location, what } => at_root(format!("{what} ({location})")),
        Error::Constraint(Constraint::BadDescr(why)) => at_root(why),
        other => other,
    };
    let text = match store {
        Some(s) => read_chain(s, pr, r)
            .and_then(ReadBytes::into_utf8)
            .map_err(relocate)?,
        None => {
            let raw = IFile::open(pr, fi, r)
                .and_then(|file| read_file(&file, pr))
                .map_err(relocate)?;
            raw.decode_utf16(pr.memory_budget()).map_err(relocate)?
        }
    };
    if let Some(budget) = pr.memory_budget() {
        let descr = crate::format::managed::parse_descr(&text, budget).map_err(relocate)?;
        let layout = crate::format::managed::layout(&descr, budget).map_err(relocate)?;
        return Ok(CatalogEntry {
            descr_ref: r,
            descr: Metadata::Managed(descr.try_into_shared()?),
            layout: Metadata::Managed(layout.try_into_shared()?),
        });
    }
    let descr = TableDescr::parse(&text).map_err(relocate)?;
    let layout = RecordLayout::new(&descr).map_err(relocate)?;
    Ok(CatalogEntry {
        descr_ref: r,
        descr: Metadata::new(descr),
        layout: Metadata::new(layout),
    })
}

fn read_chain(store: &BlobStore, pr: &dyn PageRead, start: u32) -> Result<ReadBytes> {
    match pr.memory_budget() {
        Some(budget) => store
            .read_chain_managed(
                pr,
                start,
                budget,
                memory_budget::Category::Metadata,
                budget.snapshot().admission,
            )
            .map(ReadBytes::Managed),
        None => store.read_chain(pr, start).map(ReadBytes::Legacy),
    }
}
fn read_file(file: &IFile, pr: &dyn PageRead) -> Result<ReadBytes> {
    match pr.memory_budget() {
        Some(budget) => file
            .read_all_managed(pr, budget, memory_budget::Category::Metadata)
            .map(ReadBytes::Managed),
        None => file.read_all(pr).map(ReadBytes::Legacy),
    }
}
