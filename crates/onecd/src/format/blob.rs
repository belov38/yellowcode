use super::bytes::{self, OrCorrupt};
use crate::error::{Error, Location, Result};

pub const BLOCK: usize = 256;
pub const BLOCK_DATA: usize = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobBlock {
    pub next: u32,
    pub used: u16,
    pub data: [u8; BLOCK_DATA],
}

impl BlobBlock {
    pub fn bytes(&self) -> &[u8] {
        self.data
            .get(..usize::from(self.used))
            .unwrap_or(self.data.as_slice())
    }
}

const DATA_OFFSET: usize = 6;

pub fn decode_block(b: &[u8]) -> Result<BlobBlock> {
    let loc = || Location::File;
    let block = bytes::sub(b, 0, BLOCK).or_corrupt(loc, "blob block is truncated")?;
    let next = bytes::u32le(block, 0).or_corrupt(loc, "blob block is truncated")?;
    let used = bytes::u16le(block, 4).or_corrupt(loc, "blob block is truncated")?;
    if usize::from(used) > BLOCK_DATA {
        return Err(Error::corrupt(
            loc(),
            format!("blob block claims {used} used bytes, the maximum is {BLOCK_DATA}"),
        ));
    }
    let data = bytes::array(block, DATA_OFFSET).or_corrupt(loc, "blob block is truncated")?;
    Ok(BlobBlock { next, used, data })
}

pub fn encode_block(blk: &BlobBlock, out: &mut [u8]) {
    let Some(out) = out.get_mut(..BLOCK) else {
        return;
    };
    let _ = bytes::put_u32le(out, 0, blk.next);
    let _ = bytes::put_u16le(out, 4, blk.used);
    if let Some(dst) = out.get_mut(DATA_OFFSET..) {
        dst.copy_from_slice(&blk.data);
    }
}
