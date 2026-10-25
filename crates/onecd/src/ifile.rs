use std::io;

use crate::error::{Constraint, Error, Location, Result};
use crate::format::bytes::{self, OrCorrupt};
use crate::format::ifile::{
    IFileHeader, encode_ifile_header_838, ifile_versions_838, parse_alloc_page, parse_ifile_header,
};
use crate::format::{Format, FormatInfo};
use crate::pager::txn::TxnPages;
use crate::pager::{PageKind, PageRead};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IFile {
    pub header_page: u32,
    pub fatlevel: u16,
    pub versions: [u32; 3],
    pub length: u64,
    pub alloc_pages: crate::page_numbers::PageNumbers,
    pub data_pages: crate::page_numbers::PageNumbers,
    pub owner: Option<(String, &'static str)>,
}

fn needed_in_alloc(ndata: usize, per: usize, i: usize) -> usize {
    ndata.saturating_sub(i.saturating_mul(per)).min(per)
}

impl IFile {
    pub fn open(pr: &dyn PageRead, fi: FormatInfo, header_page: u32) -> Result<IFile> {
        if pr.memory_budget().is_some() {
            return Self::open_budgeted(pr, fi, header_page);
        }
        let loc = || Location::InternalFile(header_page);
        let bad = |what: String| Error::corrupt(loc(), what);
        if pr.page_size() != fi.page_size {
            return Err(bad(format!(
                "page size {} does not match the format page size {}",
                pr.page_size(),
                fi.page_size
            )));
        }
        let page = pr.read_page(header_page)?;
        let header = parse_ifile_header(fi, &page, header_page)?;
        let (fatlevel, versions, length, slots) = match header {
            IFileHeader::V838 {
                fatlevel,
                versions,
                length,
                slots,
            } => (fatlevel, versions, length, slots),
            IFileHeader::V8214 {
                length,
                versions,
                slots,
            } => (1, versions, u64::from(length), slots),
        };
        if fatlevel > 1 {
            return Err(bad(format!("unknown fatlevel {fatlevel}")));
        }
        if length > fi.max_ifile_len() {
            return Err(bad(format!(
                "length {length} exceeds the maximum {}",
                fi.max_ifile_len()
            )));
        }
        let ndata = length.div_ceil(u64::from(fi.page_size));
        if fi.format == Format::V838 && fatlevel == 0 && ndata > slots.len() as u64 {
            return Err(bad(format!(
                "length {length} exceeds the fatlevel 0 capacity of {} pages",
                slots.len()
            )));
        }
        if ndata > u64::from(pr.num_pages()) {
            return Err(bad(format!(
                "length {length} exceeds the {} pages of the database",
                pr.num_pages()
            )));
        }
        let ndata = usize::try_from(ndata).map_err(|_| bad("file has too many pages".into()))?;

        let check = |p: u32| -> Result<u32> {
            if p == 0 || p >= pr.num_pages() {
                Err(bad(format!("page reference {p} is out of range")))
            } else {
                Ok(p)
            }
        };
        let (alloc_pages, data_pages) = match (fi.format, fatlevel) {
            (Format::V838, 0) => {
                let data = slots.get(..ndata).or_corrupt(loc, "too few page slots")?;
                (
                    Vec::new(),
                    data.iter().map(|&p| check(p)).collect::<Result<_>>()?,
                )
            }
            _ => {
                let per = fi.alloc_entries() as usize;
                let nalloc = ndata.div_ceil(per.max(1));
                let alloc = slots
                    .get(..nalloc)
                    .or_corrupt(loc, "too few allocation slots")?;
                let mut alloc_pages = Vec::with_capacity(nalloc);
                let mut data_pages: Vec<u32> = Vec::with_capacity(ndata);
                for (i, &ap) in alloc.iter().enumerate() {
                    let ap = check(ap)?;
                    alloc_pages.push(ap);
                    let refs = parse_alloc_page(fi, &pr.read_page(ap)?, ap)?;
                    if fi.format == Format::V838 {
                        let used = refs
                            .get(..needed_in_alloc(ndata, per, i))
                            .or_corrupt(|| Location::Page(ap), "allocation page is too short")?;
                        data_pages.extend_from_slice(used);
                    } else {
                        data_pages.extend_from_slice(&refs);
                    }
                }
                if data_pages.len() < ndata {
                    return Err(bad(format!(
                        "allocation pages list {} of {ndata} data pages",
                        data_pages.len()
                    )));
                }
                data_pages.truncate(ndata);
                for &p in &data_pages {
                    check(p)?;
                }
                (alloc_pages, data_pages)
            }
        };
        Ok(IFile {
            header_page,
            fatlevel,
            versions,
            length,
            alloc_pages: alloc_pages.into(),
            data_pages: data_pages.into(),
            owner: None,
        })
    }

    fn open_budgeted(pr: &dyn PageRead, fi: FormatInfo, header_page: u32) -> Result<IFile> {
        let cache = pr.file_cache();
        if let Some(file) = cache.and_then(|(files, epoch)| files.get(epoch, header_page)) {
            return Ok(file);
        }
        let file = Self::read_budgeted(pr, fi, header_page)?;
        if let Some((files, epoch)) = cache {
            files.insert(epoch, &file);
        }
        Ok(file)
    }

    fn read_budgeted(pr: &dyn PageRead, fi: FormatInfo, header_page: u32) -> Result<IFile> {
        use crate::page_numbers::PageNumbers;
        use memory_budget::{BudgetedVec, Category};
        let loc = || Location::InternalFile(header_page);
        let bad = |what: &'static str| Error::CorruptStatic {
            location: loc(),
            what,
        };
        let Some(budget) = pr.memory_budget() else {
            return Err(bad("missing managed context"));
        };
        if pr.page_size() != fi.page_size {
            return Err(bad("page size does not match the format"));
        }
        let page = pr.read_page(header_page)?;
        let (fatlevel, versions, length) = match fi.format {
            Format::V838 => {
                if page.get(..2) != Some(&[0x1c, 0xfd]) {
                    return Err(bad("bad internal file signature"));
                }
                let fat = bytes::u16le(&page, 2)
                    .or_corrupt_static(loc, "file header page is truncated")?;
                let versions = [4, 8, 12].map(|offset| bytes::u32le(&page, offset));
                let [Some(a), Some(b), Some(c)] = versions else {
                    return Err(bad("file header page is truncated"));
                };
                (
                    fat,
                    [a, b, c],
                    bytes::u64le(&page, 16)
                        .or_corrupt_static(loc, "file header page is truncated")?,
                )
            }
            Format::V8214 => {
                if page.get(..8) != Some(b"1CDBOBV8") {
                    return Err(bad("bad internal file signature"));
                }
                let versions = [12, 16, 20].map(|offset| bytes::u32le(&page, offset));
                let [Some(a), Some(b), Some(c)] = versions else {
                    return Err(bad("file header page is truncated"));
                };
                (
                    1,
                    [a, b, c],
                    u64::from(
                        bytes::u32le(&page, 8)
                            .or_corrupt_static(loc, "file header page is truncated")?,
                    ),
                )
            }
        };
        if fatlevel > 1 {
            return Err(bad("unknown fatlevel"));
        }
        if length > fi.max_ifile_len() {
            return Err(bad("length exceeds the format maximum"));
        }
        let ndata64 = length.div_ceil(u64::from(fi.page_size));
        if ndata64 > u64::from(pr.num_pages()) {
            return Err(bad("length exceeds the pages of the database"));
        }
        let ndata = usize::try_from(ndata64).map_err(|_| bad("file has too many pages"))?;
        let slots = fi.file_header_slots() as usize;
        let per = fi.alloc_entries() as usize;
        let nalloc = if fatlevel == 0 {
            0
        } else {
            ndata.div_ceil(per.max(1))
        };
        if (fatlevel == 0 && ndata > slots) || nalloc > slots {
            return Err(bad("too few allocation slots"));
        }
        let slot = |index: usize| {
            bytes::u32le(&page, 24 + 4 * index)
                .or_corrupt_static(loc, "file header page is truncated")
        };
        let check = |number: u32| -> Result<u32> {
            if number == 0 || number >= pr.num_pages() {
                Err(bad("page reference is out of range"))
            } else {
                Ok(number)
            }
        };
        let mut alloc_pages = BudgetedVec::try_with_capacity(budget, Category::Metadata, nalloc)?;
        let mut data_pages = BudgetedVec::try_with_capacity(budget, Category::Metadata, ndata)?;
        if fatlevel == 0 {
            for i in 0..ndata {
                data_pages.try_push(check(slot(i)?)?)?;
            }
        } else {
            for i in 0..nalloc {
                let ap = check(slot(i)?)?;
                alloc_pages.try_push(ap)?;
                let raw = pr.read_page(ap)?;
                let (offset, count) = match fi.format {
                    Format::V838 => (0, per),
                    Format::V8214 => {
                        let count = bytes::u32le(&raw, 0).or_corrupt_static(
                            || Location::Page(ap),
                            "allocation page is truncated",
                        )?;
                        if count > 1023 {
                            return Err(bad("allocation page count is out of range"));
                        }
                        (4, count as usize)
                    }
                };
                let count = count.min(ndata - data_pages.len());
                for index in 0..count {
                    let number = bytes::u32le(&raw, offset + 4 * index)
                        .or_corrupt_static(|| Location::Page(ap), "allocation page is truncated")?;
                    data_pages.try_push(check(number)?)?;
                }
            }
            if data_pages.len() != ndata {
                return Err(bad("allocation pages list too few data pages"));
            }
        }
        Ok(IFile {
            header_page,
            fatlevel,
            versions,
            length,
            alloc_pages: PageNumbers::Budgeted(alloc_pages.try_into_shared()?),
            data_pages: PageNumbers::Budgeted(data_pages.try_into_shared()?),
            owner: None,
        })
    }

    pub(crate) fn is_managed(&self) -> bool {
        matches!(
            self.data_pages,
            crate::page_numbers::PageNumbers::Budgeted(_)
        )
    }

    pub fn read_at(&self, pr: &dyn PageRead, off: u64, buf: &mut [u8]) -> Result<()> {
        if pr.memory_budget().is_some() {
            return self.read_at_managed(pr, off, buf);
        }
        let loc = || Location::InternalFile(self.header_page);
        let wanted = buf.len() as u64;
        if off.checked_add(wanted).is_none_or(|end| end > self.length) {
            return Err(Error::corrupt(
                loc(),
                format!(
                    "read of {wanted} bytes at {off} is beyond the file length {}",
                    self.length
                ),
            ));
        }
        let ps = u64::from(pr.page_size());
        if ps == 0 {
            return Err(Error::corrupt(loc(), "page size is zero"));
        }
        let mut pos = off;
        let mut rest = buf;
        while !rest.is_empty() {
            let index = usize::try_from(pos / ps)
                .ok()
                .or_corrupt(loc, "page index overflows")?;
            let in_page = usize::try_from(pos % ps)
                .ok()
                .or_corrupt(loc, "page offset overflows")?;
            let room = usize::try_from(ps)
                .unwrap_or(usize::MAX)
                .saturating_sub(in_page);
            let n = rest.len().min(room);
            let page_no = *self
                .data_pages
                .get(index)
                .or_corrupt(loc, "no page for the offset")?;
            let page = pr.read_page(page_no)?;
            let src = bytes::sub(&page, in_page, n).or_corrupt(
                || Location::Page(page_no),
                "page is shorter than the page size",
            )?;
            let (head, tail) = rest
                .split_at_mut_checked(n)
                .or_corrupt(loc, "read split is out of range")?;
            head.copy_from_slice(src);
            rest = tail;
            pos = pos.saturating_add(n as u64);
        }
        Ok(())
    }

    pub(crate) fn read_at_managed(
        &self,
        pr: &dyn PageRead,
        off: u64,
        buf: &mut [u8],
    ) -> Result<()> {
        let loc = || Location::InternalFile(self.header_page);
        let wanted = buf.len() as u64;
        if off.checked_add(wanted).is_none_or(|end| end > self.length) {
            return Err(Error::CorruptStatic {
                location: loc(),
                what: "read is beyond the file length",
            });
        }
        let ps = u64::from(pr.page_size());
        if ps == 0 {
            return Err(Error::CorruptStatic {
                location: loc(),
                what: "page size is zero",
            });
        }
        let mut pos = off;
        let mut rest = buf;
        while !rest.is_empty() {
            let index = usize::try_from(pos / ps)
                .ok()
                .or_corrupt_static(loc, "page index overflows")?;
            let in_page = usize::try_from(pos % ps)
                .ok()
                .or_corrupt_static(loc, "page offset overflows")?;
            let room = usize::try_from(ps)
                .unwrap_or(usize::MAX)
                .saturating_sub(in_page);
            let n = rest.len().min(room);
            let page_no = *self
                .data_pages
                .get(index)
                .or_corrupt_static(loc, "no page for the offset")?;
            let page = pr.read_page(page_no)?;
            let src = bytes::sub(&page, in_page, n).or_corrupt_static(
                || Location::Page(page_no),
                "page is shorter than the page size",
            )?;
            let (head, tail) = rest
                .split_at_mut_checked(n)
                .or_corrupt_static(loc, "read split is out of range")?;
            head.copy_from_slice(src);
            rest = tail;
            pos = pos.saturating_add(n as u64);
        }
        Ok(())
    }

    pub(crate) fn read_all_managed(
        &self,
        pr: &dyn PageRead,
        budget: &memory_budget::MemoryBudget,
        category: memory_budget::Category,
    ) -> Result<memory_budget::Charged<Vec<u8>>> {
        let len = usize::try_from(self.length).map_err(|_| memory_budget::BudgetError::Overflow)?;
        let mut out = memory_budget::BudgetedVec::try_with_capacity(budget, category, len)?;
        out.try_resize(len, 0)?;
        self.read_at(pr, 0, &mut out)?;
        Ok(out.into_charged_vec())
    }

    pub fn read_all(&self, pr: &dyn PageRead) -> Result<Vec<u8>> {
        let too_big = || {
            Error::corrupt(
                Location::InternalFile(self.header_page),
                "file is too large to read",
            )
        };
        let len = usize::try_from(self.length).map_err(|_| too_big())?;
        let mut out = Vec::new();
        out.try_reserve_exact(len).map_err(|_| {
            Error::Io(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "cannot buffer the file",
            ))
        })?;
        out.resize(len, 0);
        self.read_at(pr, 0, &mut out)?;
        Ok(out)
    }

    pub fn owned_pages(&self) -> impl Iterator<Item = u32> + '_ {
        std::iter::once(self.header_page)
            .chain(self.alloc_pages.iter().copied())
            .chain(self.data_pages.iter().copied())
    }
}

const LENGTH_LIMIT: &str = "internal file length limit";

impl IFile {
    pub fn owned_by(mut self, table: &str, kind: &'static str) -> IFile {
        self.owner = Some((table.to_string(), kind));
        self
    }

    pub fn does_not_fit(&self, why: &str) -> Error {
        let (table, field) = match &self.owner {
            Some((table, kind)) => (table.clone(), (*kind).to_string()),
            None => (String::new(), String::new()),
        };
        Error::Constraint(Constraint::ValueDoesNotFit {
            table,
            field,
            why: why.into(),
        })
    }

    pub fn check_len(&self, fi: FormatInfo, len: u64) -> Result<()> {
        if len > fi.max_ifile_len() {
            return Err(self.does_not_fit(LENGTH_LIMIT));
        }
        Ok(())
    }

    pub fn create(tx: &mut TxnPages) -> Result<IFile> {
        tx.run(|tx| {
            let header_page = tx.alloc_page(PageKind::FileHeader)?;
            let f = IFile {
                header_page,
                fatlevel: 0,
                versions: [0; 3],
                length: 0,
                alloc_pages: Vec::new().into(),
                data_pages: Vec::new().into(),
                owner: None,
            };
            f.write_header(tx, f.versions)?;
            tx.file_created(header_page);
            Ok(f)
        })
    }

    pub fn write_at(&mut self, tx: &mut TxnPages, off: u64, data: &[u8]) -> Result<()> {
        tx.run(|tx| {
            let loc = || Location::InternalFile(self.header_page);
            let n = data.len() as u64;
            if off.checked_add(n).is_none_or(|end| end > self.length) {
                return Err(Error::corrupt(
                    loc(),
                    format!(
                        "write of {n} bytes at {off} is beyond the file length {}",
                        self.length
                    ),
                ));
            }
            if data.is_empty() {
                return Ok(());
            }
            let ps = u64::from(tx.page_size());
            let mut pos = off;
            let mut rest = data;
            while !rest.is_empty() {
                let index = pos
                    .checked_div(ps)
                    .and_then(|i| usize::try_from(i).ok())
                    .or_corrupt(loc, "page index overflows")?;
                let in_page = pos
                    .checked_rem(ps)
                    .and_then(|i| usize::try_from(i).ok())
                    .or_corrupt(loc, "page offset overflows")?;
                let page_no = *self
                    .data_pages
                    .get(index)
                    .or_corrupt(loc, "no page for the offset")?;
                let page = tx.page_mut(page_no, PageKind::Content)?;
                let k = rest.len().min(page.len().saturating_sub(in_page));
                let (head, tail) = rest
                    .split_at_checked(k)
                    .or_corrupt(loc, "write split is out of range")?;
                page.get_mut(in_page..in_page.saturating_add(k))
                    .filter(|_| k > 0)
                    .or_corrupt(
                        || Location::Page(page_no),
                        "page is shorter than the page size",
                    )?
                    .copy_from_slice(head);
                rest = tail;
                pos = pos.saturating_add(k as u64);
            }
            tx.touch_file(self.header_page, false);
            Ok(())
        })
    }

    pub fn set_len(&mut self, tx: &mut TxnPages, new_len: u64) -> Result<()> {
        tx.check_alive()?;
        let fi = tx.format_info();
        self.check_len(fi, new_len)?;
        tx.run(|tx| self.grow(tx, fi, new_len))
    }

    pub fn free_all(self, tx: &mut TxnPages) -> Result<()> {
        let pages: Vec<u32> = self
            .data_pages
            .iter()
            .rev()
            .chain(self.alloc_pages.iter().rev())
            .copied()
            .chain(std::iter::once(self.header_page))
            .collect();
        tx.run(|tx| {
            tx.free_pages(&pages)?;
            tx.file_freed(self.header_page);
            Ok(())
        })
    }

    pub fn flush_header(&self, tx: &mut TxnPages) -> Result<()> {
        tx.run(|tx| {
            let h = self.header_page;
            let versions = ifile_versions_838(&tx.read_page(h)?, h)?;
            self.write_header(tx, versions)?;
            tx.touch_file(h, false);
            Ok(())
        })
    }

    fn write_header(&self, tx: &mut TxnPages, versions: [u32; 3]) -> Result<()> {
        let h = self.header_page;
        let slots = if self.fatlevel == 0 {
            &self.data_pages
        } else {
            &self.alloc_pages
        };
        let page = tx.page_mut(h, PageKind::FileHeader)?;
        encode_ifile_header_838(self.fatlevel, versions, self.length, slots, page).map_err(|e| {
            match e {
                Error::Corrupt { what, .. } => Error::corrupt(Location::InternalFile(h), what),
                other => other,
            }
        })
    }

    fn grow(&mut self, tx: &mut TxnPages, fi: FormatInfo, new_len: u64) -> Result<()> {
        let loc = || Location::InternalFile(self.header_page);
        if new_len < self.length {
            return Err(Error::corrupt(
                loc(),
                format!(
                    "internal files do not shrink ({} to {new_len} bytes)",
                    self.length
                ),
            ));
        }
        if new_len == self.length {
            return Ok(());
        }
        let ps = u64::from(fi.page_size);
        if ps == 0 {
            return Err(Error::corrupt(loc(), "page size is zero"));
        }
        let old_n = self.data_pages.len();
        let new_n = usize::try_from(new_len.div_ceil(ps))
            .ok()
            .or_corrupt(loc, "file has too many pages")?;
        self.zero_tail(tx, ps, new_len)?;
        let grows = new_n > old_n;
        if grows {
            let slots = fi.file_header_slots() as usize;
            let direct = self.fatlevel == 0;
            if direct && new_n > slots {
                self.fatlevel = 1;
            }
            if self.fatlevel == 0 {
                for _ in old_n..new_n {
                    self.data_pages.push(tx.alloc_page(PageKind::Content)?)?;
                }
            } else {
                let per = fi.alloc_entries() as usize;
                let nalloc = new_n.div_ceil(per.max(1));
                if nalloc > slots {
                    return Err(Error::corrupt(
                        loc(),
                        format!("{new_n} pages need more than {slots} allocation pages"),
                    ));
                }
                while self.alloc_pages.len() < nalloc {
                    self.alloc_pages.push(tx.alloc_page(PageKind::Alloc)?)?;
                }
                for _ in old_n..new_n {
                    self.data_pages.push(tx.alloc_page(PageKind::Content)?)?;
                }
                self.write_alloc_entries(tx, per, if direct { 0 } else { old_n })?;
            }
        }
        self.length = new_len;
        self.flush_header(tx)?;
        tx.touch_file(self.header_page, grows);
        Ok(())
    }

    fn zero_tail(&self, tx: &mut TxnPages, ps: u64, new_len: u64) -> Result<()> {
        let used = self.length.checked_rem(ps).unwrap_or(0);
        let Some(&last) = self.data_pages.last().filter(|_| used != 0) else {
            return Ok(());
        };
        let end = new_len
            .saturating_sub(self.length)
            .saturating_add(used)
            .min(ps);
        let (Ok(start), Ok(end)) = (usize::try_from(used), usize::try_from(end)) else {
            return Err(Error::corrupt(
                Location::InternalFile(self.header_page),
                "page offset overflows",
            ));
        };
        let short = || Location::Page(last);
        let tail = tx.read_page(last)?;
        let tail = tail
            .get(start..end)
            .or_corrupt(short, "page is shorter than the page size")?;
        if tail.iter().any(|&b| b != 0) {
            tx.page_mut(last, PageKind::Content)?
                .get_mut(start..end)
                .or_corrupt(short, "page is shorter than the page size")?
                .fill(0);
        }
        Ok(())
    }

    fn write_alloc_entries(&self, tx: &mut TxnPages, per: usize, from: usize) -> Result<()> {
        let per = per.max(1);
        if self.alloc_pages.len() < self.data_pages.len().div_ceil(per) {
            return Err(Error::corrupt(
                Location::InternalFile(self.header_page),
                "too few allocation pages for the data pages",
            ));
        }
        for (j, (chunk, &ap)) in self
            .data_pages
            .chunks(per)
            .zip(&self.alloc_pages)
            .enumerate()
        {
            let first = j.saturating_mul(per);
            if first.saturating_add(chunk.len()) <= from {
                continue;
            }
            let page = tx.page_mut(ap, PageKind::Alloc)?;
            for (pos, &p) in chunk.iter().enumerate().skip(from.saturating_sub(first)) {
                pos.checked_mul(4)
                    .and_then(|off| bytes::put_u32le(page, off, p))
                    .or_corrupt(|| Location::Page(ap), "allocation page is too short")?;
            }
        }
        Ok(())
    }
}
