use brace::Node;

pub(crate) fn encode_str(units: &[u16]) -> Node {
    if !units.iter().any(|u| (0xD800..=0xDFFF).contains(u)) {
        return Node::Str(String::from_utf16_lossy(units));
    }
    let mut out = String::with_capacity(units.len() + 8);
    out.push('"');
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if (0xD800..=0xDFFF).contains(&u) {
            out.push_str(&format!("\"\\{u:04x}"));
            i += 1;
            continue;
        }
        let end = units[i..]
            .iter()
            .position(|u| (0xD800..=0xDFFF).contains(u))
            .map_or(units.len(), |p| i + p);
        for c in String::from_utf16_lossy(&units[i..end]).chars() {
            if c == '"' {
                out.push('"');
            }
            out.push(c);
        }
        i = end;
    }
    out.push('"');
    Node::Atom(out)
}

pub(crate) fn node_of_units(units: &[u16]) -> Node {
    match String::from_utf16(units) {
        Ok(s) => Node::Str(s),
        Err(_) => encode_str(units),
    }
}

pub(crate) fn decode_str(n: &Node) -> Option<Vec<u16>> {
    match n {
        Node::Str(s) => Some(s.encode_utf16().collect()),
        Node::Atom(a) if a.starts_with('"') => {
            let units: Vec<u16> = a.encode_utf16().collect();
            let mut out = Vec::with_capacity(units.len());
            let mut i = 1;
            loop {
                let u = *units.get(i)?;
                i += 1;
                if u != u16::from(b'"') {
                    out.push(u);
                    continue;
                }
                match units.get(i) {
                    Some(&q) if q == u16::from(b'"') => {
                        out.push(q);
                        i += 1;
                    }
                    Some(&b) if b == u16::from(b'\\') => {
                        out.push(hex4(units.get(i + 1..i + 5)?)?);
                        i += 5;
                    }
                    None => return Some(out),
                    Some(_) => return None,
                }
            }
        }
        _ => None,
    }
}

pub(crate) fn hex4(d: &[u16]) -> Option<u16> {
    if d.len() != 4 {
        return None;
    }
    let mut v = 0u16;
    for &u in d {
        v = v * 16 + u16::try_from(char::from_u32(u32::from(u))?.to_digit(16)?).ok()?;
    }
    Some(v)
}
