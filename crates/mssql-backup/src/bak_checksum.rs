pub const PAGE_SIZE: usize = 8192;

const FLAG_CHECKSUM: u16 = 0x0200;
const FLAG_TORN_PAGE: u16 = 0x0100;
const PROTECTION_FIELD: u16 = 0x0F00;
const FLAGS_AT: usize = 4;
const TORN_BITS_AT: usize = 60;
const SECTOR: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrity {
    Checksum { stored: u32, computed: u32 },
    TornPage { ok: bool },
    Unprotected,
    InvalidProtection { flags: u16 },
}

impl Integrity {
    pub fn is_corrupt(&self) -> bool {
        match *self {
            Integrity::Checksum { stored, computed } => stored != computed,
            Integrity::TornPage { ok } => !ok,
            Integrity::Unprotected => false,
            Integrity::InvalidProtection { .. } => true,
        }
    }

    pub fn is_verified(&self) -> bool {
        match *self {
            Integrity::Checksum { stored, computed } => stored == computed,
            Integrity::TornPage { ok } => ok,
            Integrity::Unprotected | Integrity::InvalidProtection { .. } => false,
        }
    }
}

fn flag_bits(page: &[u8; PAGE_SIZE]) -> u16 {
    u16::from_le_bytes([page[FLAGS_AT], page[FLAGS_AT + 1]])
}

fn torn_bits(page: &[u8; PAGE_SIZE]) -> u32 {
    u32::from_le_bytes([
        page[TORN_BITS_AT],
        page[TORN_BITS_AT + 1],
        page[TORN_BITS_AT + 2],
        page[TORN_BITS_AT + 3],
    ])
}

pub fn page_checksum(page: &[u8; PAGE_SIZE]) -> u32 {
    let stored_word = torn_bits(page);
    let (sectors, _) = page.as_chunks::<SECTOR>();
    let mut sum = 0u32;
    for (index, sector) in sectors.iter().enumerate() {
        let (words, _) = sector.as_chunks::<4>();
        let mut folded = words
            .iter()
            .fold(0u32, |acc, word| acc ^ u32::from_le_bytes(*word));
        if index == 0 {
            folded ^= stored_word;
        }
        sum = sum.rotate_left(1) ^ folded;
    }
    sum
}

fn torn_signature_holds(page: &[u8; PAGE_SIZE], stored_word: u32) -> bool {
    let signature = (stored_word & 3) as u8;
    let (sectors, _) = page.as_chunks::<SECTOR>();
    matches!(signature, 1 | 2)
        && sectors
            .iter()
            .skip(1)
            .all(|sector| sector.last().is_some_and(|byte| byte & 3 == signature))
}

pub fn page_integrity(page: &[u8; PAGE_SIZE]) -> Integrity {
    let flags = flag_bits(page);
    let stored = torn_bits(page);
    match flags & PROTECTION_FIELD {
        0 => Integrity::Unprotected,
        FLAG_TORN_PAGE => Integrity::TornPage {
            ok: torn_signature_holds(page, stored),
        },
        FLAG_CHECKSUM => Integrity::Checksum {
            stored,
            computed: page_checksum(page),
        },
        _ => Integrity::InvalidProtection { flags },
    }
}
