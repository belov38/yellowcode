use serde_json::{Value, json};
use x_qbuilder::{Kind, find_literals};

const MAX_LITERALS: usize = 2000;
const MAX_LITERALS_BYTES: usize = 1 << 20;

pub fn literals(text: &str) -> Value {
    let mut total = 0usize;
    let mut truncated = false;
    let mut list: Vec<Value> = Vec::new();
    for l in find_literals(text) {
        total += l.text.len();
        if list.len() >= MAX_LITERALS || total > MAX_LITERALS_BYTES {
            truncated = true;
            break;
        }
        let kind = match l.kind {
            Kind::Query => "query",
            Kind::Composite => "composite",
            Kind::Other => "other",
            Kind::TooLarge => "too_large",
        };
        list.push(json!({ "line": l.line, "col": l.col, "text": l.text, "kind": kind }));
    }
    json!({ "literals": list, "truncated": truncated })
}
