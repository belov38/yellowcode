use crate::{Avail, TypeIdx};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};
use std::ops::Range;

#[derive(Debug)]
pub enum ManagedCatalogError {
    Format(&'static str),
    Memory(BudgetError),
    Inflate(zraw::ManagedInflateError),
}
impl std::fmt::Display for ManagedCatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(message) => f.write_str(message),
            Self::Memory(error) => error.fmt(f),
            Self::Inflate(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ManagedCatalogError {}
impl From<BudgetError> for ManagedCatalogError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
type Result<T> = std::result::Result<T, ManagedCatalogError>;

#[derive(Debug)]
struct TypeRow {
    id: Range<usize>,
    ru: Range<usize>,
    en: Range<usize>,
    presentation: bool,
    aliases: Range<usize>,
    properties: Range<usize>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedTypeNames<'a> {
    pub id: &'a str,
    pub name_ru: &'a str,
    pub name_en: &'a str,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyNames<'a> {
    pub name_ru: &'a str,
    pub name_en: &'a str,
}
#[derive(Debug)]
pub struct ManagedCatalog {
    raw: Charged<Vec<u8>>,
    types: BudgetedVec<TypeRow>,
    platform: BudgetedText,
}
impl ManagedCatalog {
    pub fn decode(bytes: &[u8], budget: &MemoryBudget) -> Result<Self> {
        let (platform, body) =
            crate::codec::split_header_view(bytes).map_err(ManagedCatalogError::Format)?;
        let mut platform_text = BudgetedText::new(budget, Category::Metadata, 64)?;
        platform_text.try_push_str(platform)?;
        let raw =
            zraw::inflate_budgeted(body, crate::codec::MAX_INFLATED, budget, Category::Metadata)
                .map_err(|error| match error {
                    zraw::ManagedInflateError::Memory(error) => ManagedCatalogError::Memory(error),
                    other => ManagedCatalogError::Inflate(other),
                })?;
        let mut reader = Reader::new(&raw);
        let count = reader.count()?;
        if count > usize::from(u16::MAX) + 1 {
            return format("too many types");
        }
        let mut types = BudgetedVec::try_with_capacity(budget, Category::Metadata, count)?;
        for _ in 0..count {
            types.try_push(reader.type_row()?)?;
        }
        reader.vector(Reader::property)?;
        reader.vector(Reader::global_method)?;
        reader.vector(Reader::table)?;
        reader.vector(|r| {
            r.string()?;
            r.names()?;
            Ok(())
        })?;
        if reader.at != raw.len() {
            return format("trailing bytes");
        }
        Ok(Self {
            raw,
            types,
            platform: platform_text,
        })
    }
    pub fn memory_budget(&self) -> &MemoryBudget {
        self.raw.reservation().budget()
    }
    pub fn platform(&self) -> &str {
        self.platform.as_str()
    }
    pub fn types(&self) -> impl ExactSizeIterator<Item = ManagedTypeNames<'_>> {
        self.types.iter().map(|row| self.names(row))
    }
    pub fn type_info(&self, index: TypeIdx) -> Option<ManagedTypeNames<'_>> {
        self.types
            .get(usize::from(index.0))
            .map(|row| self.names(row))
    }
    fn names(&self, row: &TypeRow) -> ManagedTypeNames<'_> {
        ManagedTypeNames {
            id: self.text(&row.id),
            name_ru: self.text(&row.ru),
            name_en: self.text(&row.en),
        }
    }
    fn text(&self, range: &Range<usize>) -> &str {
        std::str::from_utf8(self.raw.get(range.clone()).unwrap_or_default()).unwrap_or_default()
    }
    pub fn find_type(&self, name: &str) -> Option<TypeIdx> {
        self.find_type_parts(&[name])
    }
    pub fn find_type_parts(&self, parts: &[&str]) -> Option<TypeIdx> {
        let mut first = None;
        for (ordinal, row) in self.types.iter().enumerate() {
            let aliases = self.raw.get(row.aliases.clone()).unwrap_or_default();
            let mut aliases = Reader::new(aliases);
            let mut matches =
                same_parts(self.text(&row.ru), parts) || same_parts(self.text(&row.en), parts);
            let parsed = aliases.vector(|r| {
                let range = r.string()?;
                let text =
                    std::str::from_utf8(r.bytes.get(range).unwrap_or_default()).unwrap_or_default();
                matches |= same_parts(text, parts);
                Ok(())
            });
            if parsed.is_err() {
                return None;
            }
            if matches {
                let Ok(ordinal) = u16::try_from(ordinal) else {
                    return None;
                };
                let index = TypeIdx(ordinal);
                if row.presentation {
                    return Some(index);
                }
                if first.is_none() {
                    first = Some(index);
                }
            }
        }
        first
    }
    pub fn properties(&self, index: TypeIdx) -> Option<impl Iterator<Item = PropertyNames<'_>>> {
        let row = self.types.get(usize::from(index.0))?;
        let mut reader = Reader::new(self.raw.get(row.properties.clone())?);
        let Ok(count) = reader.count() else {
            return None;
        };
        Some((0..count).filter_map(move |_| {
            let Ok((ru, en)) = reader.names() else {
                return None;
            };
            if reader.boolean().is_err()
                || reader.boolean().is_err()
                || reader.avail().is_err()
                || reader.vector(Reader::result_type).is_err()
            {
                return None;
            }
            let Ok(name_ru) = std::str::from_utf8(reader.bytes.get(ru)?) else {
                return None;
            };
            let Ok(name_en) = std::str::from_utf8(reader.bytes.get(en)?) else {
                return None;
            };
            Some(PropertyNames { name_ru, name_en })
        }))
    }
    pub fn find_property(&self, index: TypeIdx, name: &str) -> Option<PropertyNames<'_>> {
        self.properties(index)?
            .find(|p| same(p.name_ru, name) || same(p.name_en, name))
    }
}
fn folded(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().map(|c| {
        let mut lower = c.to_lowercase();
        match (lower.next(), lower.next()) {
            (Some(one), None) => one,
            _ => c,
        }
    })
}
fn same_parts(a: &str, parts: &[&str]) -> bool {
    folded(a).eq(parts.iter().flat_map(|part| folded(part)))
}
fn same(a: &str, b: &str) -> bool {
    folded(a).eq(folded(b))
}
fn format<T>(message: &'static str) -> Result<T> {
    Err(ManagedCatalogError::Format(message))
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(count).ok_or(BudgetError::Overflow)?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .ok_or(ManagedCatalogError::Format("unexpected end of data"))?;
        self.at = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn boolean(&mut self) -> Result<()> {
        if self.byte()? > 1 {
            return format("bad boolean byte");
        }
        Ok(())
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn avail(&mut self) -> Result<()> {
        let b = self.take(2)?;
        if u16::from_le_bytes([b[0], b[1]]) & !Avail::ALL.0 != 0 {
            return format("unknown availability bits");
        }
        Ok(())
    }
    fn count(&mut self) -> Result<usize> {
        let count = usize::try_from(self.u32()?).map_err(|_| BudgetError::Overflow)?;
        if count > self.bytes.len() - self.at {
            return format("element count exceeds remaining data");
        }
        Ok(count)
    }
    fn string(&mut self) -> Result<Range<usize>> {
        let count = usize::try_from(self.u32()?).map_err(|_| BudgetError::Overflow)?;
        let start = self.at;
        std::str::from_utf8(self.take(count)?)
            .map_err(|_| ManagedCatalogError::Format("invalid UTF-8"))?;
        Ok(start..self.at)
    }
    fn names(&mut self) -> Result<(Range<usize>, Range<usize>)> {
        Ok((self.string()?, self.string()?))
    }
    fn option<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<Option<T>> {
        match self.byte()? {
            0 => Ok(None),
            1 => f(self).map(Some),
            _ => format("bad option byte"),
        }
    }
    fn vector<T>(&mut self, mut f: impl FnMut(&mut Self) -> Result<T>) -> Result<()> {
        for _ in 0..self.count()? {
            f(self)?;
        }
        Ok(())
    }
    fn signature(&mut self) -> Result<()> {
        self.vector(|r| {
            r.names()?;
            r.boolean()?;
            r.boolean()
        })?;
        self.option(|r| r.vector(Self::result_type))?;
        Ok(())
    }
    fn result_type(&mut self) -> Result<()> {
        self.option(Self::string)?;
        self.string()?;
        self.option(Self::string)?;
        Ok(())
    }
    fn property(&mut self) -> Result<()> {
        self.names()?;
        self.boolean()?;
        self.boolean()?;
        self.avail()?;
        self.vector(Self::result_type)
    }
    fn method(&mut self) -> Result<()> {
        self.names()?;
        self.vector(Self::signature)?;
        self.boolean()?;
        self.avail()
    }
    fn type_row(&mut self) -> Result<TypeRow> {
        let id = self.string()?;
        let (ru, en) = self.names()?;
        if self.byte()? > 2 {
            return format("bad type kind");
        }
        self.avail()?;
        self.boolean()?;
        self.option(|r| {
            r.boolean()?;
            r.boolean()
        })?;
        self.boolean()?;
        self.boolean()?;
        self.option(Self::names)?;
        self.option(|r| r.take(16))?;
        self.option(Self::string)?;
        self.option(Self::string)?;
        let presentation = self.option(Self::string)?.is_some();
        self.option(Self::string)?;
        self.option(Self::boolean)?;
        self.option(Self::boolean)?;
        self.option(Self::boolean)?;
        self.option(|r| {
            if r.byte()? > 2 {
                format("bad constructor error")
            } else {
                Ok(())
            }
        })?;
        self.option(Self::string)?;
        self.option(Self::string)?;
        self.boolean()?;
        self.option(|r| {
            r.boolean()?;
            r.boolean()?;
            r.boolean()
        })?;
        let start = self.at;
        self.vector(Self::string)?;
        let aliases = start..self.at;
        let start = self.at;
        self.vector(Self::property)?;
        let properties = start..self.at;
        self.vector(Self::method)?;
        self.vector(Self::signature)?;
        self.vector(|r| {
            r.names()?;
            r.option(Self::u32)?;
            r.option(Self::string)?;
            r.option(Self::string)?;
            Ok(())
        })?;
        Ok(TypeRow {
            id,
            ru,
            en,
            presentation,
            aliases,
            properties,
        })
    }
    fn global_method(&mut self) -> Result<()> {
        self.method()?;
        self.boolean()?;
        self.boolean()?;
        self.boolean()?;
        self.byte()?;
        self.option(Self::byte)?;
        self.boolean()
    }
    fn table(&mut self) -> Result<()> {
        self.string()?;
        self.option(Self::string)?;
        self.names()?;
        self.vector(|r| {
            r.names()?;
            r.vector(|r| {
                r.option(Self::string)?;
                r.names()?;
                Ok(())
            })
        })?;
        self.vector(|r| {
            r.names()?;
            r.boolean()
        })
    }
}
