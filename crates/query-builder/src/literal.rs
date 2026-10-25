pub fn to_bsl(text: &str, line_head: &str) -> String {
    let indent: String = line_head
        .chars()
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    let mut out = String::with_capacity(text.len() + text.len() / 8 + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\n' => {
                out.push('\n');
                out.push_str(&indent);
                out.push('|');
            }
            '"' => out.push_str("\"\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn from_bsl(lit: &str) -> Option<String> {
    let inner = lit.strip_prefix('"')?.strip_suffix('"')?;
    if inner.replace("\"\"", "").contains('"') {
        return None;
    }
    let mut out = String::with_capacity(inner.len());
    for (i, line) in inner.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if i == 0 {
            out.push_str(line);
            continue;
        }
        let t = line.trim_start_matches([' ', '\t']);
        if t.starts_with("//") {
            continue;
        }
        out.push('\n');
        out.push_str(t.strip_prefix('|')?);
    }
    Some(out.replace("\"\"", "\""))
}
