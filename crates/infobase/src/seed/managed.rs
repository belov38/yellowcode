use super::{Seed, SeedDataError, SeedDataErrorKind as Kind, SeedFile, SeedMemoryError};
use ibschema::dbnames::{DbName, DbNames};
use ibschema::logical::{Field, Index, KeyKind, LType, LogicalSchema, Table, TableKind, TypeCode};
use memory_budget::{BudgetError, Category, MemoryBudget};
use onecd::{FieldDescr, FieldType, IndexDescr, IndexField, TableDescr};

type Result<T> = std::result::Result<T, SeedMemoryError>;

#[derive(Clone, Copy)]
struct Cursor<'a> {
    text: &'a str,
    at: usize,
    file: SeedFile,
    fill: bool,
    bytes: u64,
}
impl<'a> Cursor<'a> {
    fn error(&self, kind: Kind) -> SeedMemoryError {
        SeedMemoryError::Embedded(SeedDataError {
            file: self.file,
            offset: self.at,
            kind,
        })
    }
    fn white(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.at)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.at += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.white();
        self.text.as_bytes().get(self.at).copied()
    }
    fn take(&mut self, wanted: u8) -> Result<()> {
        if self.peek() != Some(wanted) {
            return Err(self.error(Kind::Shape));
        }
        self.at += 1;
        Ok(())
    }
    fn literal(&mut self, word: &str) -> Result<()> {
        if !self
            .text
            .get(self.at..)
            .is_some_and(|tail| tail.starts_with(word))
        {
            return Err(self.error(Kind::Shape));
        }
        self.at += word.len();
        Ok(())
    }
    fn word(&mut self, word: &str) -> Result<()> {
        self.white();
        self.literal(word)
    }
    fn field(&mut self, key: &str) -> Result<()> {
        self.white();
        let previous = self
            .text
            .as_bytes()
            .get(..self.at)
            .and_then(|s| s.iter().rev().find(|b| !b.is_ascii_whitespace()))
            .copied();
        if previous != Some(b'{') {
            self.take(b',')?;
        }
        self.take(b'"')?;
        self.literal(key)?;
        self.take(b'"')?;
        self.take(b':')
    }
    fn add(&mut self, count: usize, size: usize) -> Result<()> {
        let bytes = count
            .checked_mul(size)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(BudgetError::Overflow)?;
        self.bytes = self.bytes.checked_add(bytes).ok_or(BudgetError::Overflow)?;
        Ok(())
    }
    fn hex(&mut self) -> Result<u16> {
        let mut value = 0u16;
        for _ in 0..4 {
            let byte = self
                .text
                .as_bytes()
                .get(self.at)
                .copied()
                .ok_or_else(|| self.error(Kind::String))?;
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(self.error(Kind::String)),
            };
            value = (value << 4) | u16::from(digit);
            self.at += 1;
        }
        Ok(value)
    }
    fn decoded(&mut self, mut output: Option<&mut String>) -> Result<usize> {
        self.take(b'"')?;
        let mut length = 0usize;
        loop {
            let byte = self
                .text
                .as_bytes()
                .get(self.at)
                .copied()
                .ok_or_else(|| self.error(Kind::String))?;
            if byte == b'"' {
                self.at += 1;
                return Ok(length);
            }
            let ch = if byte == b'\\' {
                self.at += 1;
                let escaped = self
                    .text
                    .as_bytes()
                    .get(self.at)
                    .copied()
                    .ok_or_else(|| self.error(Kind::String))?;
                self.at += 1;
                match escaped {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{8}',
                    b'f' => '\u{c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'u' => {
                        let first = self.hex()?;
                        let code = if (0xd800..=0xdbff).contains(&first) {
                            self.literal("\\u")?;
                            let second = self.hex()?;
                            if !(0xdc00..=0xdfff).contains(&second) {
                                return Err(self.error(Kind::String));
                            }
                            0x10000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second)
                                - 0xdc00
                        } else {
                            u32::from(first)
                        };
                        char::from_u32(code).ok_or_else(|| self.error(Kind::String))?
                    }
                    _ => return Err(self.error(Kind::String)),
                }
            } else {
                if byte < 0x20 {
                    return Err(self.error(Kind::String));
                }
                let ch = self
                    .text
                    .get(self.at..)
                    .and_then(|s| s.chars().next())
                    .ok_or_else(|| self.error(Kind::String))?;
                self.at += ch.len_utf8();
                ch
            };
            length = length
                .checked_add(ch.len_utf8())
                .ok_or(BudgetError::Overflow)?;
            if let Some(output) = output.as_deref_mut() {
                output.push(ch);
            }
        }
    }
    fn string(&mut self) -> Result<String> {
        self.prefixed("")
    }
    fn prefixed(&mut self, prefix: &str) -> Result<String> {
        let start = self.at;
        let length = self
            .decoded(None)?
            .checked_add(prefix.len())
            .ok_or(BudgetError::Overflow)?;
        self.add(length, 1)?;
        if !self.fill {
            return Ok(String::new());
        }
        let mut output = String::new();
        output
            .try_reserve_exact(length)
            .map_err(|_| BudgetError::AllocationFailed)?;
        checkpoint();
        output.push_str(prefix);
        self.at = start;
        self.decoded(Some(&mut output))?;
        Ok(output)
    }
    fn tag(&mut self) -> Result<&'a str> {
        self.take(b'"')?;
        let start = self.at;
        while self
            .text
            .as_bytes()
            .get(self.at)
            .is_some_and(u8::is_ascii_alphanumeric)
        {
            self.at += 1;
        }
        let value = self
            .text
            .get(start..self.at)
            .ok_or_else(|| self.error(Kind::Shape))?;
        self.take(b'"')?;
        Ok(value)
    }
    fn int(&mut self) -> Result<i64> {
        self.white();
        let start = self.at;
        if self.text.as_bytes().get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        while self
            .text
            .as_bytes()
            .get(self.at)
            .is_some_and(u8::is_ascii_digit)
        {
            self.at += 1;
        }
        self.text
            .get(start..self.at)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| self.error(Kind::Number))
    }
    fn u32(&mut self) -> Result<u32> {
        u32::try_from(self.int()?).map_err(|_| self.error(Kind::Number))
    }
    fn boolean(&mut self) -> Result<bool> {
        match self.peek() {
            Some(b't') => {
                self.word("true")?;
                Ok(true)
            }
            Some(b'f') => {
                self.word("false")?;
                Ok(false)
            }
            _ => Err(self.error(Kind::Shape)),
        }
    }
    fn skip(&mut self, depth: usize) -> Result<()> {
        if depth > 64 {
            return Err(self.error(Kind::Depth));
        }
        match self.peek() {
            Some(b'"') => {
                self.decoded(None)?;
            }
            Some(b'[') | Some(b'{') => {
                let open = self.peek();
                self.at += 1;
                let end = if open == Some(b'[') { b']' } else { b'}' };
                if self.peek() == Some(end) {
                    self.at += 1;
                    return Ok(());
                }
                loop {
                    if end == b'}' {
                        self.decoded(None)?;
                        self.take(b':')?;
                    }
                    self.skip(depth + 1)?;
                    if self.peek() == Some(end) {
                        self.at += 1;
                        break;
                    }
                    self.take(b',')?;
                }
            }
            Some(b't') => self.word("true")?,
            Some(b'f') => self.word("false")?,
            Some(b'n') => self.word("null")?,
            Some(_) => {
                self.int()?;
            }
            None => return Err(self.error(Kind::Shape)),
        }
        Ok(())
    }
    fn array<T>(&mut self, read: fn(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        self.take(b'[')?;
        let mut probe = *self;
        let mut count = 0usize;
        if probe.peek() != Some(b']') {
            loop {
                probe.skip(0)?;
                count = count.checked_add(1).ok_or(BudgetError::Overflow)?;
                if probe.peek() == Some(b']') {
                    break;
                }
                probe.take(b',')?;
            }
        }
        self.add(count, std::mem::size_of::<T>())?;
        let mut out = Vec::new();
        if self.fill {
            out.try_reserve_exact(count)
                .map_err(|_| BudgetError::AllocationFailed)?;
            checkpoint();
        }
        for i in 0..count {
            if i != 0 {
                self.take(b',')?;
            }
            let value = read(self)?;
            if self.fill {
                out.push(value);
            }
        }
        self.take(b']')?;
        Ok(out)
    }
    fn end(&mut self) -> Result<()> {
        if self.peek().is_some() {
            Err(self.error(Kind::Shape))
        } else {
            Ok(())
        }
    }
}

fn checkpoint() {
    #[cfg(test)]
    super::memory_tests::construction_checkpoint();
}

fn physical_field(p: &mut Cursor<'_>) -> Result<FieldDescr> {
    p.take(b'{')?;
    p.field("name")?;
    let name = p.string()?;
    p.field("ty")?;
    let ty = FieldType::from_code(p.tag()?).ok_or_else(|| p.error(Kind::Shape))?;
    p.field("length")?;
    let length = p.u32()?;
    p.field("precision")?;
    let precision = p.u32()?;
    p.field("nullable")?;
    let nullable = p.boolean()?;
    p.field("cs")?;
    let case_sensitive = p.boolean()?;
    p.take(b'}')?;
    Ok(FieldDescr {
        name,
        ty,
        length,
        precision,
        nullable,
        case_sensitive,
    })
}
fn index_field(p: &mut Cursor<'_>) -> Result<IndexField> {
    p.take(b'[')?;
    let name = p.string()?;
    p.take(b',')?;
    let prefix_len = p.u32()?;
    p.take(b']')?;
    Ok(IndexField { name, prefix_len })
}
fn physical_index(p: &mut Cursor<'_>) -> Result<IndexDescr> {
    p.take(b'{')?;
    p.field("name")?;
    let name = p.string()?;
    p.field("unique")?;
    let unique = p.boolean()?;
    p.field("fields")?;
    let fields = p.array(index_field)?;
    p.take(b'}')?;
    Ok(IndexDescr {
        name,
        unique,
        fields,
    })
}
fn physical(p: &mut Cursor<'_>) -> Result<TableDescr> {
    p.take(b'{')?;
    p.field("name")?;
    let name = p.string()?;
    p.field("recordlock")?;
    let recordlock = p.boolean()?;
    p.field("fields")?;
    let fields = p.array(physical_field)?;
    p.field("indexes")?;
    let indexes = p.array(physical_index)?;
    p.take(b'}')?;
    Ok(TableDescr {
        name,
        recordlock,
        fields,
        indexes,
        files: [0; 3],
    })
}
fn logical_type(p: &mut Cursor<'_>) -> Result<LType> {
    p.take(b'[')?;
    let code = match p.tag()? {
        "B" => TypeCode::B,
        "L" => TypeCode::L,
        "N" => TypeCode::N,
        "S" => TypeCode::S,
        "T" => TypeCode::T,
        "R" => TypeCode::R,
        "E" => TypeCode::E,
        "V" => TypeCode::V,
        _ => return Err(p.error(Kind::Shape)),
    };
    p.take(b',')?;
    let length = p.u32()?;
    p.take(b',')?;
    let precision = p.u32()?;
    p.take(b',')?;
    let ref_table = p.string()?;
    p.take(b',')?;
    let ref_kind = p.u32()?;
    p.take(b',')?;
    let extra = if p.peek() == Some(b'n') {
        p.word("null")?;
        None
    } else {
        Some(p.int()?)
    };
    p.take(b']')?;
    Ok(LType {
        code,
        length,
        precision,
        ref_table,
        ref_kind,
        extra,
    })
}
fn logical_field(p: &mut Cursor<'_>) -> Result<Field> {
    p.take(b'{')?;
    p.field("name")?;
    let name = p.string()?;
    p.field("nullable")?;
    let nullable = p.boolean()?;
    p.field("types")?;
    let types = p.array(logical_type)?;
    p.field("tail")?;
    p.take(b'[')?;
    let tail_s = p.string()?;
    p.take(b',')?;
    let tail_n = p.int()?;
    p.take(b']')?;
    p.take(b'}')?;
    Ok(Field {
        name,
        nullable,
        types,
        tail_s,
        tail_n,
    })
}
fn strings(p: &mut Cursor<'_>) -> Result<Vec<String>> {
    p.array(Cursor::string)
}
fn groups(p: &mut Cursor<'_>) -> Result<Vec<Vec<String>>> {
    p.array(strings)
}
fn logical_index(p: &mut Cursor<'_>) -> Result<Index> {
    p.take(b'{')?;
    p.field("name")?;
    let name = p.string()?;
    p.field("unique")?;
    let unique = p.boolean()?;
    p.field("fields")?;
    let fields = strings(p)?;
    p.field("clustered")?;
    let clustered = p.boolean()?;
    p.field("flag2")?;
    let flag2 = p.boolean()?;
    p.field("rest")?;
    p.take(b'[')?;
    let n3 = p.int()?;
    p.take(b',')?;
    let list = p.array(Cursor::int)?;
    p.take(b',')?;
    let n4 = p.int()?;
    p.take(b',')?;
    let n5 = p.int()?;
    p.take(b']')?;
    p.take(b'}')?;
    Ok(Index {
        name,
        unique,
        fields,
        clustered,
        flag2,
        n3,
        list,
        n4,
        n5,
    })
}
fn logical(p: &mut Cursor<'_>) -> Result<Table> {
    p.take(b'{')?;
    p.field("name")?;
    let name = p.string()?;
    p.field("kind")?;
    let kind = match p.tag()? {
        "N" => TableKind::Main,
        "I" => TableKind::Nested,
        _ => return Err(p.error(Kind::Shape)),
    };
    p.field("number")?;
    let number = p.u32()?;
    p.field("parent")?;
    let parent = p.string()?;
    p.field("fields")?;
    let fields = p.array(logical_field)?;
    p.field("nested")?;
    let nested = p.array(logical)?;
    p.field("indexes")?;
    let indexes = p.array(logical_index)?;
    p.field("one")?;
    let one = p.int()?;
    p.field("key")?;
    let key = match p.tag()? {
        "R" => KeyKind::Ref,
        "S" => KeyKind::Plain,
        _ => return Err(p.error(Kind::Shape)),
    };
    p.field("splitters")?;
    p.take(b'[')?;
    let a = groups(p)?;
    p.take(b',')?;
    let b = groups(p)?;
    p.take(b']')?;
    p.field("tail")?;
    p.take(b'[')?;
    let tail_s = p.string()?;
    p.take(b',')?;
    let tail_n1 = p.int()?;
    p.take(b',')?;
    let tail_n2 = p.int()?;
    p.take(b']')?;
    p.take(b'}')?;
    Ok(Table {
        name,
        kind,
        number,
        parent,
        fields,
        nested,
        indexes,
        one,
        key,
        splitters: [a, b],
        tail_s,
        tail_n1,
        tail_n2,
    })
}
fn name(p: &mut Cursor<'_>) -> Result<DbName> {
    p.take(b'[')?;
    let purpose = p.string()?;
    p.take(b',')?;
    let number = p.u32()?;
    p.take(b']')?;
    Ok(DbName {
        uuid: brace::Guid::NIL,
        purpose,
        number,
    })
}
fn parameter(p: &mut Cursor<'_>) -> Result<super::ParamsTemplate> {
    p.take(b'{')?;
    p.field("name")?;
    let name = p.string()?;
    p.field("text")?;
    let text = p.string()?;
    p.field("deflate")?;
    let deflate = p.boolean()?;
    p.take(b'}')?;
    Ok(super::ParamsTemplate {
        name,
        text,
        deflate,
    })
}
fn pair(p: &mut Cursor<'_>) -> Result<(u32, u32)> {
    p.take(b'[')?;
    let a = p.u32()?;
    p.take(b',')?;
    let b = p.u32()?;
    p.take(b']')?;
    Ok((a, b))
}
fn cursor(text: &str, file: SeedFile, fill: bool) -> Cursor<'_> {
    Cursor {
        text,
        at: 0,
        file,
        fill,
        bytes: 0,
    }
}

fn parse(fill: bool) -> Result<(Seed, u64)> {
    let mut p = cursor(super::TABLES_JSON, SeedFile::Tables, fill);
    let tables = p.array(physical)?;
    p.end()?;
    let mut bytes = p.bytes;
    p = cursor(super::SCHEMA_JSON, SeedFile::Schema, fill);
    let schema = LogicalSchema {
        tables: p.array(logical)?,
    };
    p.end()?;
    bytes = bytes.checked_add(p.bytes).ok_or(BudgetError::Overflow)?;
    p = cursor(super::DBNAMES_JSON, SeedFile::DbNames, fill);
    let mut probe = p;
    probe.take(b'[')?;
    let mut count = 0usize;
    if probe.peek() != Some(b']') {
        loop {
            probe.skip(0)?;
            count = count.checked_add(1).ok_or(BudgetError::Overflow)?;
            if probe.peek() == Some(b']') {
                break;
            }
            probe.take(b',')?;
        }
    }
    let entries = p.array(name)?;
    p.end()?;
    p.add(
        count,
        2usize
            .checked_mul(std::mem::size_of::<usize>())
            .ok_or(BudgetError::Overflow)?,
    )?;
    let mut by_key = Vec::new();
    let mut by_num = Vec::new();
    if fill {
        by_key
            .try_reserve_exact(count)
            .map_err(|_| BudgetError::AllocationFailed)?;
        checkpoint();
        by_num
            .try_reserve_exact(count)
            .map_err(|_| BudgetError::AllocationFailed)?;
        checkpoint();
        for i in 0..count {
            by_key.push(i);
            by_num.push(i);
        }
        by_key.sort_unstable_by(|&a, &b| {
            entries
                .get(a)
                .map(|e| (e.uuid, e.purpose.as_str()))
                .cmp(&entries.get(b).map(|e| (e.uuid, e.purpose.as_str())))
        });
        by_num.sort_unstable_by_key(|&i| entries.get(i).map(|e| e.number));
    }
    let dbnames = DbNames::from_indexed_entries(entries, by_key, by_num)
        .map_err(|_| p.error(Kind::DbNames))?;
    bytes = bytes.checked_add(p.bytes).ok_or(BudgetError::Overflow)?;
    p = cursor(super::PARAMS_JSON, SeedFile::Params, fill);
    let params = p.array(parameter)?;
    p.end()?;
    bytes = bytes.checked_add(p.bytes).ok_or(BudgetError::Overflow)?;
    p = cursor(super::ROWS_JSON, SeedFile::Rows, fill);
    p.take(b'{')?;
    p.field("ibversion")?;
    let ibversion = pair(&mut p)?;
    p.field("schemastorage")?;
    let schemastorage = pair(&mut p)?;
    p.field("newgen")?;
    let newgen = p.prefixed("\u{feff}")?.into_bytes();
    p.take(b'}')?;
    p.end()?;
    bytes = bytes.checked_add(p.bytes).ok_or(BudgetError::Overflow)?;
    Ok((
        Seed {
            tables,
            schema,
            dbnames,
            params,
            ibversion,
            newgen,
            schemastorage,
        },
        bytes,
    ))
}

pub(super) fn build(memory: Option<&MemoryBudget>) -> Result<super::SeedEntry> {
    let (_, bytes) = parse(false)?;
    let charge = memory
        .map(|m| m.root_budget().try_reserve(Category::Metadata, bytes))
        .transpose()
        .map_err(BudgetError::from)?;
    let (value, actual) = parse(true)?;
    if bytes != actual {
        return Err(SeedMemoryError::Budget(BudgetError::Overflow));
    }
    Ok(super::SeedEntry {
        value,
        charge: memory_budget::Lock::new(charge),
        retained_bytes: bytes,
    })
}
