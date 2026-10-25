use crate::time::V8Time;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    #[serde(rename = "Format15")]
    F15,
    #[serde(rename = "Format16")]
    F16,
}

impl Format {
    pub const fn base(self) -> u64 {
        match self {
            Format::F15 => 0,
            Format::F16 => 0x1359,
        }
    }
    pub const fn undefined(self) -> u64 {
        match self {
            Format::F15 => 0x7FFF_FFFF,
            Format::F16 => u64::MAX,
        }
    }
    pub const fn file_header_size(self) -> u64 {
        match self {
            Format::F15 => 16,
            Format::F16 => 20,
        }
    }
    pub const fn page_header_size(self) -> u64 {
        match self {
            Format::F15 => 31,
            Format::F16 => 55,
        }
    }
    pub const fn toc_record_size(self) -> u64 {
        match self {
            Format::F15 => 12,
            Format::F16 => 24,
        }
    }
    pub const fn default_toc_page(self) -> u64 {
        match self {
            Format::F15 => 0x200,
            Format::F16 => 0x10000,
        }
    }
    pub const fn addr_size(self) -> u64 {
        match self {
            Format::F15 => 4,
            Format::F16 => 8,
        }
    }
    pub const fn hex_digits(self) -> usize {
        match self {
            Format::F15 => 8,
            Format::F16 => 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHeader {
    pub next_free_page: u64,
    pub page_size: u32,
    pub storage_ver: u32,
    pub reserved: u32,
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

impl FileHeader {
    pub fn parse(f: Format, b: &[u8]) -> Option<Self> {
        let (next_free_page, at) = match f {
            Format::F15 => (u64::from(le_u32(b, 0)?), 4),
            Format::F16 => (u64::from_le_bytes(b.get(..8)?.try_into().ok()?), 8),
        };
        Some(FileHeader {
            next_free_page,
            page_size: le_u32(b, at)?,
            storage_ver: le_u32(b, at + 4)?,
            reserved: le_u32(b, at + 8)?,
        })
    }

    pub fn encode(&self, f: Format) -> Vec<u8> {
        let mut v = Vec::with_capacity(20);
        match f {
            Format::F15 => v.extend_from_slice(&(self.next_free_page as u32).to_le_bytes()),
            Format::F16 => v.extend_from_slice(&self.next_free_page.to_le_bytes()),
        }
        v.extend_from_slice(&self.page_size.to_le_bytes());
        v.extend_from_slice(&self.storage_ver.to_le_bytes());
        v.extend_from_slice(&self.reserved.to_le_bytes());
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageHeader {
    pub data_size: u64,
    pub page_size: u64,
    pub next_page: u64,
}

impl PageHeader {
    pub fn parse(f: Format, b: &[u8]) -> Option<Self> {
        let h = f.hex_digits();
        if b.len() != usize::try_from(f.page_header_size()).ok()? {
            return None;
        }
        if b.get(..2)? != b"\r\n" || b.get(b.len() - 2..)? != b"\r\n" {
            return None;
        }
        let field = |i: usize| -> Option<u64> {
            let start = 2 + i * (h + 1);
            let s = b.get(start..start + h)?;
            if !s.iter().all(u8::is_ascii_hexdigit) || *b.get(start + h)? != b' ' {
                return None;
            }
            u64::from_str_radix(std::str::from_utf8(s).ok()?, 16).ok()
        };
        Some(PageHeader {
            data_size: field(0)?,
            page_size: field(1)?,
            next_page: field(2)?,
        })
    }

    pub fn encode(&self, f: Format) -> Option<Vec<u8>> {
        let s = match f {
            Format::F15 => {
                let max = u64::from(u32::MAX);
                if self.data_size > max || self.page_size > max || self.next_page > max {
                    return None;
                }
                format!(
                    "\r\n{:08x} {:08x} {:08x} \r\n",
                    self.data_size, self.page_size, self.next_page
                )
            }
            Format::F16 => {
                format!(
                    "\r\n{:016X} {:016X} {:016X} \r\n",
                    self.data_size, self.page_size, self.next_page
                )
            }
        };
        Some(s.into_bytes())
    }
}

pub fn entry_header_bytes(ctime: V8Time, mtime: V8Time, attrs: u32, name: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(20 + name.len() * 2 + 4);
    v.extend_from_slice(&ctime.0.to_le_bytes());
    v.extend_from_slice(&mtime.0.to_le_bytes());
    v.extend_from_slice(&attrs.to_le_bytes());
    for u in name.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v.extend_from_slice(&[0; 4]);
    v
}

pub(crate) fn parse_entry_header(b: &[u8]) -> Option<(V8Time, V8Time, u32, String)> {
    let ctime = u64::from_le_bytes(b.get(..8)?.try_into().ok()?);
    let mtime = u64::from_le_bytes(b.get(8..16)?.try_into().ok()?);
    let attrs = le_u32(b, 16)?;
    let units: Vec<u16> = b
        .get(20..)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    Some((
        V8Time(ctime),
        V8Time(mtime),
        attrs,
        String::from_utf16_lossy(&units),
    ))
}
