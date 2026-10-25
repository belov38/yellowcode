pub fn nstr(s: &[u16], lang: Option<&[u16]>, default_lang: &str) -> String {
    let text: Vec<char> = String::from_utf16_lossy(s).chars().collect();
    let want = match lang {
        Some(l) => String::from_utf16_lossy(l),
        None => default_lang.to_string(),
    }
    .trim()
    .to_lowercase();
    let mut i = 0;
    while i < text.len() {
        let code_start = i;
        while i < text.len() && text[i] != '=' && text[i] != ';' {
            i += 1;
        }
        if i >= text.len() || text[i] == ';' {
            i += 1;
            continue;
        }
        let code: String = text[code_start..i].iter().collect();
        i += 1;
        while i < text.len() && text[i].is_whitespace() {
            i += 1;
        }
        let Some(&quote) = text.get(i).filter(|c| matches!(c, '\'' | '"')) else {
            while i < text.len() && text[i] != ';' {
                i += 1;
            }
            i += 1;
            continue;
        };
        i += 1;
        let mut value = String::new();
        let mut closed = false;
        while i < text.len() {
            if text[i] == quote {
                if text.get(i + 1) == Some(&quote) {
                    value.push(quote);
                    i += 2;
                    continue;
                }
                closed = true;
                i += 1;
                break;
            }
            value.push(text[i]);
            i += 1;
        }
        if !closed {
            return String::new();
        }
        if code.trim().to_lowercase() == want {
            return value;
        }
        while i < text.len() && text[i] != ';' {
            i += 1;
        }
        i += 1;
    }
    String::new()
}
