use super::ManagedValueContext;
use super::index::text;
use super::values::append;
use crate::LocalString;
use brace::Node;
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged};
const CATEGORY: Category = Category::Metadata;
impl ManagedValueContext<'_> {
    pub fn python_text(&self, node: &Node) -> Result<BudgetedText, BudgetError> {
        if let Some(value) = node.text() {
            return text(self.memory_budget(), &[value]);
        }
        let mut scratch = BudgetedVec::new(self.memory_budget(), CATEGORY)?;
        let mut len = 0usize;
        repr(node, &mut scratch, &mut |part| {
            len = len.checked_add(part.len()).ok_or(BudgetError::Overflow)?;
            Ok(())
        })?;
        let limit = u64::try_from(len).map_err(|_| BudgetError::Overflow)?;
        let mut output =
            BudgetedText::try_with_capacity(self.memory_budget(), CATEGORY, limit, len)?;
        repr(node, &mut scratch, &mut |part| output.try_push_str(part))?;
        Ok(output)
    }
    pub fn local_string(&self, node: &Node) -> Result<Option<Charged<LocalString>>, BudgetError> {
        let Some(list) = node.as_list() else {
            return Ok(None);
        };
        let Some(count) = list
            .first()
            .and_then(|n| n.text()?.trim().parse::<i64>().ok())
            .and_then(|n| usize::try_from(n).ok())
        else {
            return Ok(None);
        };
        if count > list.len().saturating_sub(1) / 2 {
            return Ok(None);
        }
        let mut values = BudgetedVec::try_with_capacity(self.memory_budget(), CATEGORY, count)?
            .into_charged_vec();
        for i in 0..count {
            let Some(language) = list.get(1 + 2 * i).and_then(Node::text) else {
                return Ok(None);
            };
            let Some(value) = list.get(2 + 2 * i).and_then(Node::text) else {
                return Ok(None);
            };
            let language = text(self.memory_budget(), &[language])?.into_string()?;
            let value = text(self.memory_budget(), &[value])?.into_string()?;
            values = append(values, language.try_zip(value)?)?;
        }
        Ok(Some(values.map(LocalString)))
    }
}
struct Frame<'a> {
    nodes: &'a [Node],
    next: usize,
}
fn repr<'a>(
    node: &'a Node,
    stack: &mut BudgetedVec<Frame<'a>>,
    emit: &mut impl FnMut(&str) -> Result<(), BudgetError>,
) -> Result<(), BudgetError> {
    stack.clear();
    let Some(nodes) = node.as_list() else {
        return Err(BudgetError::Overflow);
    };
    emit("[")?;
    stack.try_push(Frame { nodes, next: 0 })?;
    while let Some(frame) = stack.last_mut() {
        let Some(node) = frame.nodes.get(frame.next) else {
            stack.pop();
            emit("]")?;
            continue;
        };
        if frame.next != 0 {
            emit(", ")?;
        }
        frame.next += 1;
        match node {
            Node::List(nodes) => {
                emit("[")?;
                stack.try_push(Frame { nodes, next: 0 })?;
            }
            Node::Atom(value) => {
                emit("A")?;
                string_repr(value, emit)?;
            }
            Node::Str(value) => {
                emit("S")?;
                string_repr(value, emit)?;
            }
        }
    }
    Ok(())
}
fn character(
    value: char,
    emit: &mut impl FnMut(&str) -> Result<(), BudgetError>,
) -> Result<(), BudgetError> {
    let mut utf8 = [0; 4];
    emit(value.encode_utf8(&mut utf8))
}
fn escape(
    value: u32,
    width: usize,
    emit: &mut impl FnMut(&str) -> Result<(), BudgetError>,
) -> Result<(), BudgetError> {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut buffer = [0; 10];
    *buffer.get_mut(0).ok_or(BudgetError::Overflow)? = b'\\';
    *buffer.get_mut(1).ok_or(BudgetError::Overflow)? = match width {
        2 => b'x',
        4 => b'u',
        _ => b'U',
    };
    for i in 0..width {
        let shift = (width - i - 1) * 4;
        let nibble = usize::try_from((value >> shift) & 15).map_err(|_| BudgetError::Overflow)?;
        *buffer.get_mut(2 + i).ok_or(BudgetError::Overflow)? =
            *HEX.get(nibble).ok_or(BudgetError::Overflow)?;
    }
    let escaped = std::str::from_utf8(buffer.get(..2 + width).ok_or(BudgetError::Overflow)?)
        .map_err(|_| BudgetError::Overflow)?;
    emit(escaped)
}
fn string_repr(
    value: &str,
    emit: &mut impl FnMut(&str) -> Result<(), BudgetError>,
) -> Result<(), BudgetError> {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    character(quote, emit)?;
    for ch in value.chars() {
        let code = u32::from(ch);
        match ch {
            _ if ch == quote || ch == '\\' => {
                emit("\\")?;
                character(ch, emit)?;
            }
            '\t' => emit("\\t")?,
            '\n' => emit("\\n")?,
            '\r' => emit("\\r")?,
            _ if code < 0x20 || code == 0x7f => escape(code, 2, emit)?,
            _ if ch.is_ascii() || crate::objects::codecs::py_printable(ch) => character(ch, emit)?,
            _ if code <= 0xff => escape(code, 2, emit)?,
            _ if code <= 0xffff => escape(code, 4, emit)?,
            _ => escape(code, 8, emit)?,
        }
    }
    character(quote, emit)
}

impl ManagedValueContext<'_> {
    pub fn metadata_ref(&self, node: &Node) -> Result<Option<Charged<crate::MdRef>>, BudgetError> {
        let Some(raw) = node.text() else {
            return Ok(None);
        };
        let Some(id) = brace::Guid::parse(raw) else {
            return Ok(None);
        };
        if raw == "00000000-0000-0000-0000-000000000000" {
            return Ok(Some(super::values::empty(
                self.memory_budget(),
                crate::MdRef::default(),
            )?));
        }
        let Some(name) = self.index.names.get(&id) else {
            return Ok(None);
        };
        Ok(Some(
            text(self.memory_budget(), &[name.as_str()])?
                .into_string()?
                .map(|name| crate::MdRef { id: Some(id), name }),
        ))
    }
    pub fn field_ref(
        &self,
        kind: &str,
        name: &str,
        node: &Node,
    ) -> Result<Option<BudgetedText>, BudgetError> {
        let Some(list) = node.as_list() else {
            return Ok(None);
        };
        let Some(first) = list.first().and_then(Node::text) else {
            return Ok(None);
        };
        if first == "0" && list.len() > 1 {
            let Some(id) = list
                .get(1)
                .and_then(Node::text)
                .and_then(brace::Guid::parse)
            else {
                return Ok(None);
            };
            let Some(name) = self.index.names.get(&id) else {
                return Ok(None);
            };
            return Ok(Some(text(self.memory_budget(), &[name.as_str()])?));
        }
        let Some(attr) = std_attr(kind, first) else {
            return Ok(None);
        };
        Ok(Some(text(
            self.memory_budget(),
            &[kind, ".", name, ".StandardAttribute.", attr],
        )?))
    }
    pub fn field_path(
        &self,
        kind: &str,
        name: &str,
        segments: &[Node],
    ) -> Result<Option<BudgetedText>, BudgetError> {
        if self.other_document(kind, name, segments) {
            let len = segments
                .len()
                .checked_mul(39)
                .and_then(|n| n.checked_sub(1))
                .ok_or(BudgetError::Overflow)?;
            let mut out = BudgetedText::try_with_capacity(
                self.memory_budget(),
                CATEGORY,
                u64::try_from(len).map_err(|_| BudgetError::Overflow)?,
                len,
            )?;
            for (i, segment) in segments.iter().enumerate() {
                let Some(id) = segment.at(1).and_then(Node::as_guid) else {
                    return Ok(None);
                };
                if i != 0 {
                    out.try_push_str("/")?;
                }
                out.try_push_str("0:")?;
                out.try_push_str(super::index::GuidText::new(id).as_str()?)?;
            }
            return Ok(Some(out));
        }
        let mut prefix: Option<BudgetedText> = None;
        for segment in segments {
            let Some(list) = segment.as_list() else {
                return Ok(None);
            };
            let Some(first) = list.first().and_then(Node::text) else {
                return Ok(None);
            };
            let full = if first == "0" && list.len() > 1 {
                let Some(id) = list.get(1).and_then(Node::as_guid) else {
                    return Ok(None);
                };
                let Some(full) = self.index.names.get(&id) else {
                    return Ok(None);
                };
                text(self.memory_budget(), &[full.as_str()])?
            } else if let Some(prefix) = &prefix {
                let Some(kind) = prefix.as_str().rsplit('.').nth(1) else {
                    return Ok(None);
                };
                let Some(attr) = std_attr(kind, first) else {
                    return Ok(None);
                };
                text(
                    self.memory_budget(),
                    &[prefix.as_str(), ".StandardAttribute.", attr],
                )?
            } else {
                let Some(full) = self.field_ref(kind, name, segment)? else {
                    return Ok(None);
                };
                full
            };
            prefix = Some(full);
        }
        Ok(prefix)
    }
    fn other_document(&self, kind: &str, name: &str, segments: &[Node]) -> bool {
        let mut last = None;
        for segment in segments {
            let Some([first, id]) = segment.as_list() else {
                return false;
            };
            if first.text() != Some("0") {
                return false;
            }
            let Some(id) = id.as_guid() else { return false };
            last = self.index.names.get(&id);
        }
        let Some(last) = last else { return false };
        let mut parts = last.as_str().split('.');
        matches!((parts.next(), parts.next()), (Some("Document"), Some(other)) if !(kind == "Document" && name == other))
    }
}
fn std_attr(kind: &str, id: &str) -> Option<&'static str> {
    mdschema::static_schema()
        .table("stdattrs.json")?
        .get(kind)?
        .get(id)?
        .as_str()
}
