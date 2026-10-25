use crate::error::{Error, Location, Result};
use memory_budget::{BudgetedVec, Category, Charged, MemoryBudget};
pub(crate) enum ReadBytes {
    Legacy(Vec<u8>),
    Managed(Charged<Vec<u8>>),
}
impl std::ops::Deref for ReadBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Legacy(v) => v,
            Self::Managed(v) => v.as_slice(),
        }
    }
}
pub(crate) enum ReadText {
    Legacy(String),
    Managed(Charged<String>),
}
impl std::ops::Deref for ReadText {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Self::Legacy(v) => v,
            Self::Managed(v) => v.as_str(),
        }
    }
}
impl ReadBytes {
    pub fn into_utf8(self) -> Result<ReadText> {
        let invalid = || Error::corrupt(Location::Root, "not valid UTF-8");
        match self {
            Self::Legacy(bytes) => String::from_utf8(bytes)
                .map(ReadText::Legacy)
                .map_err(|_| invalid()),
            Self::Managed(bytes) => bytes
                .try_map(String::from_utf8)
                .map(ReadText::Managed)
                .map_err(|_| Error::CorruptStatic {
                    location: Location::Root,
                    what: "not valid UTF-8",
                }),
        }
    }
    pub fn decode_utf16(&self, budget: Option<&MemoryBudget>) -> Result<ReadText> {
        let invalid = || Error::corrupt(Location::Root, "not valid UTF-16");
        let Some(budget) = budget else {
            return crate::format::value::utf16le_decode(self)
                .map(ReadText::Legacy)
                .ok_or_else(invalid);
        };
        let invalid = || Error::CorruptStatic {
            location: Location::Root,
            what: "not valid UTF-16",
        };
        if !self.len().is_multiple_of(2) {
            return Err(invalid());
        }
        let capacity = (self.len() / 2)
            .checked_mul(3)
            .ok_or(memory_budget::BudgetError::Overflow)?;
        let mut out = BudgetedVec::try_with_capacity(budget, Category::Metadata, capacity)?;
        let units = self
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes);
        for value in char::decode_utf16(units) {
            let value = value.map_err(|_| invalid())?;
            let mut bytes = [0u8; 4];
            out.try_extend_from_slice(value.encode_utf8(&mut bytes).as_bytes())?;
        }
        out.into_charged_vec()
            .try_map(String::from_utf8)
            .map(ReadText::Managed)
            .map_err(|_| invalid())
    }
}
