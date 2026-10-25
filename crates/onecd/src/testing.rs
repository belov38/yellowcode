#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;

use crate::collate::Collate;
use crate::db::{Database, OpenOptions};
use crate::error::{Error, NotFound, Result};
use crate::format::Format;
use crate::format::blob::{BLOCK, BLOCK_DATA, BlobBlock, encode_block};
use crate::format::descr::TableDescr;
use crate::format::freelist::{FreeListHeader, encode_freelist_header_838};
use crate::format::header::{DbHeader, encode_header};
use crate::format::ifile::encode_ifile_header_838;
use crate::format::key::{KeyCtx, build_key, index_key_len};
use crate::format::layout::RecordLayout;
use crate::format::record::{encode_row, set_free_link};
use crate::format::root::{RootContent, encode_root_838};
use crate::format::value::{BlobRef, BlobVal, Numeric, Row, Value};
use crate::ifile::IFile;
use crate::pager::{MemStorage, Storage};
use crate::table::TableRead;

pub const PS: usize = 4096;
const MAX_FILE_PAGES: usize = 1018;

#[derive(Debug, Default)]
pub struct Image {
    pub pages: Vec<Vec<u8>>,
}

impl Image {
    pub fn new() -> Image {
        Image::default()
    }

    pub fn new_page(&mut self) -> u32 {
        self.pages.push(vec![0u8; PS]);
        u32::try_from(self.pages.len() - 1).unwrap()
    }

    pub fn put_file(&mut self, header_at: Option<u32>, content: &[u8]) -> u32 {
        let header = header_at.unwrap_or_else(|| self.new_page());
        let chunks: Vec<&[u8]> = content.chunks(PS).collect();
        assert!(
            chunks.len() <= MAX_FILE_PAGES,
            "builder supports fatlevel 0 only"
        );
        let mut slots = Vec::new();
        for chunk in chunks {
            let p = self.new_page();
            self.pages[p as usize][..chunk.len()].copy_from_slice(chunk);
            slots.push(p);
        }
        let versions = if content.is_empty() {
            [0, 0, 0]
        } else {
            [1, 1, 0]
        };
        let page = &mut self.pages[header as usize];
        encode_ifile_header_838(0, versions, content.len() as u64, &slots, page).unwrap();
        header
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.pages.concat()
    }
}

#[derive(Debug)]
pub struct BlobBuilder {
    pub bytes: Vec<u8>,
}

impl Default for BlobBuilder {
    fn default() -> BlobBuilder {
        BlobBuilder {
            bytes: vec![0u8; BLOCK],
        }
    }
}

impl BlobBuilder {
    pub fn new() -> BlobBuilder {
        BlobBuilder::default()
    }

    pub fn blocks(&self) -> u32 {
        u32::try_from(self.bytes.len() / BLOCK).unwrap()
    }

    pub fn add(&mut self, data: &[u8]) -> BlobRef {
        if data.is_empty() {
            return BlobRef { start: 0, len: 0 };
        }
        let start = self.blocks();
        let n = data.chunks(BLOCK_DATA).count();
        for (i, chunk) in data.chunks(BLOCK_DATA).enumerate() {
            let mut d = [0u8; BLOCK_DATA];
            d[..chunk.len()].copy_from_slice(chunk);
            let blk = BlobBlock {
                next: if i + 1 < n { start + i as u32 + 1 } else { 0 },
                used: chunk.len() as u16,
                data: d,
            };
            let mut out = [0u8; BLOCK];
            encode_block(&blk, &mut out);
            self.bytes.extend_from_slice(&out);
        }
        BlobRef {
            start,
            len: u32::try_from(data.len()).unwrap(),
        }
    }
}

pub fn live_record(d: &TableDescr, row: &Row) -> Vec<u8> {
    let l = RecordLayout::new(d).unwrap();
    let mut rec = vec![0u8; l.record_len as usize];
    encode_row(d, &l, row, &mut rec).unwrap();
    rec
}

pub fn free_record(d: &TableDescr, next: u32) -> Vec<u8> {
    let l = RecordLayout::new(d).unwrap();
    let mut rec = l.empty_template(d);
    set_free_link(&mut rec, next);
    rec
}

pub fn data_file(d: &TableDescr, head: u32, records: &[Vec<u8>]) -> Vec<u8> {
    let l = RecordLayout::new(d).unwrap();
    let mut slot0 = vec![0u8; l.record_len as usize];
    set_free_link(&mut slot0, head);
    let mut out = slot0;
    for r in records {
        assert_eq!(r.len(), l.record_len as usize);
        out.extend_from_slice(r);
    }
    out
}

#[derive(Debug)]
pub struct TableIn {
    pub descr: TableDescr,
    pub text: Option<Vec<u8>>,
    pub data: Vec<u8>,
    pub blob: Vec<u8>,
    pub index: Vec<u8>,
    pub empty_files: bool,
}

impl TableIn {
    pub fn new(descr: TableDescr) -> TableIn {
        TableIn {
            descr,
            text: None,
            data: Vec::new(),
            blob: Vec::new(),
            index: Vec::new(),
            empty_files: false,
        }
    }

    fn wanted_when_empty(&self) -> [bool; 3] {
        let d = &self.descr;
        let e = self.empty_files;
        [
            e,
            e && d.fields.iter().any(|f| f.ty.is_blob()),
            e && !d.indexes.is_empty(),
        ]
    }
}

#[derive(Debug)]
pub struct Built {
    pub bytes: Vec<u8>,
    pub files: Vec<[u32; 3]>,
}

impl Built {
    pub fn file_page_off(&self, hdr: u32, i: usize) -> usize {
        let at = hdr as usize * PS + 24 + 4 * i;
        let p = u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap());
        p as usize * PS
    }
}

pub fn build_db(locale: &str, tables: &[TableIn]) -> Built {
    let mut img = Image::new();
    for _ in 0..3 {
        img.new_page();
    }
    let mut all_files = Vec::new();
    let mut texts = Vec::new();
    for t in tables {
        let mut files = [0u32; 3];
        let contents = [&t.data, &t.blob, &t.index];
        for ((slot, content), wanted) in files.iter_mut().zip(contents).zip(t.wanted_when_empty()) {
            if !content.is_empty() || wanted {
                *slot = img.put_file(None, content);
            }
        }
        let mut d = t.descr.clone();
        d.files = files;
        texts.push(t.text.clone().unwrap_or_else(|| d.to_text().into_bytes()));
        all_files.push(files);
    }

    let blocks = |len: usize| len.div_ceil(BLOCK_DATA).max(1) as u32;
    let root_len = 36 + 4 * texts.len();
    let mut next = 1 + blocks(root_len);
    let mut refs = Vec::new();
    for t in &texts {
        refs.push(if t.is_empty() { 0 } else { next });
        next += if t.is_empty() { 0 } else { blocks(t.len()) };
    }
    let content = encode_root_838(&RootContent {
        locale: locale.to_string(),
        table_refs: refs.clone(),
    });
    let mut store = BlobBuilder::new();
    assert_eq!(store.add(&content).start, 1);
    for (t, r) in texts.iter().zip(&refs) {
        assert_eq!(store.add(t).start, *r);
    }
    img.put_file(Some(2), &store.bytes);

    let pages = u32::try_from(img.pages.len()).unwrap();
    encode_header(
        &DbHeader {
            version: [8, 3, 8, 0],
            format: Format::V838,
            length_pages: pages,
            freelist_page: 1,
            page_size: PS as u32,
        },
        &mut img.pages[0],
    )
    .unwrap();
    let fl = FreeListHeader {
        count: 0,
        counter: 0,
        slots: Vec::new(),
    };
    encode_freelist_header_838(&fl, &mut img.pages[1]).unwrap();
    Built {
        bytes: img.into_bytes(),
        files: all_files,
    }
}

pub fn sample_descr() -> TableDescr {
    TableDescr::parse(
        "{\"T\",0,{\"Fields\",{\"A\",\"N\",0,5,0,\"CS\"},{\"B\",\"NT\",1,0,0,\"CS\"}},{\"Indexes\"},{\"Recordlock\",\"0\"},{\"Files\",0,0,0}}",
    )
    .unwrap()
}

pub fn sample_blob() -> Vec<u8> {
    (0..300).map(|i| (i * 7 % 251) as u8).collect()
}

pub fn stored(m: &MemStorage) -> Vec<u8> {
    let mut b = vec![0u8; usize::try_from(m.len().unwrap()).unwrap()];
    m.read_at(0, &mut b).unwrap();
    b
}

pub fn assert_clean(db: &Database, m: &MemStorage) {
    let r = db.verify().unwrap();
    assert!(r.violations.is_empty(), "{:#?}", r.violations);
    let copy = Arc::new(MemStorage::new(stored(m)));
    let again = Database::open_storage(copy, OpenOptions::read_only()).unwrap();
    let r = again.verify().unwrap();
    assert!(r.violations.is_empty(), "reopened: {:#?}", r.violations);
}

pub fn sample_db() -> Built {
    let d = sample_descr();
    let mut blob = BlobBuilder::new();
    let r = blob.add(&sample_blob());
    let row = vec![
        Value::Numeric(Numeric::from_i64(7)),
        Value::NText(BlobVal::Ref(r)),
    ];
    let mut t = TableIn::new(d.clone());
    t.data = data_file(&d, 2, &[live_record(&d, &row), free_record(&d, 0)]);
    t.blob = blob.bytes;
    build_db("ru_RU", &[t])
}

pub fn record_key(t: &TableRead<'_>, index: &str, rec: &[u8]) -> Result<Vec<u8>> {
    let d = t.descr();
    let (_, idx) = d
        .index(index)
        .ok_or_else(|| Error::NotFound(NotFound::Index(index.to_string())))?;
    let key_len = index_key_len(d, idx)?;
    build_key(d, t.layout(), idx, rec, key_len, &t.key_ctx())
}

pub fn descr_key(
    d: &TableDescr,
    index: &str,
    rec: &[u8],
    collate: Option<&dyn Collate>,
    locale: &str,
) -> Result<Vec<u8>> {
    let (_, idx) = d
        .index(index)
        .ok_or_else(|| Error::NotFound(NotFound::Index(index.to_string())))?;
    let key_len = index_key_len(d, idx)?;
    let layout = RecordLayout::new(d)?;
    build_key(d, &layout, idx, rec, key_len, &KeyCtx { collate, locale })
}

pub fn table_file_pages(t: &TableRead<'_>) -> Result<Vec<Vec<u32>>> {
    let fi = t.format_info();
    (t.descr().files.iter().filter(|&&h| h != 0))
        .map(|&h| Ok(IFile::open(t.pager(), fi, h)?.owned_pages().collect()))
        .collect()
}
