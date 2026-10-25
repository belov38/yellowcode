use crate::allocation::{Allocation, Owned, Vector};
use memory_budget::BudgetError;

#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct Mark {
    byte_len: usize,
    lines: usize,
}
#[doc(hidden)]
pub struct XmlEmit<'a> {
    _policy: std::marker::PhantomData<&'a ()>,
    bytes: Vector<u8>,
    depth: usize,
    lines: usize,
}
impl<'a> XmlEmit<'a> {
    #[doc(hidden)]
    pub fn new(allocation: Allocation<'a>, depth: usize) -> Result<Self, BudgetError> {
        Ok(Self {
            _policy: std::marker::PhantomData,
            bytes: allocation.vector(0)?,
            depth,
            lines: 0,
        })
    }
    #[doc(hidden)]
    #[allow(
        clippy::len_without_is_empty,
        reason = "hidden codec helper (AR3 Task 4b widened it from pub(crate)); the length is a write position"
    )]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    #[doc(hidden)]
    pub fn reserve(&mut self, additional: usize) -> Result<(), BudgetError> {
        self.bytes.reserve(additional)
    }
    #[doc(hidden)]
    pub fn raw_fragments(&mut self, parts: &[&str]) -> Result<(), BudgetError> {
        let size = parts.iter().try_fold(0usize, |total, part| {
            total.checked_add(part.len()).ok_or(BudgetError::Overflow)
        })?;
        self.bytes.reserve(
            size.checked_add(self.depth)
                .and_then(|size| size.checked_add(2))
                .ok_or(BudgetError::Overflow)?,
        )?;
        self.start_line()?;
        for part in parts {
            self.text(part)?;
        }
        Ok(())
    }
    #[doc(hidden)]
    pub fn depth(&self) -> usize {
        self.depth
    }
    #[doc(hidden)]
    pub fn lines_since(&self, mark: Mark) -> Option<usize> {
        self.lines.checked_sub(mark.lines)
    }
    #[doc(hidden)]
    pub fn mark(&self) -> Mark {
        Mark {
            byte_len: self.bytes.len(),
            lines: self.lines,
        }
    }
    #[doc(hidden)]
    pub fn truncate(&mut self, mark: Mark) {
        self.bytes.truncate(mark.byte_len);
        self.lines = mark.lines;
    }
    fn start_line(&mut self) -> Result<(), BudgetError> {
        let extra = self
            .depth
            .checked_add(if self.lines == 0 { 0 } else { 2 })
            .ok_or(BudgetError::Overflow)?;
        self.bytes.reserve(extra)?;
        if self.lines > 0 {
            self.bytes.extend(b"\r\n")?;
        }
        for _ in 0..self.depth {
            self.bytes.push(b'\t')?;
        }
        self.lines = self.lines.checked_add(1).ok_or(BudgetError::Overflow)?;
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<(), BudgetError> {
        self.bytes.extend(text.as_bytes())
    }
    fn escaped(&mut self, text: &str, attr: bool) -> Result<(), BudgetError> {
        let len = text.bytes().try_fold(0usize, |len, b| {
            len.checked_add(match b {
                b'&' => 5,
                b'<' | b'>' => 4,
                b'"' if attr => 6,
                _ => 1,
            })
            .ok_or(BudgetError::Overflow)
        })?;
        self.bytes.reserve(len)?;
        let mut start = 0;
        for (at, b) in text.bytes().enumerate() {
            let replacement = match b {
                b'&' => Some("&amp;"),
                b'<' => Some("&lt;"),
                b'>' => Some("&gt;"),
                b'"' if attr => Some("&quot;"),
                _ => None,
            };
            if let Some(replacement) = replacement {
                self.bytes.extend(
                    text.as_bytes()
                        .get(start..at)
                        .ok_or(BudgetError::Overflow)?,
                )?;
                self.text(replacement)?;
                start = at.checked_add(1).ok_or(BudgetError::Overflow)?;
            }
        }
        self.bytes
            .extend(text.as_bytes().get(start..).ok_or(BudgetError::Overflow)?)
    }
    fn attrs<'b>(
        &mut self,
        attrs: impl IntoIterator<Item = (&'b str, &'b str)>,
    ) -> Result<(), BudgetError> {
        for (name, value) in attrs {
            self.text(" ")?;
            self.text(name)?;
            self.text("=\"")?;
            self.escaped(value, true)?;
            self.text("\"")?;
        }
        Ok(())
    }
    #[doc(hidden)]
    pub fn open<'b>(
        &mut self,
        tag: &str,
        attrs: impl IntoIterator<Item = (&'b str, &'b str)>,
    ) -> Result<(), BudgetError> {
        let depth = self.depth.checked_add(1).ok_or(BudgetError::Overflow)?;
        self.start_line()?;
        self.text("<")?;
        self.text(tag)?;
        self.attrs(attrs)?;
        self.text(">")?;
        self.depth = depth;
        Ok(())
    }
    #[doc(hidden)]
    pub fn close(&mut self, tag: &str) -> Result<(), BudgetError> {
        self.depth = self.depth.saturating_sub(1);
        self.start_line()?;
        self.text("</")?;
        self.text(tag)?;
        self.text(">")
    }
    #[doc(hidden)]
    pub fn leaf<'b>(
        &mut self,
        tag: &str,
        text: Option<&str>,
        attrs: impl IntoIterator<Item = (&'b str, &'b str)>,
    ) -> Result<(), BudgetError> {
        self.start_line()?;
        self.text("<")?;
        self.text(tag)?;
        self.attrs(attrs)?;
        match text {
            None | Some("") => self.text("/>"),
            Some(text) => {
                self.text(">")?;
                self.escaped(text, false)?;
                self.text("</")?;
                self.text(tag)?;
                self.text(">")
            }
        }
    }
    pub(crate) fn leaf_attribute_fragments<'b>(
        &mut self,
        tag: &str,
        attrs: impl Iterator<Item = (&'b str, [&'b str; 3])>,
    ) -> Result<(), BudgetError> {
        self.start_line()?;
        self.text("<")?;
        self.text(tag)?;
        for (name, value) in attrs {
            self.text(" ")?;
            self.text(name)?;
            self.text("=\"")?;
            for part in value {
                self.escaped(part, true)?;
            }
            self.text("\"")?;
        }
        self.text("/>")
    }
    pub(crate) fn leaf_text_fragments<'b>(
        &mut self,
        tag: &str,
        text: &[&str],
        attrs: impl IntoIterator<Item = (&'b str, &'b str)>,
    ) -> Result<(), BudgetError> {
        self.start_line()?;
        self.text("<")?;
        self.text(tag)?;
        self.attrs(attrs)?;
        if text.iter().all(|part| part.is_empty()) {
            return self.text("/>");
        }
        self.text(">")?;
        for part in text {
            self.escaped(part, false)?;
        }
        self.text("</")?;
        self.text(tag)?;
        self.text(">")
    }
    pub(crate) fn leaf_normalized<'b>(
        &mut self,
        tag: &str,
        text: Option<&str>,
        normalize: bool,
        attrs: impl IntoIterator<Item = (&'b str, &'b str)>,
    ) -> Result<(), BudgetError> {
        if !normalize {
            return self.leaf(tag, text, attrs);
        }
        self.start_line()?;
        self.text("<")?;
        self.text(tag)?;
        self.attrs(attrs)?;
        let Some(text) = text.filter(|text| !text.is_empty()) else {
            return self.text("/>");
        };
        self.text(">")?;
        let mut parts = text.split("\r\n");
        if let Some(first) = parts.next() {
            self.escaped(first, false)?;
        }
        for part in parts {
            self.text("\n")?;
            self.escaped(part, false)?;
        }
        self.text("</")?;
        self.text(tag)?;
        self.text(">")
    }
    pub(crate) fn open_fragments(&mut self, tag: &[&str]) -> Result<(), BudgetError> {
        let next = self.depth.checked_add(1).ok_or(BudgetError::Overflow)?;
        self.start_line()?;
        self.text("<")?;
        for part in tag {
            self.text(part)?;
        }
        self.text(">")?;
        self.depth = next;
        Ok(())
    }
    pub(crate) fn close_fragments(&mut self, tag: &[&str]) -> Result<(), BudgetError> {
        self.depth = self.depth.saturating_sub(1);
        self.start_line()?;
        self.text("</")?;
        for part in tag {
            self.text(part)?;
        }
        self.text(">")
    }
    pub(crate) fn leaf_fragments(
        &mut self,
        tag: &[&str],
        text: &str,
        normalize: bool,
    ) -> Result<(), BudgetError> {
        self.start_line()?;
        self.text("<")?;
        for part in tag {
            self.text(part)?;
        }
        if text.is_empty() {
            return self.text("/>");
        }
        self.text(">")?;
        if normalize {
            let mut parts = text.split("\r\n");
            if let Some(first) = parts.next() {
                self.escaped(first, false)?;
            }
            for part in parts {
                self.text("\n")?;
                self.escaped(part, false)?;
            }
        } else {
            self.escaped(text, false)?;
        }
        self.text("</")?;
        for part in tag {
            self.text(part)?;
        }
        self.text(">")
    }
    #[doc(hidden)]
    pub fn raw(&mut self, line: &str) -> Result<(), BudgetError> {
        self.start_line()?;
        self.text(line)
    }
    #[doc(hidden)]
    pub fn append_segment(&mut self, segment: &Self) -> Result<(), BudgetError> {
        if segment.lines == 0 {
            return Ok(());
        }
        if self.lines > 0 {
            self.bytes.extend(b"\r\n")?;
        }
        self.bytes.extend(&segment.bytes)?;
        self.lines = self
            .lines
            .checked_add(segment.lines)
            .ok_or(BudgetError::Overflow)?;
        Ok(())
    }
    #[doc(hidden)]
    pub fn finish(self) -> Owned<Vec<u8>> {
        self.bytes.finish()
    }
}
