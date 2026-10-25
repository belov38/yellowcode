use super::brace::Node;
use crate::error::Result;
use memory_budget::{BudgetedVec, Category, Charged, MemoryBudget};

fn bad_descr(what: &'static str) -> crate::error::Error {
    crate::error::Error::CorruptStatic {
        location: crate::error::Location::Root,
        what,
    }
}

pub(crate) fn parse_brace(text: &str, budget: &MemoryBudget) -> Result<Charged<Node>> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
        budget,
    };
    let result = parser.value(0)?;
    parser.ws();
    if parser.pos != parser.bytes.len() {
        return Err(bad_descr("unexpected text after bracket value"));
    }
    Ok(result)
}
#[derive(Clone)]
struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    budget: &'a MemoryBudget,
}
impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    fn ws(&mut self) {
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_whitespace() && matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.pos += 1;
        }
    }
    fn scan(&mut self, depth: usize) -> Result<usize> {
        self.ws();
        match self.peek() {
            Some(b'{') => {
                if depth >= 64 {
                    return Err(bad_descr("bracket nesting exceeds 64"));
                }
                self.pos += 1;
                self.ws();
                let mut count = 0;
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                    return Ok(0);
                }
                loop {
                    self.scan(depth + 1)?;
                    count += 1;
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(count);
                        }
                        _ => return Err(bad_descr("expected comma or closing brace")),
                    }
                }
            }
            Some(b'"') => {
                self.pos += 1;
                let mut len = 0;
                loop {
                    match self.peek() {
                        None => return Err(bad_descr("unterminated quoted string")),
                        Some(b'"') => {
                            self.pos += 1;
                            if self.peek() != Some(b'"') {
                                return Ok(len);
                            }
                            self.pos += 1;
                            len += 1;
                        }
                        Some(_) => {
                            self.pos += 1;
                            len += 1;
                        }
                    }
                }
            }
            Some(b',' | b'}') | None => Err(bad_descr("expected bracket value")),
            Some(_) => {
                let start = self.pos;
                while self.peek().is_some_and(|b| {
                    !matches!(b, b',' | b'{' | b'}' | b'"' | b' ' | b'\t' | b'\r' | b'\n')
                }) {
                    self.pos += 1;
                }
                Ok(self.pos - start)
            }
        }
    }
    fn value(&mut self, depth: usize) -> Result<Charged<Node>> {
        self.ws();
        let mut scanner = self.clone();
        let count = scanner.scan(depth)?;
        match self.peek() {
            Some(b'{') => {
                let mut items =
                    BudgetedVec::<Node>::try_with_capacity(self.budget, Category::Metadata, count)?
                        .into_charged_vec();
                self.pos += 1;
                self.ws();
                for i in 0..count {
                    if i != 0 {
                        self.pos += 1;
                    }
                    let child = self.value(depth + 1)?;
                    items = items.try_zip(child)?.map(|(mut items, child)| {
                        items.push(child);
                        items
                    });
                    self.ws();
                }
                self.pos += 1;
                Ok(items.map(Node::List))
            }
            Some(b'"') => {
                let mut bytes =
                    BudgetedVec::try_with_capacity(self.budget, Category::Metadata, count)?;
                self.pos += 1;
                while self.pos < scanner.pos - 1 {
                    let b = self
                        .peek()
                        .ok_or_else(|| bad_descr("truncated metadata string"))?;
                    self.pos += 1;
                    bytes.try_push(b)?;
                    if b == b'"' {
                        self.pos += 1;
                    }
                }
                self.pos = scanner.pos;
                string(bytes).map(|v| v.map(Node::Str))
            }
            _ => {
                let mut bytes =
                    BudgetedVec::try_with_capacity(self.budget, Category::Metadata, count)?;
                bytes.try_extend_from_slice(
                    self.bytes
                        .get(self.pos..scanner.pos)
                        .ok_or_else(|| bad_descr("truncated metadata atom"))?,
                )?;
                self.pos = scanner.pos;
                string(bytes).map(|v| v.map(Node::Atom))
            }
        }
    }
}
fn string(bytes: BudgetedVec<u8>) -> Result<Charged<String>> {
    bytes
        .into_charged_vec()
        .try_map(String::from_utf8)
        .map_err(|_| bad_descr("invalid metadata UTF-8"))
}

use super::descr::{FieldDescr, FieldType, IndexDescr, IndexField, TableDescr};
use super::layout::{FieldSlot, RecordLayout, value_size};
use super::root::RootContent;

fn text(n: &Node) -> Result<&str> {
    if let Node::Str(s) = n {
        Ok(s)
    } else {
        Err(bad_descr("expected quoted metadata string"))
    }
}
fn list(n: &Node) -> Result<&[Node]> {
    if let Node::List(v) = n {
        Ok(v)
    } else {
        Err(bad_descr("expected metadata list"))
    }
}
fn number(n: &Node) -> Result<u32> {
    if let Node::Atom(s) = n
        && !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_digit())
    {
        return s
            .parse()
            .map_err(|_| bad_descr("metadata number out of range"));
    }
    Err(bad_descr("expected unsigned metadata number"))
}
fn flag(n: &Node) -> Result<bool> {
    match number(n)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(bad_descr("expected zero or one")),
    }
}
fn section<'a>(n: &'a Node, title: &str) -> Result<&'a [Node]> {
    match list(n)?.split_first() {
        Some((Node::Str(t), rest)) if t == title => Ok(rest),
        _ => Err(bad_descr("unexpected metadata section")),
    }
}
pub(crate) fn copy_string(s: &str, budget: &MemoryBudget) -> Result<Charged<String>> {
    let mut bytes = BudgetedVec::try_with_capacity(budget, Category::Metadata, s.len())?;
    bytes.try_extend_from_slice(s.as_bytes())?;
    string(bytes)
}
fn collect<T>(
    nodes: &[Node],
    budget: &MemoryBudget,
    mut convert: impl FnMut(&Node) -> Result<Charged<T>>,
) -> Result<Charged<Vec<T>>> {
    let mut out =
        BudgetedVec::try_with_capacity(budget, Category::Metadata, nodes.len())?.into_charged_vec();
    for n in nodes {
        out = out.try_zip(convert(n)?)?.map(|(mut out, value)| {
            out.push(value);
            out
        });
    }
    Ok(out)
}
pub(crate) fn parse_descr(source: &str, budget: &MemoryBudget) -> Result<Charged<TableDescr>> {
    let ast = parse_brace(source, budget)?;
    let [name, zero, fields, indexes, recordlock, files] = list(&ast)? else {
        return Err(bad_descr("expected six table description items"));
    };
    if !matches!(zero, Node::Atom(v) if v == "0") {
        return Err(bad_descr("second description item must be zero"));
    }
    let fields = collect(section(fields, "Fields")?, budget, |n| {
        let [name, ty, nullable, length, precision, case] = list(n)? else {
            return Err(bad_descr("expected six field items"));
        };
        let ty = FieldType::from_code(text(ty)?).ok_or_else(|| bad_descr("unknown field type"))?;
        let nullable = flag(nullable)?;
        let length = number(length)?;
        let precision = number(precision)?;
        let case_sensitive = match text(case)? {
            "CS" => true,
            "CI" => false,
            _ => return Err(bad_descr("unknown field case mode")),
        };
        Ok(copy_string(text(name)?, budget)?.map(|name| FieldDescr {
            name,
            ty,
            nullable,
            length,
            precision,
            case_sensitive,
        }))
    })?;
    let indexes = collect(section(indexes, "Indexes")?, budget, |n| {
        let parts = list(n)?;
        let [name, unique, rest @ ..] = parts else {
            return Err(bad_descr("index needs name, flag and fields"));
        };
        if rest.is_empty() {
            return Err(bad_descr("index needs fields"));
        }
        let unique = flag(unique)?;
        let fields = collect(rest, budget, |n| {
            let [name, prefix] = list(n)? else {
                return Err(bad_descr("index field needs name and prefix"));
            };
            let prefix_len = number(prefix)?;
            Ok(copy_string(text(name)?, budget)?.map(|name| IndexField { name, prefix_len }))
        })?;
        Ok(copy_string(text(name)?, budget)?
            .try_zip(fields)?
            .map(|(name, fields)| IndexDescr {
                name,
                unique,
                fields,
            }))
    })?;
    let recordlock = match section(recordlock, "Recordlock")? {
        [Node::Str(v)] if v == "0" => false,
        [Node::Str(v)] if v == "1" => true,
        _ => return Err(bad_descr("invalid Recordlock value")),
    };
    let files = match section(files, "Files")? {
        [a, b, c] => [number(a)?, number(b)?, number(c)?],
        _ => return Err(bad_descr("Files needs three numbers")),
    };
    let result = copy_string(text(name)?, budget)?
        .try_zip(fields)?
        .try_zip(indexes)?
        .map(|((name, fields), indexes)| TableDescr {
            name,
            fields,
            indexes,
            recordlock,
            files,
        });
    for index in &result.indexes {
        for field in &index.fields {
            let (_, descr) = result
                .field(&field.name)
                .ok_or_else(|| bad_descr("index refers to unknown field"))?;
            if descr.ty.is_blob() || descr.ty == FieldType::RV {
                return Err(bad_descr("field type cannot be indexed"));
            }
        }
    }
    Ok(result)
}
pub(crate) fn layout(d: &TableDescr, budget: &MemoryBudget) -> Result<Charged<RecordLayout>> {
    if d.fields.iter().any(|f| f.ty == FieldType::RV && f.nullable) {
        return Err(bad_descr("rowversion cannot be nullable"));
    }
    let has_rowversion = d.fields.iter().any(|f| f.ty == FieldType::RV);
    let has_hidden_version = d.recordlock && !has_rowversion;
    let header_len = if has_rowversion {
        17
    } else if has_hidden_version {
        9
    } else {
        1
    };
    let mut end = u64::from(header_len);
    let mut slots = BudgetedVec::try_with_capacity(budget, Category::Metadata, d.fields.len())?;
    for f in &d.fields {
        let size = value_size(f);
        let offset = if f.ty == FieldType::RV {
            1
        } else {
            u32::try_from(end).map_err(|_| bad_descr("record too long"))?
        };
        slots.try_push(FieldSlot {
            offset,
            value_size: size,
        })?;
        if f.ty != FieldType::RV {
            end += u64::from(size) + u64::from(f.nullable);
        }
        if end > 1 << 24 {
            return Err(bad_descr("record too long"));
        }
    }
    let record_len = end.max(5) as u32;
    Ok(slots.into_charged_vec().map(|slots| RecordLayout {
        record_len,
        header_len,
        has_rowversion,
        has_hidden_version,
        slots,
    }))
}
pub(crate) fn parse_root(
    version: [u8; 4],
    content: &[u8],
    budget: &MemoryBudget,
) -> Result<Charged<RootContent>> {
    use crate::error::{Error, Location};
    let invalid = || Error::CorruptStatic {
        location: Location::Root,
        what: "invalid root content",
    };
    let len = if version >= [8, 1, 0, 0] { 32 } else { 8 };
    let raw = content.get(..len).ok_or_else(invalid)?;
    let count = i32::from_le_bytes(
        content
            .get(len..len + 4)
            .and_then(|v| v.try_into().ok())
            .ok_or_else(invalid)?,
    );
    let count = usize::try_from(count).map_err(|_| invalid())?;
    let raw_refs = count
        .checked_mul(4)
        .and_then(|n| content.get(len + 4..len + 4 + n))
        .ok_or_else(invalid)?;
    let mut locale = BudgetedVec::try_with_capacity(
        budget,
        Category::Metadata,
        raw.iter()
            .take_while(|&&b| b != 0)
            .map(|b| if b.is_ascii() { 1 } else { 3 })
            .sum(),
    )?;
    for &b in raw.iter().take_while(|&&b| b != 0) {
        if b.is_ascii() {
            locale.try_push(b)?;
        } else {
            locale.try_extend_from_slice("�".as_bytes())?;
        }
    }
    let locale = string(locale)?;
    let mut refs = BudgetedVec::try_with_capacity(budget, Category::Metadata, count)?;
    for bytes in raw_refs.as_chunks::<4>().0 {
        refs.try_push(u32::from_le_bytes(*bytes))?;
    }
    Ok(locale
        .try_zip(refs.into_charged_vec())?
        .map(|(locale, table_refs)| RootContent { locale, table_refs }))
}
