use brace::Guid;
use md_model::parts::Help;
use memory_budget::{BudgetError, BudgetedText, BudgetedWriter, Category, Charged, MemoryBudget};

type Result<T> = std::result::Result<T, BudgetError>;
fn find(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes.windows(needle.len()).position(|part| part == needle)
}
fn html_name_end(bytes: &[u8], mut at: usize) -> usize {
    while bytes
        .get(at)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':' | b'_'))
    {
        at += 1;
    }
    at
}
fn html_tag_end(bytes: &[u8], mut at: usize) -> Option<usize> {
    let mut quote = None;
    while let Some(&byte) = bytes.get(at) {
        if quote == Some(byte) {
            quote = None;
        } else if quote.is_none() {
            if matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
            } else if byte == b'>' {
                return Some(at + 1);
            }
        }
        at += 1;
    }
    None
}
fn html_raw_end(bytes: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let width = name.len().checked_add(2)?;
    let relative = bytes
        .get(from..)?
        .windows(width)
        .enumerate()
        .find_map(|(at, part)| {
            (part.starts_with(b"</")
                && part.get(2..)?.eq_ignore_ascii_case(name)
                && bytes
                    .get(from + at + width)
                    .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>')))
            .then_some(at)
        })?;
    html_tag_end(bytes, from + relative + width)
}
fn html_attribute(
    bytes: &[u8],
    mut at: usize,
    end: usize,
    wanted: &[u8],
) -> Option<(usize, usize)> {
    let bytes = bytes.get(..end)?;
    while at < end {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if bytes.get(at).is_none_or(|b| matches!(b, b'>' | b'/')) {
            break;
        }
        let name = at;
        while bytes
            .get(at)
            .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'=' | b'>' | b'/'))
        {
            at += 1;
        }
        let matching = bytes.get(name..at)?.eq_ignore_ascii_case(wanted);
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if bytes.get(at) != Some(&b'=') {
            continue;
        }
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let quote = bytes.get(at).copied().filter(|b| matches!(b, b'\'' | b'"'));
        if quote.is_some() {
            at += 1;
        }
        let start = at;
        while bytes.get(at).is_some_and(|b| match quote {
            Some(quote) => *b != quote,
            None => !b.is_ascii_whitespace() && *b != b'>',
        }) {
            at += 1;
        }
        if matching {
            return Some((start, at));
        }
        if quote.is_some() {
            at += 1;
        }
    }
    None
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
    syntax: HelpSyntax,
}
#[derive(Clone, Copy)]
enum HelpSyntax {
    Native,
    ExternalHtml,
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
            && self.files.iter().any(|(file, _)| {
                let path = match self.syntax {
                    HelpSyntax::Native => name,
                    HelpSyntax::ExternalHtml => name
                        .split(|byte| matches!(byte, b'#' | b'?'))
                        .next()
                        .unwrap_or(name),
                };
                lossy_equal(file, path)
            })
        {
            out.try_write(b"_files/")?;
            return out.try_write(name);
        }
        out.try_write(bytes)
    }
    fn tag(&self, out: &mut BudgetedWriter, tag: &[u8], is_a: bool) -> Result<()> {
        let wanted = if is_a {
            b"href".as_slice()
        } else {
            b"src".as_slice()
        };
        let name_end = html_name_end(tag, 1);
        if let Some((start, end)) = html_attribute(tag, name_end, tag.len(), wanted) {
            out.try_write(tag.get(..start).ok_or(BudgetError::Overflow)?)?;
            self.value(out, tag.get(start..end).ok_or(BudgetError::Overflow)?)?;
            out.try_write(tag.get(end..).ok_or(BudgetError::Overflow)?)
        } else {
            out.try_write(tag)
        }
    }
    fn part(&self, out: &mut BudgetedWriter, bytes: &[u8]) -> Result<()> {
        let mut pos = 0;
        let mut copied = 0;
        while let Some(relative) = find(bytes.get(pos..).ok_or(BudgetError::Overflow)?, b"<") {
            let at = pos.checked_add(relative).ok_or(BudgetError::Overflow)?;
            let rest = bytes.get(at..).ok_or(BudgetError::Overflow)?;
            if rest.starts_with(b"<!--") {
                pos = find(rest.get(4..).ok_or(BudgetError::Overflow)?, b"-->")
                    .map_or(bytes.len(), |end| at + 4 + end + 3);
                continue;
            }
            let name_start = at.checked_add(1).ok_or(BudgetError::Overflow)?;
            let opaque = rest.get(1).is_some_and(|byte| matches!(byte, b'!' | b'?'))
                || rest.get(1) == Some(&b'/') && rest.get(2).is_some_and(u8::is_ascii_alphabetic);
            if opaque {
                let Some(end) = html_tag_end(bytes, name_start) else {
                    break;
                };
                pos = end;
                continue;
            }
            if !bytes.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
                pos = name_start;
                continue;
            }
            let name_end = html_name_end(bytes, name_start);
            let name = bytes
                .get(name_start..name_end)
                .ok_or(BudgetError::Overflow)?;
            let Some(end) = html_tag_end(bytes, name_end) else {
                break;
            };
            if [b"script".as_slice(), b"style", b"textarea", b"title"]
                .iter()
                .any(|raw| name.eq_ignore_ascii_case(raw))
            {
                pos = html_raw_end(bytes, end, name).unwrap_or(bytes.len());
                continue;
            }
            let is_a = name.eq_ignore_ascii_case(b"a");
            if is_a || name.eq_ignore_ascii_case(b"img") {
                out.try_write(bytes.get(copied..at).ok_or(BudgetError::Overflow)?)?;
                self.tag(out, bytes.get(at..end).ok_or(BudgetError::Overflow)?, is_a)?;
                copied = end;
            }
            pos = end;
        }
        out.try_write(bytes.get(copied..).ok_or(BudgetError::Overflow)?)
    }
    fn page(&self, out: &mut BudgetedWriter, bytes: &[u8]) -> Result<()> {
        match self.syntax {
            HelpSyntax::Native => self.native_page(out, bytes),
            HelpSyntax::ExternalHtml => self.part(out, bytes),
        }
    }
    fn native_tag(&self, out: &mut BudgetedWriter, tag: &[u8], is_a: bool) -> Result<()> {
        let wanted = if is_a {
            b"href".as_slice()
        } else {
            b"src".as_slice()
        };
        for (at, byte) in tag.iter().enumerate() {
            if !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 11 | 12) {
                continue;
            }
            let name = at + 1;
            let equals = name + wanted.len();
            if !tag
                .get(name..equals)
                .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
                || tag.get(equals..equals + 2) != Some(b"=\"".as_slice())
            {
                continue;
            }
            let start = equals + 2;
            let Some(length) = tag.get(start..).and_then(|rest| find(rest, b"\"")) else {
                break;
            };
            let end = start + length;
            out.try_write(tag.get(..start).ok_or(BudgetError::Overflow)?)?;
            self.value(out, tag.get(start..end).ok_or(BudgetError::Overflow)?)?;
            return out.try_write(tag.get(end..).ok_or(BudgetError::Overflow)?);
        }
        out.try_write(tag)
    }
    fn native_part(&self, out: &mut BudgetedWriter, bytes: &[u8]) -> Result<()> {
        let mut pos = 0;
        let mut copied = 0;
        while let Some(relative) = find(bytes.get(pos..).ok_or(BudgetError::Overflow)?, b"<") {
            let at = pos + relative;
            let rest = bytes.get(at..).ok_or(BudgetError::Overflow)?;
            let (is_a, width) = if rest
                .get(..2)
                .is_some_and(|name| name.eq_ignore_ascii_case(b"<a"))
            {
                (true, 2)
            } else if rest
                .get(..4)
                .is_some_and(|name| name.eq_ignore_ascii_case(b"<img"))
            {
                (false, 4)
            } else {
                pos = at + 1;
                continue;
            };
            if rest
                .get(width)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                pos = at + 1;
                continue;
            }
            let Some(end) = find(rest, b">").map(|length| at + length + 1) else {
                break;
            };
            out.try_write(bytes.get(copied..at).ok_or(BudgetError::Overflow)?)?;
            self.native_tag(out, bytes.get(at..end).ok_or(BudgetError::Overflow)?, is_a)?;
            copied = end;
            pos = end;
        }
        out.try_write(bytes.get(copied..).ok_or(BudgetError::Overflow)?)
    }
    fn native_page(&self, out: &mut BudgetedWriter, bytes: &[u8]) -> Result<()> {
        let mut pos = 0;
        while let Some(relative) = find(bytes.get(pos..).ok_or(BudgetError::Overflow)?, b"<!--") {
            let start = pos + relative;
            let Some(end) = bytes
                .get(start + 4..)
                .and_then(|rest| find(rest, b"-->"))
                .map(|length| start + 4 + length + 3)
            else {
                break;
            };
            self.native_part(out, bytes.get(pos..start).ok_or(BudgetError::Overflow)?)?;
            out.try_write(bytes.get(start..end).ok_or(BudgetError::Overflow)?)?;
            pos = end;
        }
        self.native_part(out, bytes.get(pos..).ok_or(BudgetError::Overflow)?)
    }
}
pub fn managed_help_page<'a>(
    budget: &MemoryBudget,
    html: &[u8],
    files: &[(String, Vec<u8>)],
    resolve: impl Fn(Guid) -> Option<&'a str>,
    limit: u64,
) -> Result<Charged<Vec<u8>>> {
    project_help_page(budget, html, files, resolve, limit, HelpSyntax::Native)
}
pub fn managed_external_help_page<'a>(
    budget: &MemoryBudget,
    html: &[u8],
    files: &[(String, Vec<u8>)],
    resolve: impl Fn(Guid) -> Option<&'a str>,
    limit: u64,
) -> Result<Charged<Vec<u8>>> {
    project_help_page(
        budget,
        html,
        files,
        resolve,
        limit,
        HelpSyntax::ExternalHtml,
    )
}
fn project_help_page<'a>(
    budget: &MemoryBudget,
    html: &[u8],
    files: &[(String, Vec<u8>)],
    resolve: impl Fn(Guid) -> Option<&'a str>,
    limit: u64,
    syntax: HelpSyntax,
) -> Result<Charged<Vec<u8>>> {
    let mut out = BudgetedWriter::new(budget, limit)?;
    Linker {
        resolve: &resolve,
        files,
        syntax,
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
    project_help_xml(budget, help, limit, HelpSyntax::Native)
}
pub fn managed_external_help_xml(
    budget: &MemoryBudget,
    help: &Help,
    limit: u64,
) -> Result<Charged<String>> {
    project_help_xml(budget, help, limit, HelpSyntax::ExternalHtml)
}
fn project_help_xml(
    budget: &MemoryBudget,
    help: &Help,
    limit: u64,
    syntax: HelpSyntax,
) -> Result<Charged<String>> {
    let mut out = BudgetedText::new(budget, Category::Metadata, limit)?;
    for part in [
        crate::codec::parts::XML_HEAD,
        "\r\n<Help xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"2.21\">\r\n",
    ] {
        out.try_push_str(part)?;
    }
    for (language, _) in &help.pages {
        out.try_push_str("\t<Page>")?;
        match syntax {
            HelpSyntax::Native => out.try_push_str(language)?,
            HelpSyntax::ExternalHtml => {
                for character in language.chars() {
                    match character {
                        '&' => out.try_push_str("&amp;")?,
                        '<' => out.try_push_str("&lt;")?,
                        '>' => out.try_push_str("&gt;")?,
                        other => out.try_push(other)?,
                    }
                }
            }
        }
        out.try_push_str("</Page>\r\n")?;
    }
    out.try_push_str("</Help>")?;
    out.into_string()
}
