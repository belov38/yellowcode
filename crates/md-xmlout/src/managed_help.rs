use brace::Guid;
use md_model::parts::Help;
use memory_budget::{BudgetError, BudgetedText, BudgetedWriter, Category, Charged, MemoryBudget};

type Result<T> = std::result::Result<T, BudgetError>;
fn find(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes.windows(needle.len()).position(|part| part == needle)
}
fn guid(bytes: &[u8]) -> Option<Guid> {
    if bytes.len() != 36
        || !bytes.iter().enumerate().all(|(at, byte)| {
            if [8, 13, 18, 23].contains(&at) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
            }
        })
    {
        return None;
    }
    Guid::parse(std::str::from_utf8(bytes).ok()?)
}
fn lossy_equal(mut text: &str, bytes: &[u8]) -> bool {
    for chunk in bytes.utf8_chunks() {
        let Some(tail) = text.strip_prefix(chunk.valid()) else {
            return false;
        };
        text = tail;
        if !chunk.invalid().is_empty() {
            let Some(tail) = text.strip_prefix('\u{fffd}') else {
                return false;
            };
            text = tail;
        }
    }
    text.is_empty()
}
struct Linker<'a, 'b> {
    resolve: &'b dyn Fn(Guid) -> Option<&'a str>,
    files: &'b [(String, Vec<u8>)],
}
impl Linker<'_, '_> {
    fn named(&self, bytes: &[u8]) -> Option<&str> {
        (self.resolve)(guid(bytes)?)
    }
    fn standard(&self, out: &mut BudgetedWriter, bytes: &[u8]) -> Result<bool> {
        let name = std::str::from_utf8(bytes).ok().and_then(|key| {
            mdschema::static_schema()
                .table("std_pictures.json")?
                .get("std_pictures")?
                .get(key)?
                .as_str()
        });
        if let Some(name) = name {
            out.try_write(b"StdPicture.")?;
            out.try_write(name.as_bytes())?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn value(&self, out: &mut BudgetedWriter, bytes: &[u8]) -> Result<()> {
        if let Some(link) = bytes.strip_prefix(b"../id")
            && let (Some(id), Some(b'/'), Some(page), Some(rest)) = (
                link.get(..36),
                link.get(36),
                link.get(37..73),
                link.get(73..),
            )
            && guid(page).is_some()
            && let Some(name) = self.named(id)
        {
            let template =
                name.starts_with("CommonTemplate.") || name.rsplit('.').nth(1) == Some("Template");
            out.try_write(name.as_bytes())?;
            out.try_write(if template { b"/Template" } else { b"/Help" })?;
            return out.try_write(rest);
        }
        if let Some(link) = bytes.strip_prefix(b"../../mdpicture/id")
            && link.len() == 73
            && link.get(36) == Some(&b'/')
            && link.get(37..).and_then(guid).is_some()
            && let Some(id) = link.get(..36).filter(|id| guid(id).is_some())
        {
            if let Some(name) = self.named(id) {
                return out.try_write(name.as_bytes());
            }
            if self.standard(out, id)? {
                return Ok(());
            }
        }
        if let Some(key) = bytes.strip_prefix(b"../../mdpicture/idn")
            && let Some(digits) = key.strip_prefix(b"-")
            && !digits.is_empty()
            && digits.iter().all(u8::is_ascii_digit)
            && self.standard(out, key)?
        {
            return Ok(());
        }
        if let (Some(id), Some(name)) = (
            bytes.get(..36),
            bytes.get(36..).and_then(|s| s.strip_prefix(b"_files/")),
        ) && guid(id).is_some()
            && self.files.iter().any(|(file, _)| lossy_equal(file, name))
        {
            out.try_write(b"_files/")?;
            return out.try_write(name);
        }
        out.try_write(bytes)
    }
    fn tag(&self, out: &mut BudgetedWriter, tag: &[u8], is_a: bool) -> Result<()> {
        let needle = if is_a {
            b"href=\"".as_slice()
        } else {
            b"src=\"".as_slice()
        };
        for (at, byte) in tag.iter().enumerate() {
            if !matches!(byte, 9..=13 | 32) {
                continue;
            }
            let start = at.checked_add(1).ok_or(BudgetError::Overflow)?;
            let end = start
                .checked_add(needle.len())
                .ok_or(BudgetError::Overflow)?;
            if !tag
                .get(start..end)
                .is_some_and(|part| part.eq_ignore_ascii_case(needle))
            {
                continue;
            }
            let tail = tag.get(end..).ok_or(BudgetError::Overflow)?;
            let Some(length) = tail.iter().position(|byte| *byte == b'"') else {
                continue;
            };
            out.try_write(tag.get(..end).ok_or(BudgetError::Overflow)?)?;
            self.value(out, tail.get(..length).ok_or(BudgetError::Overflow)?)?;
            return out.try_write(tail.get(length..).ok_or(BudgetError::Overflow)?);
        }
        out.try_write(tag)
    }
    fn part(&self, out: &mut BudgetedWriter, bytes: &[u8]) -> Result<()> {
        let mut pos = 0;
        let mut copied = 0;
        while let Some(relative) = find(bytes.get(pos..).ok_or(BudgetError::Overflow)?, b"<") {
            let at = pos.checked_add(relative).ok_or(BudgetError::Overflow)?;
            let rest = bytes.get(at..).ok_or(BudgetError::Overflow)?;
            let (name_len, is_a) = if rest.get(..2).is_some_and(|s| s.eq_ignore_ascii_case(b"<a")) {
                (2, true)
            } else if rest
                .get(..4)
                .is_some_and(|s| s.eq_ignore_ascii_case(b"<img"))
            {
                (4, false)
            } else {
                pos = at + 1;
                continue;
            };
            if rest
                .get(name_len)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                pos = at + 1;
                continue;
            }
            let Some(length) = rest
                .iter()
                .position(|b| *b == b'>')
                .and_then(|n| n.checked_add(1))
            else {
                break;
            };
            out.try_write(bytes.get(copied..at).ok_or(BudgetError::Overflow)?)?;
            self.tag(out, rest.get(..length).ok_or(BudgetError::Overflow)?, is_a)?;
            pos = at.checked_add(length).ok_or(BudgetError::Overflow)?;
            copied = pos;
        }
        out.try_write(bytes.get(copied..).ok_or(BudgetError::Overflow)?)
    }
    fn page(&self, out: &mut BudgetedWriter, bytes: &[u8]) -> Result<()> {
        let mut rest = bytes;
        while let Some(at) = find(rest, b"<!--") {
            let comment = rest.get(at..).ok_or(BudgetError::Overflow)?;
            let Some(end) = find(comment.get(4..).ok_or(BudgetError::Overflow)?, b"-->")
                .and_then(|n| n.checked_add(7))
            else {
                break;
            };
            self.part(out, rest.get(..at).ok_or(BudgetError::Overflow)?)?;
            out.try_write(comment.get(..end).ok_or(BudgetError::Overflow)?)?;
            rest = comment.get(end..).ok_or(BudgetError::Overflow)?;
        }
        self.part(out, rest)
    }
}
pub fn managed_help_page<'a>(
    budget: &MemoryBudget,
    html: &[u8],
    files: &[(String, Vec<u8>)],
    resolve: impl Fn(Guid) -> Option<&'a str>,
    limit: u64,
) -> Result<Charged<Vec<u8>>> {
    let mut out = BudgetedWriter::new(budget, limit)?;
    Linker {
        resolve: &resolve,
        files,
    }
    .page(&mut out, html)?;
    let mut bytes = out.into_buffer();
    let mut written = 0;
    for at in 0..bytes.len() {
        let byte = *bytes.get(at).ok_or(BudgetError::Overflow)?;
        if byte == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
            continue;
        }
        *bytes.get_mut(written).ok_or(BudgetError::Overflow)? = byte;
        written += 1;
    }
    bytes.truncate(written);
    Ok(bytes.into_charged_vec())
}
pub fn managed_help_xml(budget: &MemoryBudget, help: &Help, limit: u64) -> Result<Charged<String>> {
    let mut out = BudgetedText::new(budget, Category::Metadata, limit)?;
    for part in [
        crate::codec::parts::XML_HEAD,
        "\r\n<Help xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"2.21\">\r\n",
    ] {
        out.try_push_str(part)?;
    }
    for (language, _) in &help.pages {
        for part in ["\t<Page>", language, "</Page>\r\n"] {
            out.try_push_str(part)?;
        }
    }
    out.try_push_str("</Help>")?;
    out.into_string()
}
