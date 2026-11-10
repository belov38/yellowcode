use std::collections::BTreeMap;

use crate::{
    events::Outcome,
    hasher::fold_char,
    literals::{LiteralSet, is_gap},
    utc,
    value::json_string,
};

pub(crate) const SCRUBBED: &str = "<scrubbed>";

const SHORT_SECRET: usize = 6;

pub(crate) const TEXT_KEYS: &[&str] = &["text", "name", "db", "message", "file"];

pub(crate) fn header_line(cli: &str, commit: &str, unix_seconds: i64) -> String {
    format!(
        "{{\"schema\":1,\"kind\":\"yellowcode-bak-report\",\"names\":\"hashed\",\"cli\":{},\"commit\":{},\"os\":{},\"utc\":{}}}",
        json_string(cli),
        json_string(commit),
        json_string(&format!(
            "{} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )),
        json_string(&utc::iso(unix_seconds))
    )
}

pub(crate) fn end_line(outcome: Outcome, exit_code: i32, leak_scrubbed: u64) -> String {
    let mut line = format!(
        "{{\"event\":\"end\",\"outcome\":{},\"exit_code\":{exit_code}",
        json_string(outcome.tag())
    );
    if leak_scrubbed > 0 {
        line.push_str(&format!(",\"leak_scrubbed\":{leak_scrubbed}"));
    }
    line.push('}');
    line
}

pub(crate) const DROPPED_LINE: &str = "{\"event\":\"line_dropped\",\"reason\":\"scrub\"}";

pub(crate) fn scrub_line(line: &str, literals: &LiteralSet, secret: &[u8]) -> (String, u64) {
    let (after_names, name_hits) = scrub_names(line, literals);
    let (after_secret, secret_hits) = scrub_secret(&after_names, secret);
    let hits = name_hits + secret_hits;
    if hits == 0 {
        return (line.to_owned(), 0);
    }
    if serde_json::from_str::<serde_json::Value>(&after_secret).is_err() {
        return (DROPPED_LINE.to_owned(), hits);
    }
    (after_secret, hits)
}

fn scrub_names(line: &str, literals: &LiteralSet) -> (String, u64) {
    if literals.is_empty() {
        return (line.to_owned(), 0);
    }
    let decoded = decode_values(line);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut first = 0_usize;
    while let Some(head) = decoded.get(first) {
        let value = head.value;
        let last = decoded
            .iter()
            .skip(first)
            .position(|d| d.value != value)
            .map_or(decoded.len(), |offset| first + offset);
        let group = decoded.get(first..last).unwrap_or_default();
        let folded: Vec<char> = group.iter().map(|d| fold_char(d.c)).collect();
        let mut at = 0_usize;
        while at < group.len() {
            match literals.longest_at(&folded, at) {
                Some(length) => {
                    if let (Some(from), Some(to)) = (group.get(at), group.get(at + length - 1)) {
                        ranges.push((from.from, to.to));
                    }
                    at += length;
                }
                None => at += 1,
            }
        }
        first = last;
    }
    let hits = ranges.len() as u64;
    if hits == 0 {
        return (line.to_owned(), 0);
    }
    let mut out = line.to_owned();
    replace_ranges(&mut out, ranges);
    (out, hits)
}

fn text_value_spans(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut key: Option<(usize, usize)> = None;
    let mut at = 0_usize;
    while let Some(&byte) = bytes.get(at) {
        if byte != b'"' {
            at += 1;
            continue;
        }
        let start = at + 1;
        let mut end = start;
        while let Some(&inside) = bytes.get(end) {
            match inside {
                b'"' => break,
                b'\\' => end += 2,
                _ => end += 1,
            }
        }
        let end = end.min(bytes.len());
        let mut after = end + 1;
        while bytes.get(after) == Some(&b' ') {
            after += 1;
        }
        if bytes.get(after) == Some(&b':') {
            key = Some((start, end));
        } else if let Some((from, to)) = key.take()
            && line.get(from..to).is_some_and(|k| TEXT_KEYS.contains(&k))
        {
            spans.push((start, end));
        }
        at = end + 1;
    }
    spans
}

const ACROSS_WINDOW: usize = 1024;

#[derive(Clone, Copy)]
struct Decoded {
    c: char,
    from: usize,
    to: usize,
    value: usize,
}

fn decode_values(line: &str) -> Vec<Decoded> {
    let mut out = Vec::new();
    for (value, (start, end)) in text_value_spans(line).into_iter().enumerate() {
        let Some(text) = line.get(start..end) else {
            continue;
        };
        let mut chars = text.char_indices().peekable();
        while let Some((offset, c)) = chars.next() {
            let from = start + offset;
            if c != '\\' {
                out.push(Decoded {
                    c,
                    from,
                    to: from + c.len_utf8(),
                    value,
                });
                continue;
            }
            let Some((_, kind)) = chars.next() else {
                break;
            };
            let decoded = match kind {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'u' => {
                    let hex: String = (0..4)
                        .filter_map(|_| chars.next().map(|(_, h)| h))
                        .collect();
                    u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .unwrap_or('\u{fffd}')
                }
                other => other,
            };
            let to = chars.peek().map_or(end, |(next, _)| start + next);
            out.push(Decoded {
                c: decoded,
                from,
                to,
                value,
            });
        }
    }
    out
}

fn replace_ranges(line: &mut String, mut ranges: Vec<(usize, usize)>) {
    ranges.sort_unstable_by_key(|range| std::cmp::Reverse(range.0));
    for (from, to) in ranges {
        if line.get(from..to).is_some() {
            line.replace_range(from..to, SCRUBBED);
        }
    }
}

pub(crate) fn scrub_across(lines: &mut [String], set: &LiteralSet) -> u64 {
    if !set.has_skeletons() {
        return 0;
    }
    let longest = set.longest_skeleton();
    let reach = ACROSS_WINDOW.max(longest);
    let mut hits = 0_u64;
    let mut history: Vec<(usize, Vec<Decoded>)> = Vec::new();
    for index in 0..=lines.len() {
        let final_turn = index == lines.len();
        let current = if final_turn {
            Vec::new()
        } else {
            lines
                .get(index)
                .map(|line| decode_values(line))
                .unwrap_or_default()
        };
        if current.is_empty() && !final_turn {
            continue;
        }
        let mut tails: Vec<(usize, Vec<Decoded>)> = Vec::new();
        let mut room = reach;
        for (line, decoded) in history.iter().rev() {
            if room == 0 {
                break;
            }
            let take = decoded.len().min(room);
            room -= take;
            let skip = decoded.len() - take;
            tails.push((*line, decoded.iter().skip(skip).copied().collect()));
        }
        let mut window: Vec<(Decoded, usize)> = Vec::new();
        for (line, decoded) in tails.into_iter().rev() {
            window.extend(decoded.into_iter().map(|d| (d, line)));
        }
        window.extend(current.iter().map(|d| (*d, index)));
        let mut skeleton: Vec<char> = Vec::new();
        let mut origin: Vec<usize> = Vec::new();
        for (position, (decoded, _)) in window.iter().enumerate() {
            if !is_gap(decoded.c) {
                skeleton.push(fold_char(decoded.c));
                origin.push(position);
            }
        }
        let mut ranges: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
        let mut at = 0_usize;
        while at < skeleton.len() {
            let waits = !final_turn
                && skeleton.len() - at < longest
                && origin
                    .get(at)
                    .is_some_and(|&start| window.len() - start <= reach)
                && skeleton
                    .get(at..)
                    .is_some_and(|rest| set.skeleton_in_progress(rest));
            if waits {
                break;
            }
            let Some((length, _)) = set.longest_skeleton_at(&skeleton, at) else {
                at += 1;
                continue;
            };
            let (Some(&first), Some(&last)) = (origin.get(at), origin.get(at + length - 1)) else {
                break;
            };
            let part = |position: usize| {
                window
                    .get(position)
                    .map(|(decoded, line)| (*line, decoded.value))
            };
            let torn = part(first) != part(last) || last + 1 - first != length;
            if torn {
                hits += 1;
                let mut position = first;
                while position <= last {
                    let Some(&(start, line)) = window.get(position) else {
                        break;
                    };
                    let mut end = position;
                    while window.get(end + 1).is_some_and(|&(next, next_line)| {
                        end < last && next_line == line && next.value == start.value
                    }) {
                        end += 1;
                    }
                    let to = window.get(end).map_or(start.to, |&(d, _)| d.to);
                    ranges.entry(line).or_default().push((start.from, to));
                    position = end + 1;
                }
            }
            at += length;
        }
        for (line, pieces) in ranges {
            if let Some(text) = lines.get_mut(line) {
                replace_ranges(text, pieces);
            }
            let fresh = lines
                .get(line)
                .map(|text| decode_values(text))
                .unwrap_or_default();
            if let Some(entry) = history.iter_mut().find(|(number, _)| *number == line) {
                entry.1 = fresh;
            }
        }
        if final_turn {
            break;
        }
        let fresh = lines
            .get(index)
            .map(|text| decode_values(text))
            .unwrap_or_default();
        history.push((index, fresh));
        let mut kept = 0_usize;
        let mut cut = 0_usize;
        for (position, (_, decoded)) in history.iter().enumerate().rev() {
            kept += decoded.len();
            if kept >= reach {
                cut = position;
                break;
            }
        }
        history.drain(..cut);
    }
    hits
}

fn scrub_secret(line: &str, secret: &[u8]) -> (String, u64) {
    if secret.is_empty() {
        return (line.to_owned(), 0);
    }
    let mut patterns: Vec<Vec<u8>> = vec![secret.to_vec()];
    let quoted = json_string(&String::from_utf8_lossy(secret));
    if let Some(escaped) = quoted.strip_prefix('"').and_then(|q| q.strip_suffix('"'))
        && escaped.as_bytes() != secret
    {
        patterns.push(escaped.as_bytes().to_vec());
    }
    patterns.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let standing_alone = secret.len() < SHORT_SECRET;
    let bytes = line.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let (mut at, mut hits) = (0_usize, 0_u64);
    while at < bytes.len() {
        let rest = bytes.get(at..).unwrap_or_default();
        let found = patterns.iter().find(|pattern| {
            rest.starts_with(pattern)
                && (!standing_alone || is_standing_alone(bytes, at, pattern.len()))
        });
        match found {
            Some(pattern) => {
                out.extend_from_slice(SCRUBBED.as_bytes());
                at += pattern.len();
                hits += 1;
            }
            None => {
                if let Some(&byte) = bytes.get(at) {
                    out.push(byte);
                }
                at += 1;
            }
        }
    }
    (String::from_utf8_lossy(&out).into_owned(), hits)
}

fn is_standing_alone(bytes: &[u8], at: usize, length: usize) -> bool {
    let word = |b: &u8| b.is_ascii_alphanumeric() || *b >= 0x80 || *b == b'_';
    let escape_letter = |i: usize| i.checked_sub(1).and_then(|p| bytes.get(p)) == Some(&b'\\');
    let before = at
        .checked_sub(1)
        .is_none_or(|i| bytes.get(i).is_none_or(|b| !word(b) || escape_letter(i)));
    let after = bytes.get(at + length).is_none_or(|b| !word(b));
    before && after
}
