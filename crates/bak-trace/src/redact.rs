use crate::{
    gaps::is_invisible,
    hasher::{Salt, fold_char, hash12},
    literals::{LiteralKind, LiteralSet, is_gap, is_word_char},
    tempdb::TempDbName,
    value::{Hashed, Redacted},
    vocabulary,
};
use std::{
    fmt,
    sync::{Arc, PoisonError, RwLock},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redaction {
    pub text: Redacted,
    pub omitted_lines: usize,
    pub unclassified: usize,
    pub truncated: bool,
}

struct Inner {
    salt: Salt,
    literals: RwLock<LiteralSet>,
    forms: RwLock<LiteralSet>,
    secrets: RwLock<Vec<Vec<char>>>,
}

#[derive(Clone)]
pub struct Redactor {
    inner: Arc<Inner>,
}

impl fmt::Debug for Redactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Redactor(..)")
    }
}

pub(crate) const TRUNCATION_MARK: &str = " [truncated]";

pub(crate) const MAX_INPUT_CHARS: usize = 65_536;
const BASE64_RUN: usize = 24;
const HEX_RUN: usize = 16;
const HEX_PREFIXED_DIGITS: usize = 8;
const ROW_BASE64_RUN: usize = 64;
const DROPPED: &str = "<?>";
const ID_RUN: usize = 8;
const ID_SCAN: usize = 256;
const PASSWORD_KEYS: [&str; 2] = ["password", "pwd"];

const CLEAR_NAMES: &[&str] = &[
    "master",
    "tempdb",
    "model",
    "msdb",
    "config",
    "configsave",
    "params",
    "files",
    "depotfiles",
    "configcas",
    "configcassave",
    "_yearoffset",
    "dbschema",
    "schemastorage",
    "ibversion",
    "v8users",
    "v8userpwdplcs",
    "v8cmsdpwds",
    "v8usersmatkeys",
    "binarydata",
    "binarydatastoragecontent",
    "binarydatastorageversion",
    "externalbindatastrgslist",
    "externalbindatastrgsblist",
];

impl Redactor {
    pub fn new(salt: Salt) -> Self {
        Redactor {
            inner: Arc::new(Inner {
                salt,
                literals: RwLock::new(LiteralSet::default()),
                forms: RwLock::new(LiteralSet::default()),
                secrets: RwLock::new(Vec::new()),
            }),
        }
    }

    pub fn add_literal(&self, kind: LiteralKind, value: &str) {
        if kind == LiteralKind::Secret {
            let folded: Vec<char> = value.trim().chars().map(fold_char).collect();
            let mut secrets = self
                .inner
                .secrets
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if !folded.is_empty() && !secrets.contains(&folded) {
                secrets.push(folded);
            }
        }
        self.inner
            .literals
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .add(kind, value);
        self.inner
            .forms
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .add_forms(kind, value);
    }

    pub(crate) fn hash(&self, text: &str) -> Hashed {
        Hashed::new(hash12(&self.inner.salt, text))
    }

    pub(crate) fn scan_set(&self) -> LiteralSet {
        let literals = self
            .inner
            .literals
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let forms = self
            .inner
            .forms
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        literals.merged(&forms)
    }

    pub fn redact(&self, text: &str) -> Redacted {
        self.redact_full(text, None).text
    }

    pub fn redact_source_path(&self, path: &str) -> Redacted {
        let normalized = path.replace('\\', "/");
        if is_source_path(&normalized) {
            Redacted::new(normalized)
        } else {
            self.redact(path)
        }
    }

    pub fn redact_full(&self, text: &str, limit: Option<usize>) -> Redaction {
        let literals = self
            .inner
            .literals
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let mut truncated = false;
        let capped: String = if text.chars().nth(MAX_INPUT_CHARS).is_some() {
            truncated = true;
            let kept: Vec<char> = text.chars().take(MAX_INPUT_CHARS).collect();
            let folded: Vec<char> = kept.iter().map(|c| fold_char(*c)).collect();
            let dropped = literals.partial_tail(&folded);
            kept.get(..kept.len().saturating_sub(dropped))
                .unwrap_or_default()
                .iter()
                .collect()
        } else {
            text.to_owned()
        };
        let capped = trim_at_cuts(&literals, &capped);
        let capped = self.needles_first(&literals, &capped);
        let mut stats = Stats::default();
        let mut out = String::with_capacity(capped.len());
        let mut first = true;
        let mut omitted_run = 0_usize;
        let mut separate = |out: &mut String| {
            if !first {
                out.push('\n');
            }
            first = false;
        };
        for raw in capped.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if is_row_like(line) {
                omitted_run += 1;
                stats.omitted_lines += 1;
                continue;
            }
            if omitted_run > 0 {
                separate(&mut out);
                out.push_str(&omitted_marker(omitted_run));
                omitted_run = 0;
            }
            separate(&mut out);
            out.push_str(&self.line(&literals, line, &mut stats));
        }
        if omitted_run > 0 {
            separate(&mut out);
            out.push_str(&omitted_marker(omitted_run));
        }
        if truncated {
            out.push_str(TRUNCATION_MARK);
        }
        if let Some(limit) = limit
            && out.chars().count() > limit
        {
            let keep = limit.saturating_sub(TRUNCATION_MARK.chars().count());
            out = out.chars().take(keep).collect::<String>() + TRUNCATION_MARK;
            truncated = true;
        }
        Redaction {
            text: Redacted::new(out),
            omitted_lines: stats.omitted_lines,
            unclassified: stats.unclassified,
            truncated,
        }
    }

    fn needles_first(&self, literals: &LiteralSet, text: &str) -> String {
        if literals.is_empty() {
            return text.to_owned();
        }
        let chars: Vec<char> = text.chars().collect();
        let folded: Vec<char> = chars.iter().map(|c| fold_char(*c)).collect();
        let with_skeletons = literals.has_skeletons();
        let mut skeleton: Vec<char> = Vec::new();
        let mut origin: Vec<usize> = Vec::new();
        let mut position: Vec<Option<usize>> = Vec::new();
        if with_skeletons {
            for (index, (c, f)) in chars.iter().zip(&folded).enumerate() {
                if is_gap(*c) {
                    position.push(None);
                } else {
                    position.push(Some(skeleton.len()));
                    skeleton.push(*f);
                    origin.push(index);
                }
            }
        }
        let word_at = |index: Option<usize>| {
            index
                .and_then(|i| chars.get(i))
                .is_some_and(|c| is_word_char(*c))
        };
        let mut out = String::with_capacity(text.len());
        let mut at = 0_usize;
        while let Some(&c) = chars.get(at) {
            if let Some(length) = placeholder_len(&chars, at).or_else(|| temp_db_len(&chars, at)) {
                out.extend(chars.get(at..at + length).unwrap_or_default());
                at += length;
                continue;
            }
            let exact = literals.longest_at(&folded, at);
            let torn = position
                .get(at)
                .copied()
                .flatten()
                .filter(|_| with_skeletons)
                .and_then(|start| {
                    let (length, entry) = literals.longest_skeleton_at(&skeleton, start)?;
                    let end = origin.get(start + length - 1)? + 1;
                    let alone =
                        !entry.whole_words || !(word_at(at.checked_sub(1)) || word_at(Some(end)));
                    alone.then_some((end - at, entry))
                });
            let found = match (exact, torn) {
                (Some(length), Some((span, _))) if span <= length => Some((length, None)),
                (_, Some((span, entry))) => Some((span, Some(entry))),
                (Some(length), None) => Some((length, None)),
                (None, None) => None,
            };
            if let Some((length, entry)) = found {
                let name: String = match entry {
                    Some(entry) => entry.original.iter().collect(),
                    None => chars
                        .get(at..at + length)
                        .unwrap_or_default()
                        .iter()
                        .collect(),
                };
                out.push_str(&format!("<h:{}>", self.hash(&name).as_str()));
                at += length;
            } else {
                out.push(c);
                at += 1;
            }
        }
        if !literals.has_fragments() {
            return out;
        }
        let chars: Vec<char> = out.chars().collect();
        let folded: Vec<char> = chars.iter().map(|c| fold_char(*c)).collect();
        let mut hidden = String::with_capacity(out.len());
        let mut at = 0_usize;
        while let Some(&c) = chars.get(at) {
            if let Some(length) = placeholder_len(&chars, at).or_else(|| temp_db_len(&chars, at)) {
                hidden.extend(chars.get(at..at + length).unwrap_or_default());
                at += length;
                continue;
            }
            let run = literals.fragment_run_at(&folded, at);
            if run > 0 {
                hidden.push_str(DROPPED);
                at += run;
            } else {
                hidden.push(c);
                at += 1;
            }
        }
        hidden
    }

    fn line(&self, literals: &LiteralSet, line: &str, stats: &mut Stats) -> String {
        let secrets = self
            .inner
            .secrets
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let prepared = self.with_server_fragment(&hide_password_values(&secrets, literals, line));
        let chars: Vec<char> = prepared.chars().filter(|c| !is_invisible(*c)).collect();
        let folded: Vec<char> = chars.iter().map(|c| fold_char(*c)).collect();
        let mut scan = Scan {
            redactor: self,
            literals,
            chars,
            folded,
            out: String::with_capacity(line.len()),
            at: 0,
            unclassified: 0,
        };
        scan.run();
        stats.unclassified += scan.unclassified;
        collapse_dropped(&scan.out)
    }

    fn with_server_fragment(&self, line: &str) -> String {
        let Some((start, end)) = server_fragment(line) else {
            return line.to_owned();
        };
        let name = line.get(start..end).unwrap_or_default().trim();
        if is_placeholder(&name.chars().collect::<Vec<char>>()) {
            return line.to_owned();
        }
        format!(
            "{}<host:{}>{}",
            line.get(..start).unwrap_or_default(),
            self.hash(name).as_str(),
            line.get(end..).unwrap_or_default()
        )
    }
}

fn password_value_end(chars: &[char], start: usize, command_line: bool) -> usize {
    let close = match chars.get(start) {
        Some('\'') => Some('\''),
        Some('"') => Some('"'),
        Some('{') if !command_line => Some('}'),
        _ => None,
    };
    let ends_value = |c: Option<&char>| match c {
        None => true,
        Some(c) if command_line => c.is_whitespace(),
        Some(c) => *c == ';',
    };
    let Some(close) = close else {
        return (start..chars.len())
            .find(|at| ends_value(chars.get(*at)))
            .unwrap_or(chars.len());
    };
    let mut at = start + 1;
    while let Some(&c) = chars.get(at) {
        if c == close {
            let mut after = at + 1;
            while !command_line && chars.get(after) == Some(&' ') {
                after += 1;
            }
            if ends_value(chars.get(after)) {
                return at + 1;
            }
        }
        at += 1;
    }
    chars.len()
}

fn hide_password_values(secrets: &[Vec<char>], literals: &LiteralSet, line: &str) -> String {
    let chars: Vec<char> = line.chars().filter(|c| !is_invisible(*c)).collect();
    let folded: Vec<char> = chars.iter().map(|c| fold_char(*c)).collect();
    let known = |value: usize| -> usize {
        secrets
            .iter()
            .filter(|secret| folded.get(value..value + secret.len()) == Some(secret.as_slice()))
            .map(Vec::len)
            .chain(literals.longest_at(&folded, value))
            .max()
            .unwrap_or(0)
    };
    let mut out = String::with_capacity(line.len());
    let mut at = 0_usize;
    while let Some(&c) = chars.get(at) {
        let key = PASSWORD_KEYS
            .iter()
            .find(|key| starts_with_str(folded.get(at..).unwrap_or_default(), key));
        let mut value = None;
        let mut command_line = false;
        if let Some(key) = key {
            let mut p = at + key.len();
            while chars.get(p).is_some_and(|c| matches!(c, ' ' | '\t')) {
                p += 1;
            }
            if chars.get(p) == Some(&'=') {
                p += 1;
                while chars.get(p).is_some_and(|c| matches!(c, ' ' | '\t')) {
                    p += 1;
                }
                value = Some(p);
            }
        } else if c == '-'
            && chars.get(at + 1) == Some(&'P')
            && chars.get(at + 2).is_some_and(|c| c.is_whitespace())
            && (at == 0 || chars.get(at - 1).is_some_and(|c| c.is_whitespace()))
        {
            let mut p = at + 2;
            while chars.get(p).is_some_and(|c| c.is_whitespace()) {
                p += 1;
            }
            value = Some(p);
            command_line = true;
        }
        if let Some(value) = value {
            let end = password_value_end(&chars, value, command_line).max(value + known(value));
            out.extend(chars.get(at..value).unwrap_or_default());
            if end > value {
                out.push_str(DROPPED);
            }
            at = end.max(value);
            continue;
        }
        out.push(c);
        at += 1;
    }
    out
}

fn is_source_path(path: &str) -> bool {
    let mut segments = path.split('/');
    let first_ok = segments
        .next()
        .is_some_and(|first| matches!(first, "crates" | "src" | "tools" | "desktop" | "tests"));
    first_ok
        && path.len() <= 200
        && path.ends_with(".rs")
        && path.split('/').all(|segment| {
            !matches!(segment, "" | "." | "..")
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        })
}

#[derive(Default)]
struct Stats {
    omitted_lines: usize,
    unclassified: usize,
}

fn omitted_marker(lines: usize) -> String {
    if lines == 1 {
        "[1 line omitted]".to_owned()
    } else {
        format!("[{lines} lines omitted]")
    }
}

fn temp_db_len(chars: &[char], at: usize) -> Option<usize> {
    const NAME: usize = 7 + 32;
    let name: String = chars.get(at..at + NAME)?.iter().collect();
    let word_before = at
        .checked_sub(1)
        .and_then(|i| chars.get(i))
        .is_some_and(|c| is_word_char(*c));
    let word_after = chars.get(at + NAME).is_some_and(|c| is_word_char(*c));
    (!word_before && !word_after && TempDbName::parse(&name).is_some()).then_some(NAME)
}

fn is_cut_marker(line: &str) -> bool {
    line.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|inside| inside.contains("bytes of sqlcmd diagnostics omitted"))
}

fn trim_at_cuts(literals: &LiteralSet, text: &str) -> String {
    if !text.contains("bytes of sqlcmd diagnostics omitted") {
        return text.to_owned();
    }
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    for marker in 0..lines.len() {
        if !lines.get(marker).is_some_and(|line| is_cut_marker(line)) {
            continue;
        }
        if let Some(before) = marker.checked_sub(1).and_then(|i| lines.get_mut(i)) {
            let chars: Vec<char> = before.chars().collect();
            let folded: Vec<char> = chars.iter().map(|c| fold_char(*c)).collect();
            let dropped = literals.partial_tail(&folded);
            *before = chars
                .get(..chars.len().saturating_sub(dropped))
                .unwrap_or_default()
                .iter()
                .collect();
        }
        if let Some(after) = lines.get_mut(marker + 1) {
            let chars: Vec<char> = after.chars().collect();
            let folded: Vec<char> = chars.iter().map(|c| fold_char(*c)).collect();
            let dropped = literals.partial_head(&folded);
            *after = chars.get(dropped..).unwrap_or_default().iter().collect();
        }
    }
    lines.join("\n")
}

const HEADER_WORDS: [(&str, &str, &str, &str); 2] = [
    ("Msg ", ", Level ", ", State ", ", Server "),
    ("Сообщение ", ", уровень ", ", состояние ", ", сервер "),
];

fn server_fragment(line: &str) -> Option<(usize, usize)> {
    HEADER_WORDS
        .iter()
        .find_map(|&(msg, level, state, marker)| {
            let at = line.find(marker)?;
            let head = line.get(..at)?;
            let header = head.get(head.rfind(msg)?..)?;
            if !(header.contains(level) && header.contains(state)) {
                return None;
            }
            let start = at + marker.len();
            let rest = line.get(start..)?;
            let length = rest.find(',').unwrap_or(rest.len());
            (!rest.get(..length)?.trim().is_empty()).then_some((start, start + length))
        })
}

fn longest_base64_run(text: &str) -> usize {
    let (mut longest, mut run) = (0_usize, 0_usize);
    for c in text.chars() {
        if is_base64(c) {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

fn is_base64(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')
}

fn is_row_like(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    if line.contains('|') || line.contains('\t') {
        return true;
    }
    if (line.starts_with('{') || line.starts_with('[')) && line.contains("\":") {
        return true;
    }
    if longest_base64_run(line) > ROW_BASE64_RUN {
        return true;
    }
    let (mut run, mut wide, mut gaps_of_two) = (0_usize, false, 0_usize);
    for c in line.chars().chain(std::iter::once('x')) {
        if c == ' ' {
            run += 1;
        } else {
            wide |= run >= 3;
            gaps_of_two += usize::from(run >= 2);
            run = 0;
        }
    }
    wide || gaps_of_two >= 2
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WordClass {
    Keep,
    Hash,
    Drop,
}

fn starts_with_str(chars: &[char], text: &str) -> bool {
    text.chars()
        .enumerate()
        .all(|(k, c)| chars.get(k) == Some(&c))
}

fn longest_hex_run(word: &str) -> usize {
    let (mut longest, mut run) = (0_usize, 0_usize);
    for b in word.bytes() {
        if b.is_ascii_hexdigit() {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

fn prefixed_hex_digits(text: &str) -> Option<&str> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_hexdigit())).then_some(digits)
}

fn is_guid(word: &str) -> bool {
    let bytes = word.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(k, b)| {
            if matches!(k, 8 | 13 | 18 | 23) {
                *b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn part_is_usual(part: &str) -> bool {
    if part.is_empty() {
        return false;
    }
    if part.bytes().all(|b| b.is_ascii_digit()) {
        return part.len() < ID_RUN;
    }
    if let Some(digits) = prefixed_hex_digits(part) {
        return digits.len() < HEX_PREFIXED_DIGITS;
    }
    if part.len() == 1 && part.bytes().all(|b| b.is_ascii_alphabetic()) {
        return true;
    }
    vocabulary::contains(part)
}

fn classify_word(word: &str) -> WordClass {
    if TempDbName::parse(word).is_some() {
        return WordClass::Keep;
    }
    if is_guid(word)
        || longest_hex_run(word) >= HEX_RUN
        || prefixed_hex_digits(word).is_some_and(|d| d.len() >= HEX_PREFIXED_DIGITS)
        || word
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|piece| piece.len() >= BASE64_RUN)
    {
        return WordClass::Hash;
    }
    if hyphen_joined_digits(word) >= ID_RUN {
        return WordClass::Hash;
    }
    if word.split(['-', '\'']).all(part_is_usual) {
        WordClass::Keep
    } else {
        WordClass::Drop
    }
}

fn hyphen_joined_digits(word: &str) -> usize {
    let (mut parts, mut digits) = (0_usize, 0_usize);
    for part in word.split(['-', '\'']) {
        if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
            parts += 1;
            digits += part.len();
        } else if let Some(hex) = prefixed_hex_digits(part) {
            parts += 1;
            digits += hex.len();
        }
    }
    if parts >= 2 { digits } else { 0 }
}

fn is_small_number(text: &str) -> bool {
    let digits =
        |s: &str| !s.is_empty() && s.len() < ID_RUN && s.bytes().all(|b| b.is_ascii_digit());
    let hex = |s: &str, most: usize| {
        !s.is_empty() && s.len() <= most && s.bytes().all(|b| b.is_ascii_hexdigit())
    };
    digits(text)
        || text
            .strip_prefix('x')
            .is_some_and(|d| hex(d, 4) || d.eq_ignore_ascii_case("FFFFFFFF"))
        || text
            .strip_prefix("0x")
            .or_else(|| text.strip_prefix("0X"))
            .is_some_and(|d| hex(d, HEX_PREFIXED_DIGITS - 1))
}

fn is_own_marker(text: &str) -> bool {
    let count = |s: &str| !s.is_empty() && s.len() <= 15 && s.bytes().all(|b| b.is_ascii_digit());
    if text == "redacted" {
        return true;
    }
    if let Some(n) = text.strip_suffix(" bytes of sqlcmd diagnostics omitted") {
        return count(n);
    }
    if let Some(n) = text
        .strip_suffix(" bytes of sqlcmd diagnostics omitted; the stream was not read to its end")
    {
        return n.strip_prefix("at least ").is_some_and(count);
    }
    text.strip_suffix(" line omitted")
        .or_else(|| text.strip_suffix(" lines omitted"))
        .is_some_and(count)
}

fn is_placeholder(chars: &[char]) -> bool {
    placeholder_len(chars, 0) == Some(chars.len())
}

fn placeholder_len(chars: &[char], at: usize) -> Option<usize> {
    let rest = chars.get(at..)?;
    for fixed in [DROPPED, "<scrubbed>"] {
        if starts_with_str(rest, fixed) {
            return Some(fixed.len());
        }
    }
    for prefix in ["<h:", "<path:", "<host:"] {
        if starts_with_str(rest, prefix) {
            let digits = rest.get(prefix.len()..prefix.len() + 12);
            let closed = rest.get(prefix.len() + 12) == Some(&'>');
            if closed
                && digits.is_some_and(|d| {
                    d.iter()
                        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(c))
                })
            {
                return Some(prefix.len() + 13);
            }
        }
    }
    None
}

const CLEAR_SNIPPETS: &[&str] = &[
    "Result::unwrap()",
    "Option::unwrap()",
    "Result::expect()",
    "Option::expect()",
    "Err",
    "Ok",
    "None",
    "Some",
    "left == right",
    "left != right",
];

fn clear_name(text: &str) -> bool {
    let folded = crate::hasher::fold(text);
    let name = folded.strip_prefix("dbo.").unwrap_or(&folded);
    CLEAR_NAMES.contains(&name)
}

fn keep_quoted(open: char, content: &[char]) -> bool {
    if content.is_empty() {
        return true;
    }
    let text: String = content.iter().collect();
    TempDbName::parse(&text).is_some()
        || is_placeholder(content)
        || (open == '[' && (is_small_number(&text) || is_own_marker(&text)))
        || CLEAR_SNIPPETS.contains(&text.as_str())
        || clear_name(&text)
}

fn collapse_dropped(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut at = 0_usize;
    while let Some(&c) = chars.get(at) {
        if starts_with_str(chars.get(at..).unwrap_or_default(), DROPPED) {
            out.push_str(DROPPED);
            at += DROPPED.len();
            loop {
                let mut next = at;
                while chars.get(next) == Some(&' ') {
                    next += 1;
                }
                if starts_with_str(chars.get(next..).unwrap_or_default(), DROPPED) {
                    at = next + DROPPED.len();
                } else {
                    break;
                }
            }
        } else {
            out.push(c);
            at += 1;
        }
    }
    out
}

fn ends_token(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '\'' | '"' | '`' | '<' | '>' | '|' | ';' | ',' | ')' | ']' | '}'
        )
}

struct Scan<'a> {
    redactor: &'a Redactor,
    literals: &'a LiteralSet,
    chars: Vec<char>,
    folded: Vec<char>,
    out: String,
    at: usize,
    unclassified: usize,
}

impl Scan<'_> {
    fn run(&mut self) {
        while self.at < self.chars.len() {
            let before = self.at;
            let done = self.try_placeholder()
                || self.try_literal()
                || self.try_quote()
                || self.try_token()
                || self.try_fragment()
                || self.try_numeric()
                || self.try_word();
            if !done {
                if let Some(c) = self.peek(0) {
                    self.out.push(c);
                }
                self.at += 1;
            }
            debug_assert!(self.at > before);
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    fn prev(&self) -> Option<char> {
        self.at
            .checked_sub(1)
            .and_then(|i| self.chars.get(i).copied())
    }

    fn text(&self, from: usize, to: usize) -> String {
        self.chars
            .get(from..to)
            .unwrap_or_default()
            .iter()
            .collect()
    }

    fn push_hashed(&mut self, tag: &str, from: usize, to: usize) {
        let hash = self.redactor.hash(&self.text(from, to));
        self.out.push('<');
        self.out.push_str(tag);
        self.out.push(':');
        self.out.push_str(hash.as_str());
        self.out.push('>');
    }

    fn push_original(&mut self, from: usize, to: usize) {
        let original = self.text(from, to);
        self.out.push_str(&original);
    }

    fn try_placeholder(&mut self) -> bool {
        match placeholder_len(&self.chars, self.at) {
            Some(length) => {
                self.push_original(self.at, self.at + length);
                self.at += length;
                true
            }
            None => false,
        }
    }

    fn try_literal(&mut self) -> bool {
        match self.literals.longest_at(&self.folded, self.at) {
            Some(length) => {
                self.push_hashed("h", self.at, self.at + length);
                self.at += length;
                true
            }
            None => false,
        }
    }

    fn find_close(&self, open: char, from: usize) -> (usize, usize) {
        let close = if open == '[' { ']' } else { open };
        let mut at = from;
        while let Some(&c) = self.chars.get(at) {
            if c == close {
                if open == '[' {
                    if self.chars.get(at + 1) == Some(&']') {
                        at += 2;
                        continue;
                    }
                    return (at, at + 1);
                }
                if !self
                    .chars
                    .get(at + 1)
                    .is_some_and(|next| next.is_alphanumeric())
                {
                    return (at, at + 1);
                }
            }
            at += 1;
        }
        (self.chars.len(), self.chars.len())
    }

    fn try_quote(&mut self) -> bool {
        let Some(open) = self.peek(0) else {
            return false;
        };
        if !matches!(open, '\'' | '"' | '`' | '[') {
            return false;
        }
        if open != '[' && self.prev().is_some_and(char::is_alphanumeric) {
            return false;
        }
        let start = self.at + 1;
        if start >= self.chars.len() {
            return false;
        }
        let (end, next) = self.find_close(open, start);
        let content = self.chars.get(start..end).unwrap_or_default();
        if keep_quoted(open, content) {
            self.push_original(self.at, next);
        } else {
            let hash = self.redactor.hash(&self.text(start, end));
            self.out.push('\'');
            self.out.push_str("<h:");
            self.out.push_str(hash.as_str());
            self.out.push_str(">'");
        }
        self.at = next;
        true
    }

    fn at_token_start(&self) -> bool {
        match self.prev() {
            None => true,
            Some(p) => {
                !(p.is_alphanumeric()
                    || matches!(
                        p,
                        '_' | '.' | '-' | '/' | '\\' | '~' | '$' | '@' | '%' | '+'
                    ))
            }
        }
    }

    fn token_end(&self) -> usize {
        let mut end = self.at;
        while self.chars.get(end).is_some_and(|c| !ends_token(*c)) {
            end += 1;
        }
        while end > self.at + 1
            && self
                .chars
                .get(end - 1)
                .is_some_and(|c| matches!(c, '.' | ':' | '!' | '?'))
        {
            end -= 1;
        }
        end
    }

    fn explicit_path_start(&self) -> bool {
        let (c0, c1, c2) = (self.peek(0), self.peek(1), self.peek(2));
        let separator = |c: Option<char>| matches!(c, Some('/' | '\\'));
        c0.is_some_and(|c| c.is_ascii_alphabetic()) && c1 == Some(':') && separator(c2)
            || c0 == Some('\\') && c1 == Some('\\') && c2.is_some_and(|c| !c.is_whitespace())
            || c0 == Some('/')
                && c1.is_some_and(|c| {
                    c.is_alphanumeric() || matches!(c, '.' | '_' | '~' | '-' | '$')
                })
            || c0 == Some('~') && separator(c1)
            || c0 == Some('.') && separator(c1)
            || c0 == Some('.') && c1 == Some('.') && separator(c2)
    }

    fn url_end(&self) -> Option<usize> {
        let mut at = self.at;
        if !self.chars.get(at).is_some_and(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        while self
            .chars
            .get(at)
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        {
            at += 1;
        }
        let rest = self.chars.get(at..)?;
        (starts_with_str(rest, "://") && rest.get(3).is_some_and(|c| !c.is_whitespace()))
            .then(|| self.token_end())
    }

    fn relative_path_end(&self) -> Option<usize> {
        let end = self.token_end();
        let token = self.chars.get(self.at..end)?;
        let between = |k: usize, c: char| {
            token
                .get(k.wrapping_sub(1))
                .is_some_and(|p| p.is_alphanumeric())
                && token.get(k + 1).is_some_and(|n| n.is_alphanumeric())
                && token.get(k) == Some(&c)
        };
        let slashes = (1..token.len()).filter(|k| between(*k, '/')).count();
        let backslashes = (1..token.len()).filter(|k| between(*k, '\\')).count();
        (slashes >= 2 || backslashes >= 1).then_some(end)
    }

    fn base64_run(&self) -> usize {
        self.chars
            .get(self.at..)
            .unwrap_or_default()
            .iter()
            .take_while(|c| is_base64(**c))
            .count()
    }

    fn try_base64(&mut self) -> bool {
        let length = self.base64_run();
        if length < BASE64_RUN {
            return false;
        }
        self.push_hashed("h", self.at, self.at + length);
        self.at += length;
        true
    }

    fn host_end(&self) -> Option<usize> {
        let mut end = self.at;
        while self.chars.get(end).is_some_and(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '%' | '+' | '-')
        }) {
            end += 1;
        }
        while end > self.at
            && self
                .chars
                .get(end - 1)
                .is_some_and(|c| matches!(c, '.' | '-' | '_' | '@' | '%' | '+'))
        {
            end -= 1;
        }
        let token = self.chars.get(self.at..end)?;
        let text: String = token.iter().collect();
        let dotted = text.split('.').filter(|label| !label.is_empty()).count() >= 2
            && text.contains('.')
            && !text.starts_with('.')
            && !text.contains("..");
        let email = {
            let mut sides = text.splitn(2, '@');
            let (local, domain) = (
                sides.next().unwrap_or_default(),
                sides.next().unwrap_or_default(),
            );
            !local.is_empty() && domain.contains('.') && !domain.contains('@')
        };
        let ipv4 = {
            let groups: Vec<&str> = text.split('.').collect();
            groups.len() == 4
                && groups.iter().all(|g| {
                    !g.is_empty()
                        && g.len() <= 3
                        && g.bytes().all(|b| b.is_ascii_digit())
                        && g.parse::<u16>().is_ok_and(|n| n <= 255)
                })
        };
        let named = dotted
            && text.bytes().any(|b| b.is_ascii_alphabetic())
            && !text.split('.').all(|label| label.chars().count() == 1);
        if !(email || ipv4 || named) {
            return None;
        }
        let mut with_port = end;
        if matches!(self.chars.get(with_port), Some(':' | ',')) {
            let digits = self
                .chars
                .get(with_port + 1..)
                .unwrap_or_default()
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            if (1..=5).contains(&digits) {
                with_port += 1 + digits;
            }
        }
        Some(with_port)
    }

    fn try_token(&mut self) -> bool {
        if !self.at_token_start() || self.peek(0).is_none() {
            return false;
        }
        if let Some(end) = self.url_end() {
            self.push_hashed("host", self.at, end);
            self.at = end;
            return true;
        }
        if self.explicit_path_start() {
            let end = self.token_end();
            self.push_hashed("path", self.at, end);
            self.at = end;
            return true;
        }
        if self.try_base64() {
            return true;
        }
        if let Some(end) = self.relative_path_end() {
            self.push_hashed("path", self.at, end);
            self.at = end;
            return true;
        }
        if let Some(end) = self.host_end() {
            self.push_hashed("host", self.at, end);
            self.at = end;
            return true;
        }
        false
    }

    fn try_fragment(&mut self) -> bool {
        let run = self.literals.fragment_run_at(&self.folded, self.at);
        if run == 0 {
            return false;
        }
        self.out.push_str(DROPPED);
        self.at += run;
        true
    }

    fn numeric_end(&self) -> Option<usize> {
        if self.prev().is_some_and(|p| p.is_alphanumeric() || p == '_') {
            return None;
        }
        let class = |c: char| c.is_ascii_hexdigit() || matches!(c, ':' | '.' | '-');
        let security_id = matches!(self.peek(0), Some('S' | 's'))
            && self.peek(1) == Some('-')
            && self.peek(2).is_some_and(|c| c.is_ascii_digit());
        let mut end = self.at + if security_id { 2 } else { 0 };
        let limit = self.at + ID_SCAN;
        while end < limit && self.chars.get(end).copied().is_some_and(class) {
            end += 1;
        }
        if end < limit && self.chars.get(end).copied().is_some_and(is_word_char) {
            let colons = self
                .chars
                .get(self.at..end)
                .unwrap_or_default()
                .iter()
                .filter(|c| **c == ':')
                .count();
            if colons < 2 {
                return None;
            }
            while end < limit
                && self
                    .chars
                    .get(end)
                    .copied()
                    .is_some_and(|c| class(c) || is_word_char(c))
            {
                end += 1;
            }
        }
        while end > self.at
            && self
                .chars
                .get(end - 1)
                .is_some_and(|c| matches!(c, ':' | '.' | '-'))
        {
            end -= 1;
        }
        let run = self.chars.get(self.at..end)?;
        if !run.iter().any(char::is_ascii_digit) {
            return None;
        }
        let count = |wanted: char| run.iter().filter(|c| **c == wanted).count();
        if security_id {
            return (count('-') >= 3).then_some(end);
        }
        if run.len() >= ID_RUN || count(':') >= 2 {
            return Some(end);
        }
        if !run.iter().all(char::is_ascii_digit) {
            return None;
        }
        let (mut digits, mut groups, mut next) = (run.len(), 1_usize, end);
        while self
            .chars
            .get(next)
            .is_some_and(|c| matches!(c, ' ' | '+' | '\''))
        {
            let start = next + 1;
            let mut stop = start;
            while self.chars.get(stop).is_some_and(char::is_ascii_digit) {
                stop += 1;
            }
            if stop == start || self.chars.get(stop).copied().is_some_and(is_word_char) {
                break;
            }
            digits += stop - start;
            groups += 1;
            next = stop;
        }
        (groups >= 2 && digits >= ID_RUN - 1).then_some(next)
    }

    fn try_numeric(&mut self) -> bool {
        let Some(end) = self.numeric_end() else {
            return false;
        };
        self.push_hashed("h", self.at, end);
        self.at = end;
        true
    }

    fn word_end(&self) -> usize {
        let mut end = self.at + 1;
        while let Some(&c) = self.chars.get(end) {
            if self.literals.longest_at(&self.folded, end).is_some() {
                break;
            }
            let joins = matches!(c, '-' | '\'')
                && self.chars.get(end - 1).is_some_and(|p| p.is_alphanumeric())
                && self.chars.get(end + 1).is_some_and(|n| n.is_alphanumeric())
                && self.literals.longest_at(&self.folded, end + 1).is_none();
            if is_word_char(c) || joins {
                end += 1;
            } else {
                break;
            }
        }
        end
    }

    fn try_word(&mut self) -> bool {
        if !self.peek(0).is_some_and(is_word_char) {
            return false;
        }
        if self.try_base64() {
            return true;
        }
        let end = self.word_end();
        let word = self.text(self.at, end);
        match classify_word(&word) {
            WordClass::Keep => self.out.push_str(&word),
            WordClass::Hash => self.push_hashed("h", self.at, end),
            WordClass::Drop => {
                self.out.push_str(DROPPED);
                self.unclassified += 1;
            }
        }
        self.at = end;
        true
    }
}
