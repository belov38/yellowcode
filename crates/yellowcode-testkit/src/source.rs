const ITEM_WORDS: [&str; 15] = [
    "fn",
    "struct",
    "enum",
    "union",
    "impl",
    "mod",
    "use",
    "const",
    "static",
    "type",
    "trait",
    "extern",
    "unsafe",
    "async",
    "macro_rules",
];

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn literal_end(text: &str, i: usize) -> Option<usize> {
    let b = text.as_bytes();
    let at = |k: usize| b.get(k).copied();
    match at(i)? {
        b'/' if at(i + 1) == Some(b'/') => Some(text[i..].find('\n').map_or(b.len(), |n| i + n)),
        b'/' if at(i + 1) == Some(b'*') => {
            let (mut k, mut depth) = (i + 2, 1usize);
            while k < b.len() && depth > 0 {
                if at(k) == Some(b'/') && at(k + 1) == Some(b'*') {
                    depth += 1;
                    k += 2;
                } else if at(k) == Some(b'*') && at(k + 1) == Some(b'/') {
                    depth -= 1;
                    k += 2;
                } else {
                    k += 1;
                }
            }
            Some(k)
        }
        b'"' => {
            let mut k = i + 1;
            while let Some(c) = at(k) {
                match c {
                    b'\\' => k += 2,
                    b'"' => return Some(k + 1),
                    _ => k += 1,
                }
            }
            Some(b.len())
        }
        b'r' if i == 0
            || !is_ident(b[i - 1])
            || (b[i - 1] == b'b' && (i < 2 || !is_ident(b[i - 2]))) =>
        {
            let mut k = i + 1;
            while at(k) == Some(b'#') {
                k += 1;
            }
            if at(k) != Some(b'"') {
                return None;
            }
            let close = format!("\"{}", "#".repeat(k - i - 1));
            Some(
                text[k + 1..]
                    .find(&close)
                    .map_or(b.len(), |n| k + 1 + n + close.len()),
            )
        }
        b'\'' => {
            if at(i + 1) == Some(b'\\') {
                let k = i + 3;
                return Some(text.get(k..)?.find('\'').map_or(b.len(), |n| k + n + 1));
            }
            let c = text.get(i + 1..)?.chars().next()?;
            let k = i + 1 + c.len_utf8();
            if at(k) == Some(b'\'') {
                Some(k + 1)
            } else {
                Some(i + 1)
            }
        }
        _ => None,
    }
}

fn closing(text: &str, open: usize) -> usize {
    let b = text.as_bytes();
    let (mut k, mut depth) = (open, 0usize);
    while k < b.len() {
        if let Some(end) = literal_end(text, k) {
            k = end;
            continue;
        }
        match b[k] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return k;
                }
            }
            _ => {}
        }
        k += 1;
    }
    b.len()
}

fn skip_trivia(text: &str, mut i: usize) -> usize {
    let b = text.as_bytes();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b.get(i) == Some(&b'/') && matches!(b.get(i + 1), Some(b'/' | b'*')) {
            i = literal_end(text, i).unwrap_or(b.len());
        } else if b.get(i) == Some(&b'#') && b.get(i + 1) == Some(&b'[') {
            i = closing(text, i + 1) + 1;
        } else {
            return i;
        }
    }
}

fn word(text: &str, i: usize) -> (&str, usize) {
    let b = text.as_bytes();
    let mut k = i;
    while k < b.len() && is_ident(b[k]) {
        k += 1;
    }
    (&text[i..k], k)
}

fn item_end(text: &str, start: usize) -> usize {
    let b = text.as_bytes();
    let mut i = skip_trivia(text, start);
    let (first, mut after) = word(text, i);
    let mut head = first;
    if first == "pub" {
        let k = skip_trivia(text, after);
        after = if b.get(k) == Some(&b'(') {
            closing(text, k) + 1
        } else {
            k
        };
        let k = skip_trivia(text, after);
        head = word(text, k).0;
    }
    let item = ITEM_WORDS.contains(&head) || head == "thread_local";
    let field = !item && {
        let k = skip_trivia(text, word(text, i).1);
        b.get(k) == Some(&b':') && b.get(k + 1) != Some(&b':')
    };
    let (mut depth, mut angle) = (0usize, 0usize);
    while i < b.len() {
        if let Some(end) = literal_end(text, i) {
            i = end;
            continue;
        }
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
                if depth == 0 && b[i] == b'}' && angle == 0 {
                    return i + 1;
                }
            }
            b'<' if field && depth == 0 => angle += 1,
            b'>' if field && depth == 0 && angle > 0 && b.get(i.wrapping_sub(1)) != Some(&b'-') => {
                angle -= 1
            }
            b';' if depth == 0 => return i + 1,
            b',' if depth == 0 && angle == 0 && !item => return i + 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

pub fn product_code(text: &str) -> String {
    let b = text.as_bytes();
    let (mut out, mut last, mut i) = (String::with_capacity(text.len()), 0usize, 0usize);
    while i < b.len() {
        if let Some(end) = literal_end(text, i) {
            i = end;
            continue;
        }
        if b[i] == b'#' {
            let inner = b.get(i + 1) == Some(&b'!');
            let open = if inner { i + 2 } else { i + 1 };
            if b.get(open) == Some(&b'[') {
                let close = closing(text, open);
                let content: String = text[open + 1..close.min(b.len())]
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                if content == "cfg(test)" {
                    out.push_str(&text[last..i]);
                    if inner {
                        return out;
                    }
                    let end = item_end(text, close + 1);
                    last = end;
                    i = end;
                    continue;
                }
                i = close + 1;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    out
}

pub fn code_only(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut last, mut i) = (0usize, 0usize);
    while i < b.len() {
        match literal_end(text, i) {
            Some(end) if end == i + 1 => i = end,
            Some(end) => {
                out.push_str(&text[last..i]);
                for c in text[i..end].chars() {
                    if c == '\n' {
                        out.push('\n');
                    } else {
                        out.extend(std::iter::repeat_n(' ', c.len_utf8()));
                    }
                }
                last = end;
                i = end;
            }
            None => i += 1,
        }
    }
    out.push_str(&text[last..]);
    out
}
