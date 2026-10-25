use super::view::{Attr, Declaration, Node, NodeId, Source, Span, Text, XmlArena};
use crate::allocation::{Allocation, Owned, Vector};
use memory_budget::BudgetError;
use std::fmt::Write as _;

const XML: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
#[derive(Debug)]
#[doc(hidden)]
pub enum ReadError {
    Memory(BudgetError),
    Semantic { offset: usize, kind: ErrorKind },
}
#[derive(Debug)]
#[doc(hidden)]
pub enum ErrorKind {
    Static(&'static str),
    Syntax(&'static str),
    Utf8(std::str::Utf8Error),
    Mismatch(Span, Span),
    Unmatched(Span),
    UnknownPrefix(Span),
    Mixed(Span),
    Attr {
        kind: u8,
        pos: usize,
        previous: usize,
    },
    Namespace {
        kind: u8,
        span: Span,
    },
    UnknownEntity {
        span: Span,
        start: usize,
        end: usize,
    },
    UnterminatedEntity(usize, usize),
    CharSign,
    CharNumber(std::num::ParseIntError),
    CharInvalid(u32),
    CharZero,
}
impl From<BudgetError> for ReadError {
    fn from(value: BudgetError) -> Self {
        Self::Memory(value)
    }
}
fn error(offset: usize, kind: ErrorKind) -> ReadError {
    ReadError::Semantic { offset, kind }
}
fn fixed(offset: usize, text: &'static str) -> ReadError {
    error(offset, ErrorKind::Static(text))
}
fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}
fn blank(s: &str) -> bool {
    s.bytes().all(space)
}
fn span(start: usize, end: usize) -> Span {
    Span { start, end }
}
fn split(source: &str, name: Span) -> Result<(Option<Span>, Span), ReadError> {
    Ok(
        match name.get(source).ok_or(BudgetError::Overflow)?.find(':') {
            Some(p) => (
                Some(span(name.start, name.start + p)),
                span(name.start + p + 1, name.end),
            ),
            None => (None, name),
        },
    )
}
pub(crate) fn legacy_error(input: &[u8], failure: ReadError) -> super::XmlError {
    let input = input.strip_prefix(b"\xef\xbb\xbf").unwrap_or(input);
    let (offset, kind) = match failure {
        ReadError::Memory(_) => {
            return super::XmlError {
                offset: 0,
                what: String::new(),
            };
        }
        ReadError::Semantic { offset, kind } => (offset, kind),
    };
    let source = std::str::from_utf8(input).unwrap_or("");
    let mut output = LegacyText {
        bytes: match Allocation::Native.vector(0) {
            Ok(bytes) => bytes,
            Err(_) => {
                return super::XmlError {
                    offset: 0,
                    what: String::new(),
                };
            }
        },
        failed: None,
    };
    let formatted = match kind {
        ErrorKind::Static(s) => output.write_str(s),
        ErrorKind::Syntax(s) => write!(&mut output, "syntax error: {s}"),
        ErrorKind::Utf8(e) => write!(&mut output, "{e}"),
        ErrorKind::Mismatch(a, b) => write!(
            &mut output,
            "ill-formed document: expected `</{}>`, but `</{}>` was found",
            a.get(source).unwrap_or_default(),
            b.get(source).unwrap_or_default()
        ),
        ErrorKind::Unmatched(s) => write!(
            &mut output,
            "ill-formed document: close tag `</{}>` does not match any open tag",
            s.get(source).unwrap_or_default()
        ),
        ErrorKind::UnknownPrefix(s) => write!(
            &mut output,
            "undeclared prefix {:?}",
            s.get(source).unwrap_or_default()
        ),
        ErrorKind::Mixed(s) => write!(
            &mut output,
            "mixed content in <{}>",
            s.get(source).unwrap_or_default()
        ),
        ErrorKind::Attr {
            kind,
            pos,
            previous,
        } => match kind {
            0 => write!(
                &mut output,
                "position {pos}: attribute key must be directly followed by `=` or space"
            ),
            1 => write!(
                &mut output,
                "position {pos}: `=` must be followed by an attribute value"
            ),
            2 => write!(
                &mut output,
                "position {pos}: attribute value must be enclosed in `\"` or `'`"
            ),
            3 => write!(
                &mut output,
                "position {pos}: missing closing quote `{}` in attribute value",
                previous as u8 as char
            ),
            _ => write!(
                &mut output,
                "position {pos}: duplicated attribute, previous declaration at position {previous}"
            ),
        },
        ErrorKind::Namespace { kind, span } => match kind {
            0 => write!(
                &mut output,
                "the namespace prefix 'xml' cannot be bound to '{:?}'",
                span.get(source).unwrap_or_default()
            ),
            1 => write!(
                &mut output,
                "the namespace prefix 'xmlns' cannot be bound to '{:?}'",
                span.get(source).unwrap_or_default()
            ),
            2 => write!(
                &mut output,
                "the namespace prefix '{:?}' cannot be bound to '{XML}'",
                span.get(source).unwrap_or_default()
            ),
            _ => write!(
                &mut output,
                "the namespace prefix '{:?}' cannot be bound to '{XMLNS}'",
                span.get(source).unwrap_or_default()
            ),
        },
        ErrorKind::UnknownEntity { span, start, end } => write!(
            &mut output,
            "at {start}..{end}: unrecognized entity `{}`",
            span.get(source).unwrap_or_default()
        ),
        ErrorKind::UnterminatedEntity(start, end) => write!(
            &mut output,
            "Error while escaping character at range {start}..{end}: Cannot find ';' after '&'"
        ),
        ErrorKind::CharSign => {
            output.write_str("invalid character reference: unexpected number sign")
        }
        ErrorKind::CharNumber(e) => write!(&mut output, "invalid character reference: {e}"),
        ErrorKind::CharInvalid(n) => {
            write!(
                &mut output,
                "invalid character reference: `{n}` is not a valid codepoint"
            )
        }
        ErrorKind::CharZero => {
            output.write_str("invalid character reference: 0x0 character is not permitted in XML")
        }
    };
    if formatted.is_err() {
        return super::XmlError {
            offset: 0,
            what: String::new(),
        };
    }
    let what = match output.finish() {
        Ok(text) => text,
        Err(_) => {
            return super::XmlError {
                offset: 0,
                what: String::new(),
            };
        }
    };
    super::XmlError { offset, what }
}
struct LegacyText {
    bytes: Vector<u8>,
    failed: Option<BudgetError>,
}
impl std::fmt::Write for LegacyText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.failed.is_some() {
            return Err(std::fmt::Error);
        }
        self.bytes.extend(text.as_bytes()).map_err(|error| {
            self.failed = Some(error);
            std::fmt::Error
        })
    }
}
impl LegacyText {
    fn finish(self) -> Result<String, BudgetError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        self.bytes
            .finish()
            .map(String::from_utf8)
            .try_string()?
            .native()
    }
}
fn byte(input: &[u8], at: usize) -> Result<u8, ReadError> {
    input
        .get(at)
        .copied()
        .ok_or_else(|| BudgetError::Overflow.into())
}
fn slice<I: std::slice::SliceIndex<str, Output = str>>(
    source: &str,
    range: I,
) -> Result<&str, ReadError> {
    source
        .get(range)
        .ok_or_else(|| BudgetError::Overflow.into())
}
struct RawAttr {
    name: Span,
    value: Span,
}
fn next_attr(
    source: &str,
    cursor: &mut usize,
    end: usize,
    base: usize,
    off: usize,
) -> Result<Option<RawAttr>, ReadError> {
    let b = source.as_bytes();
    while *cursor < end && space(byte(b, *cursor)?) {
        *cursor += 1;
    }
    if *cursor == end {
        return Ok(None);
    }
    let start = *cursor;
    *cursor += 1;
    while *cursor < end && !space(byte(b, *cursor)?) && byte(b, *cursor)? != b'=' {
        *cursor += 1;
    }
    let name = span(start, *cursor);
    while *cursor < end && space(byte(b, *cursor)?) {
        *cursor += 1;
    }
    if *cursor == end || byte(b, *cursor)? != b'=' {
        return Err(error(
            off,
            ErrorKind::Attr {
                kind: 0,
                pos: *cursor - base,
                previous: 0,
            },
        ));
    }
    *cursor += 1;
    while *cursor < end && space(byte(b, *cursor)?) {
        *cursor += 1;
    }
    if *cursor == end {
        return Err(error(
            off,
            ErrorKind::Attr {
                kind: 1,
                pos: *cursor - base,
                previous: 0,
            },
        ));
    }
    let quote = byte(b, *cursor)?;
    if quote != b'\'' && quote != b'"' {
        return Err(error(
            off,
            ErrorKind::Attr {
                kind: 2,
                pos: *cursor - base,
                previous: 0,
            },
        ));
    }
    *cursor += 1;
    let value_start = *cursor;
    while *cursor < end && byte(b, *cursor)? != quote {
        *cursor += 1;
    }
    if *cursor == end {
        return Err(error(
            off,
            ErrorKind::Attr {
                kind: 3,
                pos: end - base,
                previous: usize::from(quote),
            },
        ));
    }
    let value = span(value_start, *cursor);
    *cursor += 1;
    Ok(Some(RawAttr { name, value }))
}
fn declaration(source: &str, name: Span) -> Result<Option<Option<Span>>, ReadError> {
    let name_str = name.get(source).ok_or(BudgetError::Overflow)?;
    Ok(if name_str == "xmlns" {
        Some(None)
    } else {
        name_str
            .strip_prefix("xmlns:")
            .map(|_| Some(span(name.start + 6, name.end)))
    })
}
impl XmlArena<'_> {
    fn store(&mut self, value: Owned<String>) -> Result<Text, ReadError> {
        let id = self.pool.len();
        self.pool.push(Some(value))?;
        Ok(Text::Pool(id))
    }
    fn discard(&mut self, value: Text) -> Result<(), ReadError> {
        if let Text::Pool(id) = value {
            drop(self.pool.get_mut(id).ok_or(BudgetError::Overflow)?.take());
        }
        Ok(())
    }
    fn decode(&mut self, raw: Span, a: Allocation<'_>, off: usize) -> Result<Text, ReadError> {
        let text = raw.get(self.source()?).ok_or(BudgetError::Overflow)?;
        if !text.contains('&') {
            return Ok(Text::Source(raw));
        }
        let mut out = a.vector(text.len())?;
        let mut at = 0;
        while let Some(found) = slice(text, at..)?.find('&') {
            let start = at + found;
            out.extend(slice(text, at..start)?.as_bytes())?;
            let Some(end) = slice(text, start + 1..)?.find(';').map(|p| start + 1 + p) else {
                return Err(error(off, ErrorKind::UnterminatedEntity(start, text.len())));
            };
            if slice(text, start + 1..end)?.contains('&') {
                return Err(error(off, ErrorKind::UnterminatedEntity(start, text.len())));
            }
            let name = slice(text, start + 1..end)?;
            let mut buf = [0u8; 4];
            let decoded = match name {
                "lt" => "<",
                "gt" => ">",
                "amp" => "&",
                "apos" => "'",
                "quot" => "\"",
                _ if name.starts_with('#') => {
                    let (digits, radix) = slice(name, 1..)?
                        .strip_prefix('x')
                        .map_or((slice(name, 1..)?, 10), |d| (d, 16));
                    if digits.starts_with(['+', '-']) {
                        return Err(error(off, ErrorKind::CharSign));
                    }
                    let n = u32::from_str_radix(digits, radix)
                        .map_err(|e| error(off, ErrorKind::CharNumber(e)))?;
                    if n == 0 {
                        return Err(error(off, ErrorKind::CharZero));
                    }
                    char::from_u32(n)
                        .ok_or_else(|| error(off, ErrorKind::CharInvalid(n)))?
                        .encode_utf8(&mut buf)
                }
                _ => {
                    return Err(error(
                        off,
                        ErrorKind::UnknownEntity {
                            span: span(raw.start + start + 1, raw.start + end),
                            start: start + 1,
                            end,
                        },
                    ));
                }
            };
            out.extend(decoded.as_bytes())?;
            at = end + 1;
        }
        out.extend(slice(text, at..)?.as_bytes())?;
        let owned = out.finish().map(String::from_utf8).try_string()?;
        self.store(owned)
    }
    fn resolve(
        &self,
        node: NodeId,
        prefix: Option<Span>,
        attribute: bool,
        off: usize,
    ) -> Result<Option<Text>, ReadError> {
        if attribute && prefix.is_none() {
            return Ok(None);
        }
        let p = prefix
            .map(|s| s.get(self.source()?).ok_or(BudgetError::Overflow))
            .transpose()?;
        if p == Some("") {
            return Err(error(
                off,
                ErrorKind::UnknownPrefix(prefix.ok_or(BudgetError::Overflow)?),
            ));
        }
        if p == Some("xml") {
            return Ok(Some(Text::Static(XML)));
        }
        if p == Some("xmlns") {
            return Ok(Some(Text::Static(XMLNS)));
        }
        let mut at = Some(node);
        while let Some(id) = at {
            let n = self.nodes.get(id.0).ok_or(BudgetError::Overflow)?;
            for d in self
                .decls
                .get(n.decls.clone())
                .ok_or(BudgetError::Overflow)?
                .iter()
                .rev()
            {
                if d.prefix
                    .map(|s| s.get(self.source()?).ok_or(BudgetError::Overflow))
                    .transpose()?
                    .filter(|s| !s.is_empty())
                    == p
                {
                    return if d.raw.start == d.raw.end {
                        if let Some(p) = prefix {
                            Err(error(off, ErrorKind::UnknownPrefix(p)))
                        } else {
                            Ok(None)
                        }
                    } else {
                        Ok(Some(Text::Source(d.raw)))
                    };
                }
            }
            at = n.parent;
        }
        match prefix {
            Some(p) => Err(error(off, ErrorKind::UnknownPrefix(p))),
            None => Ok(None),
        }
    }
    fn add_text(
        &mut self,
        stack: &[NodeId],
        text: Text,
        a: Allocation<'_>,
        off: usize,
    ) -> Result<(), ReadError> {
        let Some(id) = stack.last().copied() else {
            return if blank(self.text(text)?) {
                self.discard(text)?;
                Ok(())
            } else {
                Err(fixed(off, "text outside the root element"))
            };
        };
        let old = self.nodes.get(id.0).ok_or(BudgetError::Overflow)?.text;
        let next = if let Some(old) = old {
            let combined = a.text(&[self.text(old)?, self.text(text)?])?;
            let combined = self.store(combined)?;
            self.discard(old)?;
            self.discard(text)?;
            combined
        } else {
            text
        };
        self.nodes.get_mut(id.0).ok_or(BudgetError::Overflow)?.text = Some(next);
        Ok(())
    }
    fn close(&mut self, id: NodeId, off: usize) -> Result<(), ReadError> {
        if self
            .nodes
            .get(id.0)
            .ok_or(BudgetError::Overflow)?
            .first
            .is_some()
            && let Some(text) = self.nodes.get(id.0).ok_or(BudgetError::Overflow)?.text
        {
            if !blank(self.text(text)?) {
                return Err(error(
                    off,
                    ErrorKind::Mixed(self.nodes.get(id.0).ok_or(BudgetError::Overflow)?.local),
                ));
            }
            self.nodes.get_mut(id.0).ok_or(BudgetError::Overflow)?.text = None;
            self.discard(text)?;
        }
        Ok(())
    }
}
#[doc(hidden)]
pub fn parse_owned_arena(
    input: Owned<Vec<u8>>,
    a: Allocation<'_>,
) -> Result<XmlArena<'static>, ReadError> {
    let compatible = match (&input, a) {
        (Owned::Native(_), Allocation::Native) => true,
        (Owned::Managed(value), Allocation::Managed(budget)) => {
            value.reservation().category() == memory_budget::Category::Metadata
                && value.reservation().budget().same_scope(budget)
        }
        _ => false,
    };
    if !compatible {
        return Err(BudgetError::Overflow.into());
    }
    let logical_start = if input.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    let XmlArena {
        source: _,
        nodes,
        attrs,
        decls,
        pool,
        root,
        native,
    } = parse_arena(&input, a)?;
    let value = input.map(String::from_utf8).try_string()?;
    Ok(XmlArena {
        source: Source::Owned {
            value,
            logical_start,
        },
        nodes,
        attrs,
        decls,
        pool,
        root,
        native,
    })
}

#[doc(hidden)]
pub fn parse_arena<'src>(
    input: &'src [u8],
    a: Allocation<'_>,
) -> Result<XmlArena<'src>, ReadError> {
    let input = input.strip_prefix(b"\xef\xbb\xbf").unwrap_or(input);
    let source =
        std::str::from_utf8(input).map_err(|e| error(e.valid_up_to(), ErrorKind::Utf8(e)))?;
    let mut arena = XmlArena {
        source: Source::Borrowed(source),
        nodes: a.vector(0)?,
        attrs: a.vector(0)?,
        decls: a.vector(0)?,
        pool: a.vector(0)?,
        root: NodeId(0),
        native: matches!(a, Allocation::Native),
    };
    let mut stack = a.vector::<NodeId>(0)?;
    let mut root = None;
    let mut at = if input.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    let event_origin = at;
    let events: Result<(), ReadError> = (|| {
        while at < input.len() {
            if byte(input, at)? != b'<' {
                let start = at;
                at = input
                    .get(at..)
                    .ok_or(BudgetError::Overflow)?
                    .iter()
                    .position(|&b| b == b'<')
                    .map_or(input.len(), |p| at + p);
                let value = arena.decode(span(start, at), a, at)?;
                arena.add_text(&stack, value, a, at)?;
                continue;
            }
            let start = at;
            if input
                .get(at..)
                .ok_or(BudgetError::Overflow)?
                .starts_with(b"<!-")
            {
                let Some(p) = input
                    .get((at + 4).min(input.len())..)
                    .ok_or(BudgetError::Overflow)?
                    .windows(3)
                    .position(|bytes| bytes == b"-->")
                else {
                    return Err(error(
                        input.len(),
                        ErrorKind::Syntax(
                            "comment not closed: `-->` not found before end of input",
                        ),
                    ));
                };
                at += 4 + p + 3;
                if !input
                    .get(start..)
                    .ok_or(BudgetError::Overflow)?
                    .starts_with(b"<!--")
                {
                    return Err(error(
                        at,
                        ErrorKind::Syntax(
                            "comment not closed: `-->` not found before end of input",
                        ),
                    ));
                }
                continue;
            }
            if input
                .get(at..)
                .ok_or(BudgetError::Overflow)?
                .starts_with(b"<![")
            {
                let begin = (at + 9).min(input.len());
                let Some(p) = slice(source, (at + 3).min(input.len())..)?
                    .find("]]>")
                    .map(|p| at + 3 + p)
                else {
                    return Err(error(
                        input.len(),
                        ErrorKind::Syntax("CDATA not closed: `]]>` not found before end of input"),
                    ));
                };
                at = p + 3;
                if !input
                    .get(start..)
                    .ok_or(BudgetError::Overflow)?
                    .starts_with(b"<![CDATA[")
                {
                    return Err(error(
                        at,
                        ErrorKind::Syntax("CDATA not closed: `]]>` not found before end of input"),
                    ));
                }
                arena.add_text(&stack, Text::Source(span(begin, p)), a, at)?;
                continue;
            }
            if input
                .get(at..)
                .ok_or(BudgetError::Overflow)?
                .starts_with(b"<?")
            {
                let Some(p) = slice(source, at + 1..)?.find("?>") else {
                    return Err(error(
                        input.len(),
                        ErrorKind::Syntax(
                            "processing instruction or xml declaration not closed: `?>` not found before end of input",
                        ),
                    ));
                };
                at += 1 + p + 2;
                if at - start < 4 || byte(input, at - 2)? != b'?' {
                    return Err(error(
                        at,
                        ErrorKind::Syntax(
                            "processing instruction or xml declaration not closed: `?>` not found before end of input",
                        ),
                    ));
                }
                continue;
            }
            if input
                .get(at..)
                .ok_or(BudgetError::Overflow)?
                .starts_with(b"<!D")
                || input
                    .get(at..)
                    .ok_or(BudgetError::Overflow)?
                    .starts_with(b"<!d")
            {
                at += 2;
                let begin = (start + 9).min(input.len());
                let mut balance = 0usize;
                while at < input.len() {
                    match byte(input, at)? {
                        b'<' => balance = balance.checked_add(1).ok_or(BudgetError::Overflow)?,
                        b'>' if balance == 0 => break,
                        b'>' => balance -= 1,
                        _ => {}
                    }
                    at += 1;
                }
                if at == input.len() {
                    return Err(error(
                        at,
                        ErrorKind::Syntax("DOCTYPE not closed: `>` not found before end of input"),
                    ));
                }
                if !input
                    .get(start..)
                    .ok_or(BudgetError::Overflow)?
                    .get(..9)
                    .is_some_and(|b| b.eq_ignore_ascii_case(b"<!DOCTYPE"))
                {
                    return Err(error(
                        at + 1,
                        ErrorKind::Syntax("DOCTYPE not closed: `>` not found before end of input"),
                    ));
                }
                if blank(slice(source, begin..at)?) {
                    return Err(fixed(
                        at + 1,
                        "ill-formed document: `<!DOCTYPE>` declaration does not contain a name of a document type",
                    ));
                }
                at += 1;
                continue;
            }
            if input
                .get(at..)
                .ok_or(BudgetError::Overflow)?
                .starts_with(b"<!")
            {
                return Err(error(
                    (at + 1).min(input.len()),
                    ErrorKind::Syntax("unknown or missed symbol in markup"),
                ));
            }
            at += 1;
            let base = at;
            let closing = input.get(at) == Some(&b'/');
            if closing {
                at += 1;
            }
            let begin = at;
            let mut quote = None;
            while at < input.len() {
                let b = byte(input, at)?;
                if let Some(q) = quote {
                    if b == q {
                        quote = None;
                    }
                } else if b == b'\'' || b == b'"' {
                    quote = Some(b);
                } else if b == b'>' {
                    break;
                }
                at += 1;
            }
            if at == input.len() {
                return Err(error(
                    at,
                    ErrorKind::Syntax("tag not closed: `>` not found before end of input"),
                ));
            }
            let end = at;
            at += 1;
            if closing {
                let mut name_end = end;
                while name_end > begin && space(byte(input, name_end - 1)?) {
                    name_end -= 1;
                }
                if name_end == begin {
                    name_end = end;
                }
                let name = span(begin, name_end);
                let Some(id) = stack.pop() else {
                    return Err(error(at, ErrorKind::Unmatched(name)));
                };
                if arena
                    .nodes
                    .get(id.0)
                    .ok_or(BudgetError::Overflow)?
                    .name
                    .get(source)
                    .ok_or(BudgetError::Overflow)?
                    != name.get(source).ok_or(BudgetError::Overflow)?
                {
                    return Err(error(
                        at,
                        ErrorKind::Mismatch(
                            arena.nodes.get(id.0).ok_or(BudgetError::Overflow)?.name,
                            name,
                        ),
                    ));
                }
                arena.close(id, at)?;
                if stack.is_empty() {
                    root = Some(id);
                }
                continue;
            }
            let empty = end > begin && byte(input, end - 1)? == b'/';
            let attrs_end = end - usize::from(empty);
            let name_end = (begin..attrs_end)
                .find(|&i| input.get(i).is_some_and(|&b| space(b)))
                .unwrap_or(attrs_end);
            let name = span(begin, name_end);
            let (prefix, local) = split(source, name)?;
            if root.is_some() {
                return Err(fixed(at, "content after the root element"));
            }
            if stack.len() >= brace::MAX_DEPTH {
                return Err(fixed(at, "nesting too deep"));
            }
            let id = NodeId(arena.nodes.len());
            let parent = stack.last().copied();
            let decl_start = arena.decls.len();
            let mut cursor = name_end;
            loop {
                let raw = match next_attr(source, &mut cursor, attrs_end, base, at) {
                    Ok(Some(raw)) => raw,
                    Ok(None) | Err(ReadError::Semantic { .. }) => break,
                    Err(memory @ ReadError::Memory(_)) => return Err(memory),
                };
                if let Some(p) = declaration(source, raw.name)? {
                    let value = raw.value.get(source).ok_or(BudgetError::Overflow)?;
                    let pstr = p
                        .map(|p| p.get(source).ok_or(BudgetError::Overflow))
                        .transpose()?;
                    let invalid = match pstr {
                        Some("xml") if value != XML => Some((0, raw.value)),
                        Some("xmlns") => Some((1, raw.value)),
                        Some(p) if p != "xml" && value == XML => Some((2, prefix_span(raw.name))),
                        Some(p) if p != "xml" && value == XMLNS => Some((3, prefix_span(raw.name))),
                        _ => None,
                    };
                    if let Some((kind, span)) = invalid {
                        return Err(error(at, ErrorKind::Namespace { kind, span }));
                    }
                    arena.decls.push(Declaration {
                        prefix: p,
                        raw: raw.value,
                        value: Text::Source(raw.value),
                    })?;
                }
            }
            arena.nodes.push(Node {
                name,
                local,
                prefix,
                ns: None,
                parent,
                first: None,
                last: None,
                next: None,
                attrs: arena.attrs.len()..arena.attrs.len(),
                decls: decl_start..arena.decls.len(),
                text: None,
            })?;
            let ns = arena.resolve(id, prefix, false, at)?;
            arena.nodes.get_mut(id.0).ok_or(BudgetError::Overflow)?.ns = ns;
            let mut cursor = name_end;
            let mut decl_at = decl_start;
            loop {
                let mut key_start = cursor;
                while key_start < attrs_end && space(byte(input, key_start)?) {
                    key_start += 1;
                }
                if key_start == attrs_end {
                    break;
                }
                let mut key_end = key_start + 1;
                while key_end < attrs_end
                    && !space(byte(input, key_end)?)
                    && byte(input, key_end)? != b'='
                {
                    key_end += 1;
                }
                let mut eq = key_end;
                while eq < attrs_end && space(byte(input, eq)?) {
                    eq += 1;
                }
                if eq < attrs_end && byte(input, eq)? == b'=' {
                    let key = span(key_start, key_end);
                    let mut previous = name_end;
                    while let Some(old) = next_attr(source, &mut previous, key_start, base, at)? {
                        if old.name.get(source).ok_or(BudgetError::Overflow)?
                            == key.get(source).ok_or(BudgetError::Overflow)?
                        {
                            return Err(error(
                                at,
                                ErrorKind::Attr {
                                    kind: 4,
                                    pos: key_start - base,
                                    previous: old.name.start - base,
                                },
                            ));
                        }
                    }
                }
                let Some(raw) = next_attr(source, &mut cursor, attrs_end, base, at)? else {
                    break;
                };
                let value = arena.decode(raw.value, a, at)?;
                if declaration(source, raw.name)?.is_some() {
                    arena
                        .decls
                        .get_mut(decl_at)
                        .ok_or(BudgetError::Overflow)?
                        .value = value;
                    decl_at += 1;
                } else {
                    let (prefix, local) = split(source, raw.name)?;
                    let ns = arena.resolve(id, prefix, true, at)?;
                    arena.attrs.push(Attr {
                        local,
                        prefix,
                        ns,
                        value,
                    })?;
                }
            }
            arena
                .nodes
                .get_mut(id.0)
                .ok_or(BudgetError::Overflow)?
                .attrs
                .end = arena.attrs.len();
            if let Some(p) = parent {
                if let Some(last) = arena.nodes.get(p.0).ok_or(BudgetError::Overflow)?.last {
                    arena
                        .nodes
                        .get_mut(last.0)
                        .ok_or(BudgetError::Overflow)?
                        .next = Some(id);
                } else {
                    arena.nodes.get_mut(p.0).ok_or(BudgetError::Overflow)?.first = Some(id);
                }
                arena.nodes.get_mut(p.0).ok_or(BudgetError::Overflow)?.last = Some(id);
            }
            if empty {
                arena.close(id, at)?;
                if parent.is_none() {
                    root = Some(id);
                }
            } else {
                stack.push(id)?;
            }
        }
        Ok(())
    })();
    events.map_err(|failure| match failure {
        ReadError::Semantic { offset, kind } => match offset.checked_sub(event_origin) {
            Some(offset) => ReadError::Semantic { offset, kind },
            None => BudgetError::Overflow.into(),
        },
        memory => memory,
    })?;
    if !stack.is_empty() {
        return Err(fixed(input.len(), "unclosed element"));
    }
    arena.root = root.ok_or_else(|| fixed(input.len(), "no root element"))?;
    Ok(arena)
}
fn prefix_span(name: Span) -> Span {
    span(name.start + 6, name.end)
}
