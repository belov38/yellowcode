pub(crate) mod blob;
pub(crate) mod brace;
pub(crate) mod bytes;
pub mod descr;
pub(crate) mod freelist;
pub(crate) mod header;
pub(crate) mod ifile;
pub(crate) mod index_page;
pub(crate) mod key;
pub mod layout;
pub(crate) mod managed;
pub(crate) mod record;
pub(crate) mod root;
pub(crate) mod value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    V8214,
    V838,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatInfo {
    pub format: Format,
    pub page_size: u32,
}

pub(crate) const PAGE_SIZES_838: [u32; 5] = [4096, 8192, 16384, 32768, 65536];

const MAX_IFILE_LEN_CAP: u64 = 16 << 30;

impl FormatInfo {
    pub fn file_header_slots(&self) -> u32 {
        match self.format {
            Format::V8214 => 1018,
            Format::V838 => (self.page_size / 4).saturating_sub(6),
        }
    }

    pub fn alloc_entries(&self) -> u32 {
        match self.format {
            Format::V8214 => 1023,
            Format::V838 => self.page_size / 4,
        }
    }

    pub fn freelist_slots(&self) -> u32 {
        match self.format {
            Format::V8214 => 1018,
            Format::V838 => self.page_size.saturating_sub(12) / 4,
        }
    }

    pub fn max_ifile_len(&self) -> u64 {
        u64::from(self.file_header_slots())
            .saturating_mul(u64::from(self.alloc_entries()))
            .saturating_mul(u64::from(self.page_size))
            .min(MAX_IFILE_LEN_CAP)
    }

    pub fn index_ptr_unit(&self) -> u64 {
        match self.format {
            Format::V8214 => 1,
            Format::V838 => u64::from(self.page_size),
        }
    }
}
