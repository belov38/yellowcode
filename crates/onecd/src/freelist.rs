use crate::error::{Error, Location, Result};
use crate::format::FormatInfo;
use crate::format::bytes;
use crate::format::freelist::{entries_per_list_page, parse_freelist_header};
use crate::pager::PageRead;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreeList {
    pub count: u32,
    pub counter: u32,
    pub slots: Vec<u32>,
    pub list_pages: Vec<u32>,
    pub free_pages: Vec<u32>,
}

impl FreeList {
    pub fn load(pr: &dyn PageRead, fi: FormatInfo, header_page: u32) -> Result<FreeList> {
        let (list, problems) = FreeList::load_lossy(pr, fi, header_page)?;
        match problems.into_iter().next() {
            Some(e) => Err(e),
            None => Ok(list),
        }
    }

    pub fn load_lossy(
        pr: &dyn PageRead,
        fi: FormatInfo,
        header_page: u32,
    ) -> Result<(FreeList, Vec<Error>)> {
        let loc = || Location::Page(header_page);
        let bad = |what: String| Error::corrupt(loc(), what);
        if pr.page_size() != fi.page_size {
            return Err(bad(format!(
                "page size {} does not match the format page size {}",
                pr.page_size(),
                fi.page_size
            )));
        }
        let page = pr.read_page(header_page)?;
        let h = parse_freelist_header(fi, &page)?;
        let mut problems = Vec::new();
        let in_range = |p: u32| p != 0 && p < pr.num_pages();

        let prefix: Vec<u32> = h.slots.iter().copied().take_while(|&s| s != 0).collect();
        for &s in prefix.iter().filter(|&&s| !in_range(s)) {
            problems.push(bad(format!("list page {s} is out of range")));
        }

        let per = entries_per_list_page(fi) as usize;
        let count = h.count as usize;
        if count > prefix.len().saturating_mul(per) {
            problems.push(bad(format!(
                "{count} free pages do not fit in {} list pages",
                prefix.len()
            )));
        }
        if h.count >= pr.num_pages() {
            problems.push(bad(format!(
                "{count} free pages exceed the {} pages of the database",
                pr.num_pages()
            )));
        }

        let mut free_pages = Vec::with_capacity(count.min(pr.num_pages() as usize));
        for (i, &lp) in prefix.iter().enumerate() {
            let want = count.saturating_sub(i.saturating_mul(per)).min(per);
            if want == 0 {
                break;
            }
            if !in_range(lp) {
                continue;
            }
            let Some(entries) = bytes::u32s_le(&pr.read_page(lp)?, 0, want) else {
                problems.push(Error::corrupt(Location::Page(lp), "list page is truncated"));
                continue;
            };
            for e in entries {
                if in_range(e) {
                    free_pages.push(e);
                } else {
                    problems.push(Error::corrupt(
                        Location::Page(lp),
                        format!("free page {e} is out of range"),
                    ));
                }
            }
        }
        let list = FreeList {
            count: h.count,
            counter: h.counter,
            list_pages: prefix.into_iter().filter(|&p| in_range(p)).collect(),
            slots: h.slots,
            free_pages,
        };
        Ok((list, problems))
    }

    pub fn pop(&mut self) -> Option<u32> {
        let page = self.free_pages.pop()?;
        self.count = self.count.saturating_sub(1);
        Some(page)
    }

    pub fn push_target(&self, per: u32) -> Option<(usize, usize)> {
        let slot = usize::try_from(self.count.checked_div(per)?).ok()?;
        let pos = usize::try_from(self.count.checked_rem(per)?).ok()?;
        (slot < self.slots.len()).then_some((slot, pos))
    }

    pub fn set_list_page(&mut self, slot: usize, page: u32) {
        if let Some(s) = self.slots.get_mut(slot) {
            *s = page;
        }
        self.list_pages = self.slots.iter().copied().take_while(|&s| s != 0).collect();
    }

    pub fn push(&mut self, page: u32) {
        self.free_pages.push(page);
        self.count = self.count.saturating_add(1);
    }
}
