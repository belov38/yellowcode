use crate::binary::{BinNode, Reader};

#[allow(clippy::panic)]
pub fn exercise_text(bytes: &[u8]) {
    let _ = crate::is_json_entry(bytes);
    if let Ok(doc) = crate::parse_doc(bytes) {
        let out = crate::write_doc(&doc);
        match crate::parse_doc(&out) {
            Ok(again) if again == doc && crate::write_doc(&again) == out => {}
            other => panic!("text round trip broken: {other:?}"),
        }
        let _ = crate::to_compact(&doc.root);
    }
}

fn touch(n: &BinNode) {
    match n {
        BinNode::List(v) => v.iter().for_each(touch),
        BinNode::Atom(a) => {
            let _ = (
                a.as_int(),
                a.as_bool(),
                a.as_str(),
                a.as_utf16(),
                a.as_bytes(),
                a.as_guid(),
                a.as_numeric_text(),
                a.as_decimal(),
                a.as_f64(),
            );
        }
    }
}

#[allow(clippy::panic)]
pub fn exercise_binary(bytes: &[u8]) {
    if let Ok(items) = crate::binary::decode_all(bytes) {
        let out = crate::binary::encode_all(&items);
        if crate::binary::decode_all(&out).as_ref() != Ok(&items) {
            panic!("binary round trip broken");
        }
        items.iter().for_each(touch);
    }
    let cap = bytes.len().saturating_mul(8).saturating_add(16);
    let mut r = Reader::new(bytes);
    for i in 0.. {
        if i > cap {
            panic!("reader made no progress");
        }
        let step: Result<bool, _> = match i % 3 {
            0 => r.peek_event().map(|e| e.is_some()),
            1 => r.skip_value(),
            _ => r.next_event().map(|e| e.is_some()),
        };
        match step {
            Ok(true) => {}
            Ok(false) if i % 3 == 0 => {}
            Ok(false) | Err(_) => break,
        }
    }
}
