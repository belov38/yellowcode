use crate::transport::Response;

#[derive(Debug)]
pub enum Body {
    Native(Vec<u8>),
    Charged(memory_budget::Charged<Vec<u8>>),
}
impl Default for Body {
    fn default() -> Self {
        Self::Native(Vec::new())
    }
}
impl From<Vec<u8>> for Body {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Native(bytes)
    }
}
impl From<memory_budget::Charged<Vec<u8>>> for Body {
    fn from(bytes: memory_budget::Charged<Vec<u8>>) -> Self {
        Self::Charged(bytes)
    }
}
impl AsRef<[u8]> for Body {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Native(bytes) => bytes,
            Self::Charged(bytes) => bytes,
        }
    }
}
impl std::ops::Deref for Body {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_ref()
    }
}
impl<T: AsRef<[u8]>> PartialEq<T> for Body {
    fn eq(&self, other: &T) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl Eq for Body {}
impl PartialEq<Body> for &[u8] {
    fn eq(&self, other: &Body) -> bool {
        *self == other.as_ref()
    }
}
impl<'a> IntoIterator for &'a Body {
    type Item = &'a u8;
    type IntoIter = std::slice::Iter<'a, u8>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl Body {
    pub(crate) fn account(
        self,
        budget: &memory_budget::MemoryBudget,
        limit: u64,
    ) -> Result<Self, memory_budget::BudgetError> {
        match self {
            Self::Native(bytes) => {
                let mut writer = memory_budget::BudgetedWriter::new(budget, limit)?;
                writer.try_write(&bytes)?;
                drop(bytes);
                Ok(Self::Charged(writer.into_buffer().into_charged_vec()))
            }
            charged => Ok(charged),
        }
    }
    pub fn deliver(self, consume: impl FnOnce(Vec<u8>)) {
        match self {
            Self::Native(bytes) => consume(bytes),
            Self::Charged(bytes) => drop(bytes.map(consume)),
        }
    }
}

#[derive(Debug)]
pub enum JsonError {
    Memory(memory_budget::BudgetError),
    Serialization(serde_json::Error),
}
impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(f),
            Self::Serialization(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for JsonError {}

pub fn json_with_memory<T: serde::Serialize + ?Sized>(
    status: u16,
    value: &T,
    budget: &memory_budget::MemoryBudget,
    limit: u64,
) -> Result<Response, JsonError> {
    let mut writer =
        memory_budget::BudgetedWriter::new(budget, limit).map_err(JsonError::Memory)?;
    let result = to_writer_wide(writer.serialization_sink(), value);
    if let Some(error) = writer.error().copied() {
        return Err(JsonError::Memory(error));
    }
    result.map_err(JsonError::Serialization)?;
    Ok(Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: writer.into_buffer().into_charged_vec().into(),
        ..Default::default()
    })
}

struct WideFloats;
impl serde_json::ser::Formatter for WideFloats {
    fn write_f32<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        value: f32,
    ) -> std::io::Result<()> {
        self.write_f64(writer, f64::from(value))
    }
}
pub(crate) fn to_writer_wide<W: std::io::Write, T: serde::Serialize + ?Sized>(
    writer: W,
    value: &T,
) -> serde_json::Result<()> {
    value.serialize(&mut serde_json::Serializer::with_formatter(
        writer, WideFloats,
    ))
}
