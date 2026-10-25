#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug)]
pub struct SourceText {
    name: String,
    units: Vec<u16>,
    line_starts: Vec<u32>,
}

impl SourceText {
    pub fn from_utf8(name: impl Into<String>, bytes: &[u8]) -> SourceText {
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let mut units = Vec::with_capacity(bytes.len());
        if !decode_valid_utf8(bytes, &mut units) {
            units.clear();
            units.extend(String::from_utf8_lossy(bytes).encode_utf16());
        }
        Self::from_utf16(name, units)
    }

    pub fn from_utf16(name: impl Into<String>, units: Vec<u16>) -> SourceText {
        let mut line_starts = vec![0u32];
        let mut i = 0usize;
        while let Some(&c) = units.get(i) {
            i += 1;
            if c == 0x0D && units.get(i) == Some(&0x0A) {
                i += 1;
                line_starts.push(u32::try_from(i).unwrap_or(u32::MAX));
            } else if c == 0x0A || c == 0x0D {
                line_starts.push(u32::try_from(i).unwrap_or(u32::MAX));
            }
        }
        SourceText {
            name: name.into(),
            units,
            line_starts,
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(name: impl Into<String>, text: &str) -> SourceText {
        Self::from_utf16(name, text.encode_utf16().collect())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn units(&self) -> &[u16] {
        &self.units
    }

    pub fn line_count(&self) -> u32 {
        u32::try_from(self.line_starts.len()).unwrap_or(u32::MAX)
    }

    pub fn line_units(&self, line: u32) -> &[u16] {
        let Some(idx) = (line as usize).checked_sub(1) else {
            return &[];
        };
        let Some(&start) = self.line_starts.get(idx) else {
            return &[];
        };
        let end = match self.line_starts.get(idx + 1) {
            Some(&next) => next as usize,
            None => self.units.len(),
        };
        let line = self.units.get(start as usize..end).unwrap_or(&[]);
        match line {
            [rest @ .., 0x0D, 0x0A] | [rest @ .., 0x0A] | [rest @ .., 0x0D] => rest,
            _ => line,
        }
    }

    pub fn pos(&self, offset: u32) -> Pos {
        let offset = offset.min(u32::try_from(self.units.len()).unwrap_or(u32::MAX));
        let idx = self
            .line_starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1);
        let start = self.line_starts.get(idx).copied().unwrap_or(0);
        Pos {
            line: u32::try_from(idx + 1).unwrap_or(u32::MAX),
            col: (offset - start).saturating_add(1),
        }
    }

    pub fn text(&self, span: Span) -> String {
        let end = (span.end as usize).min(self.units.len());
        let start = (span.start as usize).min(end);
        String::from_utf16_lossy(self.units.get(start..end).unwrap_or(&[]))
    }
}

fn decode_valid_utf8(b: &[u8], out: &mut Vec<u16>) -> bool {
    let cont = |i: usize| {
        b.get(i)
            .copied()
            .filter(|&y| y & 0xC0 == 0x80)
            .map(u32::from)
    };
    let mut i = 0;
    while let Some(&x) = b.get(i) {
        if x < 0x80 {
            out.push(u16::from(x));
            i += 1;
            continue;
        }
        let x = u32::from(x);
        let (cp, len) = match x {
            0xC2..=0xDF => {
                let Some(y) = cont(i + 1) else { return false };
                (((x & 0x1F) << 6) | (y & 0x3F), 2)
            }
            0xE0..=0xEF => {
                let (Some(y), Some(z)) = (cont(i + 1), cont(i + 2)) else {
                    return false;
                };
                if (x == 0xE0 && y < 0xA0) || (x == 0xED && y >= 0xA0) {
                    return false;
                }
                (((x & 0x0F) << 12) | ((y & 0x3F) << 6) | (z & 0x3F), 3)
            }
            0xF0..=0xF4 => {
                let (Some(y), Some(z), Some(w)) = (cont(i + 1), cont(i + 2), cont(i + 3)) else {
                    return false;
                };
                if (x == 0xF0 && y < 0x90) || (x == 0xF4 && y >= 0x90) {
                    return false;
                }
                let cp = ((x & 0x07) << 18) | ((y & 0x3F) << 12) | ((z & 0x3F) << 6) | (w & 0x3F);
                (cp, 4)
            }
            _ => return false,
        };
        match u16::try_from(cp) {
            Ok(u) => out.push(u),
            Err(_) => {
                let c = cp - 0x1_0000;
                out.push(0xD800 | (c >> 10) as u16);
                out.push(0xDC00 | (c & 0x3FF) as u16);
            }
        }
        i += len;
    }
    true
}
