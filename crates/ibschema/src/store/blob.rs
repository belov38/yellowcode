use super::{ReadSeek, StoreError, StoreResult};
use memory_budget::{Charged, Reservation};
use std::io::{self, Cursor, Read, Seek, SeekFrom};

struct ChargedBlob {
    bytes: Charged<Vec<u8>>,
    position: u64,
    _boxed: Reservation,
}

pub fn charged_blob(bytes: Charged<Vec<u8>>) -> StoreResult<Box<dyn ReadSeek>> {
    let paid = bytes
        .reservation()
        .budget()
        .try_reserve(
            bytes.reservation().category(),
            std::mem::size_of::<ChargedBlob>() as u64,
        )
        .map_err(StoreError::Memory)?;
    Ok(Box::new(ChargedBlob {
        bytes,
        position: 0,
        _boxed: paid,
    }))
}

impl Read for ChargedBlob {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut cursor = Cursor::new(self.bytes.as_slice());
        cursor.set_position(self.position);
        let size = cursor.read(output)?;
        self.position = cursor.position();
        Ok(size)
    }
}

impl Seek for ChargedBlob {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let mut cursor = Cursor::new(self.bytes.as_slice());
        cursor.set_position(self.position);
        self.position = cursor.seek(from)?;
        Ok(self.position)
    }
}
