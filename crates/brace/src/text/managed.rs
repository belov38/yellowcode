use super::{Doc, Error, ErrorKind, MAX_DEPTH};
use crate::Node;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedParseError {
    Parse(Error),
    Memory(BudgetError),
}
impl fmt::Display for ManagedParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(f),
            Self::Memory(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ManagedParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::Memory(error) => Some(error),
        }
    }
}
impl From<BudgetError> for ManagedParseError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl From<Error> for ManagedParseError {
    fn from(error: Error) -> Self {
        Self::Parse(error)
    }
}
type Result<T> = std::result::Result<T, ManagedParseError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseLimit {
    Nodes,
    Depth,
    InvalidDepthPolicy,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitedParseError {
    Parse(Error),
    Memory(BudgetError),
    Policy { offset: usize, kind: ParseLimit },
}
impl fmt::Display for LimitedParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => e.fmt(f),
            Self::Memory(e) => e.fmt(f),
            Self::Policy { offset, kind } => {
                write!(f, "text reader policy {kind:?} at byte {offset}")
            }
        }
    }
}
impl std::error::Error for LimitedParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(e) => Some(e),
            Self::Memory(e) => Some(e),
            Self::Policy { .. } => None,
        }
    }
}
impl From<ManagedParseError> for LimitedParseError {
    fn from(e: ManagedParseError) -> Self {
        match e {
            ManagedParseError::Parse(e) => Self::Parse(e),
            ManagedParseError::Memory(e) => Self::Memory(e),
        }
    }
}
impl From<BudgetError> for LimitedParseError {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
type LimitedResult<T> = std::result::Result<T, LimitedParseError>;
#[derive(Clone, Copy)]
struct Policy {
    nodes: usize,
    depth: usize,
}
fn error(offset: usize, kind: ErrorKind) -> ManagedParseError {
    Error { offset, kind }.into()
}
fn whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
}
fn delimiter(byte: u8) -> bool {
    matches!(byte, b',' | b'{' | b'}' | b'"')
}
fn string(bytes: BudgetedVec<u8>, offset: usize) -> Result<Charged<String>> {
    bytes
        .into_charged_vec()
        .try_map(String::from_utf8)
        .map_err(|_| error(offset, ErrorKind::InvalidUtf8))
}
fn atom(text: &str, budget: &MemoryBudget, category: Category) -> Result<Charged<Node>> {
    let mut bytes = BudgetedVec::try_with_capacity(budget, category, text.len())?;
    bytes.try_extend_from_slice(text.as_bytes())?;
    Ok(string(bytes, 0)?.map(Node::Atom))
}

struct Frame {
    items: BudgetedVec<Charged<Node>>,
    have_value: bool,
    open: usize,
}
impl Frame {
    fn finish(mut self, budget: &MemoryBudget, category: Category) -> Result<Charged<Node>> {
        let mut output =
            BudgetedVec::<Node>::try_with_capacity(budget, category, self.items.len())?
                .into_charged_vec();
        while let Some(child) = self.items.pop() {
            output = output.try_zip(child)?.map(|(mut output, child)| {
                output.push(child);
                output
            });
        }
        Ok(output.map(|mut items| {
            items.reverse();
            Node::List(items)
        }))
    }
}
struct Parser {
    stack: BudgetedVec<Frame>,
    tops: BudgetedVec<(usize, Charged<Node>)>,
    policy: Option<Policy>,
    nodes: usize,
}
impl Parser {
    fn admit_node(&mut self, offset: usize) -> LimitedResult<()> {
        if let Some(policy) = self.policy {
            self.nodes = self
                .nodes
                .checked_add(1)
                .filter(|n| *n <= policy.nodes)
                .ok_or(LimitedParseError::Policy {
                    offset,
                    kind: ParseLimit::Nodes,
                })?;
        }
        Ok(())
    }
    fn check_slot(&self, start: usize) -> Result<()> {
        if self.stack.last().is_some_and(|frame| frame.have_value) {
            return Err(error(start, ErrorKind::MissingComma));
        }
        Ok(())
    }
    fn place(&mut self, start: usize, node: Charged<Node>) -> Result<()> {
        self.check_slot(start)?;
        if let Some(frame) = self.stack.last_mut() {
            frame.items.try_push(node)?;
            frame.have_value = true;
        } else {
            self.tops.try_push((start, node))?;
        }
        Ok(())
    }
}

pub fn parse_bytes_budgeted_limited(
    bytes: &[u8],
    budget: &MemoryBudget,
    category: Category,
    max_nodes: usize,
    max_depth: usize,
) -> LimitedResult<Charged<Node>> {
    if max_depth > MAX_DEPTH {
        return Err(LimitedParseError::Policy {
            offset: 0,
            kind: ParseLimit::InvalidDepthPolicy,
        });
    }
    let (base, body) = bytes
        .strip_prefix(b"\xef\xbb\xbf")
        .map_or((0, bytes), |body| (3, body));
    let text = std::str::from_utf8(body).map_err(|e| {
        LimitedParseError::Parse(Error {
            offset: base + e.valid_up_to(),
            kind: ErrorKind::InvalidUtf8,
        })
    })?;
    run_with_policy(
        text,
        budget,
        category,
        Some(Policy {
            nodes: max_nodes,
            depth: max_depth,
        }),
    )
    .map_err(|e| match e {
        LimitedParseError::Parse(mut e) => {
            e.offset = e.offset.saturating_add(base);
            LimitedParseError::Parse(e)
        }
        LimitedParseError::Policy { offset, kind } => LimitedParseError::Policy {
            offset: offset.saturating_add(base),
            kind,
        },
        e => e,
    })
}

pub fn parse_bytes_budgeted(
    bytes: &[u8],
    budget: &MemoryBudget,
    category: Category,
) -> Result<Charged<Node>> {
    let (base, body) = bytes
        .strip_prefix(b"\xef\xbb\xbf")
        .map_or((0, bytes), |body| (3, body));
    let text = std::str::from_utf8(body)
        .map_err(|err| error(base + err.valid_up_to(), ErrorKind::InvalidUtf8))?;
    run(text, budget, category).map_err(|err| match err {
        ManagedParseError::Parse(mut err) => {
            err.offset = err.offset.saturating_add(base);
            err.into()
        }
        other => other,
    })
}

pub fn parse_doc_budgeted(
    bytes: &[u8],
    budget: &MemoryBudget,
    category: Category,
) -> Result<Charged<Doc>> {
    let (base, body, bom) = bytes
        .strip_prefix(b"\xef\xbb\xbf")
        .map_or((0, bytes, false), |body| (3, body, true));
    let text = std::str::from_utf8(body)
        .map_err(|err| error(base + err.valid_up_to(), ErrorKind::InvalidUtf8))?;
    let ws = [' ', '\t', '\r', '\n'];
    let no_lead = text.trim_start_matches(ws);
    let lead = text.get(..text.len() - no_lead.len()).unwrap_or_default();
    let mid = no_lead.trim_end_matches(ws);
    let trailer = no_lead.get(mid.len()..).unwrap_or_default();
    let shift = base + lead.len();
    if mid.is_empty() {
        return Err(error(shift, ErrorKind::Empty));
    }
    let root = run(mid, budget, category).map_err(|err| match err {
        ManagedParseError::Parse(mut err) => {
            err.offset = err.offset.saturating_add(shift);
            err.into()
        }
        other => other,
    })?;
    if !bom && lead.is_empty() && matches!(&*root, Node::Atom(atom) if atom.starts_with('\u{feff}'))
    {
        return Err(error(shift, ErrorKind::StrayBom));
    }
    let copy = |text: &str| -> Result<Charged<String>> {
        let mut bytes = BudgetedVec::try_with_capacity(budget, category, text.len())?;
        bytes.try_extend_from_slice(text.as_bytes())?;
        string(bytes, 0)
    };
    Ok(root
        .try_zip(copy(lead)?)?
        .try_zip(copy(trailer)?)?
        .map(|((root, lead), trailer)| Doc {
            root,
            bom,
            lead,
            trailer,
        }))
}
fn run(text: &str, budget: &MemoryBudget, category: Category) -> Result<Charged<Node>> {
    run_with_policy(text, budget, category, None).map_err(|e| match e {
        LimitedParseError::Parse(e) => ManagedParseError::Parse(e),
        LimitedParseError::Memory(e) => ManagedParseError::Memory(e),
        LimitedParseError::Policy { offset, .. } => error(offset, ErrorKind::TooDeep),
    })
}
fn run_with_policy(
    text: &str,
    budget: &MemoryBudget,
    category: Category,
    policy: Option<Policy>,
) -> LimitedResult<Charged<Node>> {
    let bytes = text.as_bytes();
    let mut parser = Parser {
        stack: BudgetedVec::new(budget, category)?,
        tops: BudgetedVec::new(budget, category)?,
        policy,
        nodes: 0,
    };
    let mut position = 0usize;
    while let Some(&byte) = bytes.get(position) {
        let start = position;
        match byte {
            b'{' => {
                parser.check_slot(start)?;
                if parser.policy.is_some_and(|p| parser.stack.len() >= p.depth) {
                    return Err(LimitedParseError::Policy {
                        offset: start,
                        kind: ParseLimit::Depth,
                    });
                }
                if parser.stack.len() >= MAX_DEPTH {
                    return Err(error(start, ErrorKind::TooDeep).into());
                }
                parser.admit_node(start)?;
                parser.stack.try_push(Frame {
                    items: BudgetedVec::new(budget, category)?,
                    have_value: false,
                    open: start,
                })?;
                position += 1;
            }
            b'}' => {
                let Some(mut frame) = parser.stack.pop() else {
                    return Err(error(start, ErrorKind::UnexpectedClose).into());
                };
                if !frame.have_value && !frame.items.is_empty() {
                    parser.admit_node(start)?;
                    frame.items.try_push(atom("", budget, category)?)?;
                }
                let open = frame.open;
                parser.place(open, frame.finish(budget, category)?)?;
                position += 1;
            }
            b',' => {
                if parser.stack.last().is_some_and(|frame| !frame.have_value) {
                    parser.admit_node(start)?;
                }
                let Some(frame) = parser.stack.last_mut() else {
                    return Err(error(start, ErrorKind::UnexpectedComma).into());
                };
                if !frame.have_value {
                    frame.items.try_push(atom("", budget, category)?)?;
                }
                frame.have_value = false;
                position += 1;
            }
            b'"' => {
                parser.admit_node(start)?;
                let (value, end) = quoted(text, start, budget, category)?;
                parser.place(start, value.map(Node::Str))?;
                position = end;
            }
            _ if whitespace(byte) => position += 1,
            _ => {
                parser.admit_node(start)?;
                let rest = bytes.get(start..).unwrap_or_default();
                let base64 = rest.starts_with(b"#base64:");
                let length = rest
                    .iter()
                    .position(|&byte| delimiter(byte) || (!base64 && whitespace(byte)))
                    .unwrap_or(rest.len());
                let end = start + length;
                parser.place(
                    start,
                    atom(text.get(start..end).unwrap_or_default(), budget, category)?,
                )?;
                position = end;
            }
        }
    }
    if let Some(frame) = parser.stack.last() {
        return Err(error(frame.open, ErrorKind::UnterminatedList).into());
    }
    if let Some((offset, _)) = parser.tops.get(1) {
        return Err(error(*offset, ErrorKind::TrailingData).into());
    }
    parser
        .tops
        .pop()
        .map(|(_, node)| node)
        .ok_or_else(|| error(0, ErrorKind::Empty).into())
}

fn quoted(
    text: &str,
    start: usize,
    budget: &MemoryBudget,
    category: Category,
) -> Result<(Charged<String>, usize)> {
    let bytes = text.as_bytes();
    let mut segment = start + 1;
    let mut position = segment;
    let mut length = 0usize;
    let end = loop {
        let rest = bytes.get(position..).unwrap_or_default();
        let quote = position
            + rest
                .iter()
                .position(|&byte| byte == b'"')
                .ok_or_else(|| error(start, ErrorKind::UnterminatedString))?;
        length = length
            .checked_add(quote - segment)
            .ok_or(BudgetError::Overflow)?;
        if bytes.get(quote + 1) == Some(&b'"') {
            length = length.checked_add(1).ok_or(BudgetError::Overflow)?;
            segment = quote + 2;
            position = segment;
        } else {
            break quote + 1;
        }
    };
    let mut output = BudgetedVec::try_with_capacity(budget, category, length)?;
    segment = start + 1;
    position = segment;
    loop {
        let rest = bytes.get(position..end).unwrap_or_default();
        let quote = position
            + rest
                .iter()
                .position(|&byte| byte == b'"')
                .ok_or_else(|| error(start, ErrorKind::UnterminatedString))?;
        if bytes.get(quote + 1) == Some(&b'"') {
            output.try_extend_from_slice(bytes.get(segment..=quote).unwrap_or_default())?;
            segment = quote + 2;
            position = segment;
        } else {
            output.try_extend_from_slice(bytes.get(segment..quote).unwrap_or_default())?;
            break;
        }
    }
    Ok((string(output, start)?, end))
}
