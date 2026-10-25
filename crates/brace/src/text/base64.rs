use super::write::Newline;
use crate::Node;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

const LINE: usize = 64;

pub fn base64_atom(data: &[u8], nl: Newline, final_break: bool) -> Node {
    let enc = STANDARD.encode(data);
    let mut out = String::from("#base64:");
    if enc.len() <= LINE {
        out.push_str(&enc);
        return Node::Atom(out);
    }
    let brk = nl.as_str();
    let mut rest = enc.as_str();
    while !rest.is_empty() {
        let (line, tail) = rest.split_at(LINE.min(rest.len()));
        out.push_str(line);
        if line.len() == LINE && (final_break || !tail.is_empty()) {
            out.push('\r');
            out.push_str(brk);
        }
        rest = tail;
    }
    Node::Atom(out)
}
