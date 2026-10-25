use crate::format::{FileHeader, Format, PageHeader};
use crate::layout::spec_of;
use crate::{Container, EntrySpec, VecData, is_container, write_exact};

pub mod probe {
    use std::cell::Cell;

    thread_local! {
        static PAGES: Cell<u64> = const { Cell::new(0) };
    }

    pub fn reset() {
        PAGES.with(|p| p.set(0));
    }

    pub fn get() -> u64 {
        PAGES.with(Cell::get)
    }

    #[inline]
    pub(crate) fn hit() {
        PAGES.with(|p| p.set(p.get().wrapping_add(1)));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSpec {
    pub addr: u64,
    pub page_size: u64,
    pub size_field: u64,
    pub next: u64,
    pub payload: Vec<u8>,
}

fn put(img: &mut [u8], at: u64, bytes: &[u8]) {
    let Ok(at) = usize::try_from(at) else { return };
    let Some(dst) = img.get_mut(at..) else { return };
    let n = dst.len().min(bytes.len());
    if let (Some(d), Some(s)) = (dst.get_mut(..n), bytes.get(..n)) {
        d.copy_from_slice(s);
    }
}

pub fn assemble(
    format: Format,
    header: &FileHeader,
    placeholder: &[u8],
    pages: &[PageSpec],
    file_len: u64,
    fill: u8,
) -> Vec<u8> {
    let mut img = vec![fill; usize::try_from(file_len).unwrap_or(0)];
    put(&mut img, 0, placeholder);
    put(&mut img, format.base(), &header.encode(format));
    for p in pages {
        let at = format.base().saturating_add(p.addr);
        let h = PageHeader {
            data_size: p.size_field,
            page_size: p.page_size,
            next_page: p.next,
        };
        if let Some(hb) = h.encode(format) {
            put(&mut img, at, &hb);
        }
        let body = at.saturating_add(format.page_header_size());
        let pad = usize::try_from(p.page_size.max(p.payload.len() as u64)).unwrap_or(0);
        put(&mut img, body, &vec![0; pad]);
        put(&mut img, body, &p.payload);
    }
    img
}

pub fn toc_payload(format: Format, records: &[(u64, Option<u64>)]) -> Vec<u8> {
    let n = usize::try_from(format.addr_size()).unwrap_or(4);
    let mut v = Vec::new();
    for &(h, d) in records {
        for a in [h, d.unwrap_or(format.undefined()), format.undefined()] {
            v.extend_from_slice(a.to_le_bytes().get(..n).unwrap_or_default());
        }
    }
    v
}

#[allow(clippy::panic)]
pub fn exercise(bytes: &[u8]) {
    let Ok(c) = Container::open(bytes) else {
        return;
    };
    let mut data = Vec::with_capacity(c.entries().len());
    for e in c.entries() {
        let _ = std::io::copy(&mut c.data_reader(e), &mut std::io::sink());
        let Ok(d) = c.read_data(e) else { return };
        if let Some(inf) = zraw::try_inflate(&d, 1 << 20) {
            let _ = is_container(&inf);
        }
        data.push(d);
    }
    let Ok(cap) = c.capture_layout() else { return };
    let specs: Vec<EntrySpec> = c.entries().iter().map(spec_of).collect();
    if cap.layout.check(&specs).is_err() {
        return;
    }
    let mut out = Vec::new();
    if let Err(e) = write_exact(
        &mut out,
        &cap.layout,
        &cap.residue,
        cap.placeholder.as_deref(),
        &specs,
        &mut VecData(&data),
    ) {
        panic!("the layout passes check, but the exact rewrite fails: {e}");
    }
    if out != bytes {
        panic!(
            "exact rewrite differs from the input ({} vs {} bytes)",
            out.len(),
            bytes.len()
        );
    }
}
