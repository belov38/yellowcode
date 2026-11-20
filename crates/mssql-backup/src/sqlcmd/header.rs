const MAX_DIGITS: usize = 10;

pub(super) fn error_numbers(text: &str) -> Vec<u32> {
    let mut numbers = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let Some(rest) = text.get(at..) else {
            break;
        };
        let starts_word = text
            .get(..at)
            .and_then(|head| head.chars().next_back())
            .is_none_or(|c| !c.is_alphanumeric());
        if starts_word && let Some((number, used)) = english(rest).or_else(|| any_language(rest)) {
            numbers.push(number);
            at += more_pairs(rest, used);
            continue;
        }
        at += rest.chars().next().map_or(1, char::len_utf8);
    }
    numbers
}

pub(super) fn has_server_error(text: &str) -> bool {
    !error_numbers(text).is_empty()
}

pub(super) fn os_error(text: &str) -> Option<u32> {
    let mut quote: Option<char> = None;
    let mut start: Option<usize> = None;
    let mut previous = ' ';
    for (at, c) in text.char_indices() {
        if quote == Some(c) {
            quote = None;
            start = None;
        } else if quote.is_some() {
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            start = None;
        } else if c.is_ascii_digit() {
            if start.is_none() && previous == ' ' {
                start = Some(at);
            }
        } else {
            if c == '('
                && let Some(from) = start
                && let Some(number) = text.get(from..at).and_then(digits)
            {
                return Some(number);
            }
            start = None;
        }
        previous = c;
    }
    None
}

fn digits(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > MAX_DIGITS || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn english(text: &str) -> Option<(u32, usize)> {
    let rest = text.strip_prefix("Msg ")?;
    let count = rest.bytes().take_while(u8::is_ascii_digit).count();
    let number = digits(rest.get(..count)?)?;
    rest.get(count..)?
        .starts_with(',')
        .then_some((number, 4 + count))
}

fn more_pairs(text: &str, mut used: usize) -> usize {
    while let Some(rest) = text.get(used..).and_then(|tail| tail.strip_prefix(", "))
        && let Some((_, more)) = pair(rest)
    {
        used += 2 + more;
    }
    used
}

fn pair(text: &str) -> Option<(u32, usize)> {
    let word = text
        .char_indices()
        .find(|(_, c)| !c.is_alphabetic())
        .map_or(text.len(), |(at, _)| at);
    if word == 0 {
        return None;
    }
    let rest = text.get(word..)?.strip_prefix(' ')?;
    let count = rest.bytes().take_while(u8::is_ascii_digit).count();
    let number = digits(rest.get(..count)?)?;
    Some((number, word + 1 + count))
}

fn any_language(text: &str) -> Option<(u32, usize)> {
    let (number, mut used) = pair(text)?;
    for _ in 0..2 {
        let rest = text.get(used..)?.strip_prefix(", ")?;
        let (_, more) = pair(rest)?;
        used += 2 + more;
    }
    let ends = text
        .get(used..)
        .and_then(|tail| tail.chars().next())
        .is_none_or(|c| !c.is_alphanumeric());
    ends.then_some((number, used))
}
