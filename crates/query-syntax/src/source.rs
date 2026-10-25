#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
    pub fn text(self, src: &str) -> &str {
        src.get(self.start as usize..self.end as usize)
            .unwrap_or("")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ErrAt {
    pub offset: u32,
    pub pos: Pos,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMap {
    line_starts: Vec<u32>,
    len: u32,
}

fn line_starts_of(text: &str) -> Vec<u32> {
    let bytes = text.as_bytes();
    let mut starts = vec![0u32];
    for (i, b) in bytes.iter().enumerate() {
        let next = (i + 1) as u32;
        match b {
            b'\n' => starts.push(next),
            b'\r' if bytes.get(i + 1) != Some(&b'\n') => starts.push(next),
            _ => {}
        }
    }
    starts
}

impl SourceMap {
    pub(crate) fn from_lines(line_starts: Vec<u32>, len: u32) -> Self {
        Self { line_starts, len }
    }
    pub fn new(text: &str) -> SourceMap {
        SourceMap {
            line_starts: line_starts_of(text),
            len: text.len() as u32,
        }
    }

    pub fn add_eof_line(&mut self) {
        if self.line_starts.last().is_none_or(|&s| s < self.len) {
            self.line_starts.push(self.len);
        }
    }

    pub fn pos(&self, text: &str, offset: u32) -> Pos {
        let offset = offset.min(self.len);
        let idx = self.line_starts.partition_point(|&s| s <= offset);
        let line = idx.max(1);
        let start = self.line_starts.get(line - 1).copied().unwrap_or(0) as usize;
        let mut end = offset as usize;
        let seg = loop {
            if let Some(s) = text.get(start..end) {
                break s;
            }
            if end <= start {
                break "";
            }
            end -= 1;
        };
        Pos {
            line: line as u32,
            col: 1 + seg.encode_utf16().count() as u32,
        }
    }

    pub fn at(&self, text: &str, offset: u32) -> ErrAt {
        ErrAt {
            offset,
            pos: self.pos(text, offset),
        }
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }
}

pub fn line_text(text: &str, line: u32) -> Option<(u32, &str)> {
    let starts = line_starts_of(text);
    let idx = (line as usize).checked_sub(1)?;
    let start = *starts.get(idx)? as usize;
    let end = starts.get(idx + 1).map_or(text.len(), |&n| n as usize);
    let raw = text.get(start..end)?;
    let body = raw
        .strip_suffix("\r\n")
        .or_else(|| raw.strip_suffix('\n'))
        .or_else(|| raw.strip_suffix('\r'))
        .unwrap_or(raw);
    Some((start as u32, body))
}
