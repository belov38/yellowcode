pub use crate::preview_wire::WriteError;
use crate::{
    DateParts, LStr, Resolved, ResolvedView, TitleView, TypeDesc, TypeNamesView, TypeResolver,
    preview_wire::Requests,
};
use std::cell::Cell;

#[derive(Debug, PartialEq, Eq)]
pub struct InvalidAnswers;

struct Writer<F>(F);
impl<E, F: FnMut(&[u8]) -> Result<(), E>> Writer<F> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), WriteError<E>> {
        (self.0)(bytes).map_err(WriteError::Sink)
    }
    fn count(&mut self, count: usize) -> Result<(), WriteError<E>> {
        self.put(
            &u64::try_from(count)
                .map_err(|_| WriteError::Overflow)?
                .to_le_bytes(),
        )
    }
    fn text(&mut self, text: &str) -> Result<(), WriteError<E>> {
        self.count(text.len())?;
        self.put(text.as_bytes())
    }
    fn type_name(&mut self, name: &mdconf::XmlTypeName) -> Result<(), WriteError<E>> {
        let uri = name.namespace_uri();
        let fixed = mdconf::FixedNamespace::from_uri(uri);
        let parts: [&str; 4] = match fixed {
            Some(ns) => [ns.prefix(), ":", "", &name.local],
            None => ["{", uri, "}", &name.local],
        };
        let length = parts
            .iter()
            .try_fold(0usize, |sum, part| sum.checked_add(part.len()))
            .ok_or(WriteError::Overflow)?;
        self.count(length)?;
        for part in parts {
            self.put(part.as_bytes())?;
        }
        Ok(())
    }
    fn optional_u32(&mut self, value: Option<u32>) -> Result<(), WriteError<E>> {
        self.put(&[u8::from(value.is_some())])?;
        if let Some(value) = value {
            self.put(&value.to_le_bytes())?;
        }
        Ok(())
    }
}
pub fn encode_header<E>(
    count: usize,
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut out = Writer(sink);
    out.put(b"ANS1")?;
    out.count(count)
}
pub fn encode_answer<E>(
    ordinal: usize,
    value: Option<ResolvedView<'_>>,
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let mut out = Writer(sink);
    out.count(ordinal)?;
    let Some(value) = value else {
        return out.put(&[0]);
    };
    out.put(&[1])?;
    match value.ty.types {
        TypeNamesView::Empty => out.count(0)?,
        TypeNamesView::Declared(names) => {
            out.count(names.len())?;
            for name in names {
                out.type_name(&name.name)?;
            }
        }
        TypeNamesView::Scalar(name) => {
            out.count(1)?;
            out.text(name)?;
        }
        TypeNamesView::Reference { kind, name } => {
            out.count(1)?;
            let kind = kind.as_str();
            let length = 4usize
                .checked_add(kind.len())
                .and_then(|n| n.checked_add(4))
                .and_then(|n| n.checked_add(name.len()))
                .ok_or(WriteError::Overflow)?;
            out.count(length)?;
            for part in ["cfg:", kind, "Ref.", name] {
                out.put(part.as_bytes())?;
            }
        }
    }
    out.optional_u32(value.ty.string_len)?;
    out.put(&[u8::from(value.ty.number.is_some())])?;
    if let Some((digits, fraction)) = value.ty.number {
        out.put(&digits.to_le_bytes())?;
        out.put(&fraction.to_le_bytes())?;
    }
    out.put(&[match value.ty.date {
        None => 0,
        Some(DateParts::Date) => 1,
        Some(DateParts::Time) => 2,
        Some(DateParts::DateTime) => 3,
    }])?;
    out.put(&[u8::from(value.title.is_some())])?;
    match value.title {
        None => {}
        Some(TitleView::Declared(pairs)) => {
            out.count(pairs.len())?;
            for (language, title) in pairs {
                out.text(language)?;
                out.text(title)?;
            }
        }
        Some(TitleView::Russian(title)) => {
            out.count(1)?;
            out.text("ru")?;
            out.text(title)?;
        }
    }
    Ok(())
}

struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], InvalidAnswers> {
        let bytes = self.0.get(..count).ok_or(InvalidAnswers)?;
        self.0 = self.0.get(count..).ok_or(InvalidAnswers)?;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8, InvalidAnswers> {
        self.take(1)?.first().copied().ok_or(InvalidAnswers)
    }
    fn count(&mut self) -> Result<usize, InvalidAnswers> {
        let raw = self.take(8)?.try_into().map_err(|_| InvalidAnswers)?;
        usize::try_from(u64::from_le_bytes(raw)).map_err(|_| InvalidAnswers)
    }
    fn bounded_count(&mut self, minimum_row: usize) -> Result<usize, InvalidAnswers> {
        let count = self.count()?;
        if count > self.0.len() / minimum_row {
            return Err(InvalidAnswers);
        }
        Ok(count)
    }
    fn text(&mut self) -> Result<String, InvalidAnswers> {
        let count = self.count()?;
        let bytes = self.take(count)?;
        Ok(std::str::from_utf8(bytes)
            .map_err(|_| InvalidAnswers)?
            .to_owned())
    }
    fn u32(&mut self) -> Result<u32, InvalidAnswers> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| InvalidAnswers)?,
        ))
    }
    fn optional_u32(&mut self) -> Result<Option<u32>, InvalidAnswers> {
        match self.byte()? {
            0 => Ok(None),
            1 => self.u32().map(Some),
            _ => Err(InvalidAnswers),
        }
    }
    fn resolved(&mut self) -> Result<Resolved, InvalidAnswers> {
        let count = self.bounded_count(8)?;
        let mut types = Vec::with_capacity(count);
        for _ in 0..count {
            types.push(self.text()?);
        }
        let string_len = self.optional_u32()?;
        let number = match self.byte()? {
            0 => None,
            1 => Some((self.u32()?, self.u32()?)),
            _ => return Err(InvalidAnswers),
        };
        let date = match self.byte()? {
            0 => None,
            1 => Some(DateParts::Date),
            2 => Some(DateParts::Time),
            3 => Some(DateParts::DateTime),
            _ => return Err(InvalidAnswers),
        };
        let title = match self.byte()? {
            0 => None,
            1 => {
                let count = self.bounded_count(16)?;
                let mut pairs = Vec::with_capacity(count);
                for _ in 0..count {
                    pairs.push((self.text()?, self.text()?));
                }
                Some(LStr(pairs))
            }
            _ => return Err(InvalidAnswers),
        };
        Ok(Resolved {
            ty: TypeDesc {
                types,
                string_len,
                number,
                date,
            },
            title,
        })
    }
}
enum Slot {
    Unset,
    Miss,
    Value(Resolved),
}
pub struct Answers<'a> {
    requests: Requests<'a>,
    rows: Vec<Slot>,
    unknown: Cell<bool>,
}
impl Answers<'_> {
    pub fn has_unknown_request(&self) -> bool {
        self.unknown.get()
    }
}
impl TypeResolver for Answers<'_> {
    fn resolve(&self, owner: &str, path: &[&str]) -> Option<Resolved> {
        for (request, value) in self.requests.iter().zip(&self.rows) {
            let Ok(request) = request else {
                self.unknown.set(true);
                return None;
            };
            if request.owner == owner && request.segments().eq(path.iter().map(|s| Ok(*s))) {
                return match value {
                    Slot::Value(value) => Some(value.clone()),
                    Slot::Miss => None,
                    Slot::Unset => {
                        self.unknown.set(true);
                        None
                    }
                };
            }
        }
        self.unknown.set(true);
        None
    }
}
pub fn decode<'a>(bytes: &[u8], requests: Requests<'a>) -> Result<Answers<'a>, InvalidAnswers> {
    let mut input = Reader(bytes);
    if input.take(4)? != b"ANS1" {
        return Err(InvalidAnswers);
    }
    let count = input.bounded_count(9)?;
    if count != requests.len() {
        return Err(InvalidAnswers);
    }
    for (ordinal, request) in requests.iter().enumerate() {
        let request = request.map_err(|_| InvalidAnswers)?;
        for previous in requests.iter().take(ordinal) {
            let previous = previous.map_err(|_| InvalidAnswers)?;
            if request.owner == previous.owner && request.segments().eq(previous.segments()) {
                return Err(InvalidAnswers);
            }
        }
    }
    let mut rows = Vec::with_capacity(count);
    rows.resize_with(count, || Slot::Unset);
    for _ in 0..count {
        let ordinal = input.count()?;
        let slot = rows.get_mut(ordinal).ok_or(InvalidAnswers)?;
        if !matches!(slot, Slot::Unset) {
            return Err(InvalidAnswers);
        }
        *slot = match input.byte()? {
            0 => Slot::Miss,
            1 => Slot::Value(input.resolved()?),
            _ => return Err(InvalidAnswers),
        };
    }
    if !input.0.is_empty() || rows.iter().any(|slot| matches!(slot, Slot::Unset)) {
        return Err(InvalidAnswers);
    }
    Ok(Answers {
        requests,
        rows,
        unknown: Cell::new(false),
    })
}
