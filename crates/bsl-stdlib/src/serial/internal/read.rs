use super::text::{hex4, node_of_units};
use crate::serial::depth::MAX_VALUE_DEPTH;
use brace::Node;
use bsl_vm::{Exception, Execution};

pub(crate) enum ParseError {
    Format,
    Exc(Exception),
}

impl From<Exception> for ParseError {
    fn from(e: Exception) -> ParseError {
        ParseError::Exc(e)
    }
}

const BASE64: &[u8] = b"#base64:";
const UNITS_PER_CHARGE: usize = 4096;
const NODE_BYTES: u64 = std::mem::size_of::<Node>() as u64;

fn is_space(u: u16) -> bool {
    matches!(u, 0x20 | 0x09 | 0x0A | 0x0D)
}

enum State {
    First,
    Item,
    After,
}

const LISTS_PER_OBJECT: usize = 3;
const MAX_LIST_DEPTH: usize = LISTS_PER_OBJECT * MAX_VALUE_DEPTH + 16;

fn count(ex: &mut Execution, nodes: &mut u64) -> Result<(), Exception> {
    *nodes += 1;
    if nodes.is_multiple_of(1024) {
        ex.reserve(nodes.saturating_mul(NODE_BYTES))?;
    }
    Ok(())
}

pub(crate) fn parse(ex: &mut Execution, text: &[u16]) -> Result<Node, ParseError> {
    ex.reserve((text.len() as u64).saturating_mul(2))?;
    let mut pos = 0usize;
    let mut stack: Vec<Vec<Node>> = Vec::new();
    let mut state = State::First;
    let mut nodes = 0u64;
    let mut charged = 0usize;
    skip_space(text, &mut pos);
    if text.get(pos) != Some(&u16::from(b'{')) {
        return Err(ParseError::Format);
    }
    loop {
        if pos - charged >= UNITS_PER_CHARGE {
            ex.charge(1)?;
            charged = pos;
        }
        skip_space(text, &mut pos);
        let Some(&c) = text.get(pos) else {
            return Err(ParseError::Format);
        };
        match c {
            0x7B => {
                if !matches!(state, State::First | State::Item) && !stack.is_empty() {
                    return Err(ParseError::Format);
                }
                if stack.len() >= MAX_LIST_DEPTH {
                    return Err(ParseError::Format);
                }
                count(ex, &mut nodes)?;
                stack.push(Vec::new());
                pos += 1;
                state = State::First;
            }
            0x7D => {
                let Some(mut items) = stack.pop() else {
                    return Err(ParseError::Format);
                };
                if matches!(state, State::Item) {
                    count(ex, &mut nodes)?;
                    items.push(Node::empty());
                }
                pos += 1;
                let done = Node::List(items);
                match stack.last_mut() {
                    None => return Ok(done),
                    Some(parent) => parent.push(done),
                }
                state = State::After;
            }
            0x2C => {
                let Some(items) = stack.last_mut() else {
                    return Err(ParseError::Format);
                };
                if !matches!(state, State::After) {
                    count(ex, &mut nodes)?;
                    items.push(Node::empty());
                }
                pos += 1;
                state = State::Item;
            }
            _ => {
                if matches!(state, State::After) {
                    return Err(ParseError::Format);
                }
                let node = if c == 0x22 {
                    string(ex, text, &mut pos)?
                } else {
                    atom(ex, text, &mut pos)?
                };
                count(ex, &mut nodes)?;
                match stack.last_mut() {
                    Some(items) => items.push(node),
                    None => return Err(ParseError::Format),
                }
                state = State::After;
            }
        }
    }
}

fn skip_space(text: &[u16], pos: &mut usize) {
    while text.get(*pos).is_some_and(|&u| is_space(u)) {
        *pos += 1;
    }
}

fn string(ex: &mut Execution, text: &[u16], pos: &mut usize) -> Result<Node, ParseError> {
    let quote = u16::from(b'"');
    let mut i = *pos + 1;
    let start = i;
    let mut j = i;
    while let Some(&u) = text.get(j) {
        if u == quote {
            match text.get(j + 1) {
                Some(&q) if q == quote || q == u16::from(b'\\') => j += 2,
                _ => break,
            }
        } else {
            j += 1;
        }
    }
    if text.get(j) != Some(&quote) {
        return Err(ParseError::Format);
    }
    ex.reserve(((j - start) as u64).saturating_mul(3))?;
    let mut buf: Vec<u16> = Vec::with_capacity(j - start);
    while i < j {
        let u = text[i];
        if u != quote {
            buf.push(u);
            i += 1;
            continue;
        }
        if text.get(i + 1) == Some(&quote) {
            buf.push(quote);
            i += 2;
        } else {
            let h = text.get(i + 2..i + 6).and_then(hex4);
            let Some(h) = h else {
                return Err(ParseError::Format);
            };
            buf.push(h);
            i += 6;
        }
    }
    *pos = j + 1;
    Ok(node_of_units(&buf))
}

fn atom(ex: &mut Execution, text: &[u16], pos: &mut usize) -> Result<Node, ParseError> {
    let start = *pos;
    let base64 = text
        .get(start..start + BASE64.len())
        .is_some_and(|head| head.iter().zip(BASE64).all(|(&u, &b)| u == u16::from(b)));
    let mut end = start;
    while let Some(&u) = text.get(end) {
        if matches!(u, 0x2C | 0x7B | 0x7D) || (!base64 && is_space(u)) {
            break;
        }
        end += 1;
    }
    *pos = end;
    let len = end - start;
    ex.reserve(len as u64)?;
    let mut s = String::new();
    s.try_reserve_exact(len).map_err(|_| ex.out_of_memory())?;
    for &u in &text[start..end] {
        match char::from_u32(u32::from(u)) {
            Some(c) => s.push(c),
            None => return Err(ParseError::Format),
        }
    }
    Ok(Node::Atom(s))
}
