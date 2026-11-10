pub(crate) use crate::gaps::is_gap;
use crate::hasher::fold_char;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiteralKind {
    Name,
    Path,
    Server,
    Secret,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Needle {
    pub chars: Vec<char>,
    pub whole_words: bool,
}

pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

const SKELETON_MIN: usize = 4;

const FRAGMENT_MIN: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Skeleton {
    pub chars: Vec<char>,
    pub original: Vec<char>,
    pub whole_words: bool,
}

fn qualifies_as_component(chars: &[char], kind: LiteralKind) -> bool {
    chars.len() >= 4
        && chars.iter().any(|c| c.is_alphanumeric())
        && (kind == LiteralKind::Path || !chars.iter().all(char::is_ascii_digit))
}

pub(crate) fn forms_of(value: &str) -> Vec<String> {
    let value = value.trim();
    let mut forms: Vec<String> = Vec::new();
    let mut push = |form: String| {
        if !form.is_empty() && form != value && !forms.contains(&form) {
            forms.push(form);
        }
    };
    let percent = |keep: &dyn Fn(u8) -> bool, space_as_plus: bool| -> String {
        value
            .bytes()
            .map(|byte| {
                if space_as_plus && byte == b' ' {
                    "+".to_owned()
                } else if keep(byte) {
                    char::from(byte).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect()
    };
    push(percent(
        &|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'),
        false,
    ));
    push(percent(
        &|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'*'),
        true,
    ));
    push(percent(
        &|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'-' | b'.' | b'_' | b'~' | b'!' | b'*' | b'\'' | b'(' | b')'
                )
        },
        false,
    ));
    push(percent(&|_| false, false));
    let punctuation =
        |c: char| c.is_ascii() && !c.is_ascii_alphanumeric() && !c.is_ascii_control() && c != ' ';
    push(
        value
            .chars()
            .map(|c| match c {
                '&' => "&amp;".to_owned(),
                '<' => "&lt;".to_owned(),
                '>' => "&gt;".to_owned(),
                '"' => "&quot;".to_owned(),
                '\'' => "&#39;".to_owned(),
                other => other.to_string(),
            })
            .collect(),
    );
    push(
        value
            .chars()
            .map(|c| {
                if punctuation(c) {
                    format!("&#{};", u32::from(c))
                } else {
                    c.to_string()
                }
            })
            .collect(),
    );
    push(
        value
            .chars()
            .map(|c| {
                if punctuation(c) {
                    format!("&#x{:X};", u32::from(c))
                } else {
                    c.to_string()
                }
            })
            .collect(),
    );
    let quoted = crate::value::json_string(value);
    if let Some(inner) = quoted.strip_prefix('"').and_then(|q| q.strip_suffix('"')) {
        push(inner.to_owned());
    }
    let debug = format!("{value:?}");
    if let Some(inner) = debug.strip_prefix('"').and_then(|q| q.strip_suffix('"')) {
        push(inner.to_owned());
    }
    push(value.replace('\'', "''"));
    push(value.replace('"', "\"\""));
    push(value.replace('\'', "'\\''"));
    push(value.replace(']', "]]"));
    forms
}

fn component_candidates(kind: LiteralKind, value: &str) -> Vec<String> {
    let separators: &[char] = match kind {
        LiteralKind::Name | LiteralKind::Secret => return Vec::new(),
        LiteralKind::Path => &['/', '\\'],
        LiteralKind::Server => &['/', '\\', ':', ','],
    };
    let mut found: Vec<String> = Vec::new();
    for piece in value.trim().split(|c| separators.contains(&c)) {
        let text = piece.trim();
        if matches!(text, "" | "." | "..") {
            continue;
        }
        let mut candidates: Vec<String> = vec![text.to_owned()];
        let chars: Vec<char> = text.chars().collect();
        if let Some(dot) = chars.iter().rposition(|c| *c == '.')
            && dot > 0
        {
            candidates.push(chars.iter().take(dot).collect());
        }
        if text.contains(char::is_whitespace) {
            candidates.extend(text.split_whitespace().map(str::to_owned));
        }
        for candidate in candidates {
            let letters: Vec<char> = candidate.chars().collect();
            if qualifies_as_component(&letters, kind) && !found.contains(&candidate) {
                found.push(candidate);
            }
        }
    }
    found
}

pub(crate) fn needles(kind: LiteralKind, value: &str) -> Vec<Needle> {
    let folded: Vec<char> = value.trim().chars().map(fold_char).collect();
    let searchable = folded.iter().any(|c| c.is_alphanumeric()) || folded.len() >= 3;
    if !searchable {
        return Vec::new();
    }
    let mut found: Vec<Needle> = Vec::new();
    let mut add = |chars: Vec<char>, whole_words: bool| {
        if !chars.is_empty() && !found.iter().any(|n| n.chars == chars) {
            found.push(Needle { chars, whole_words });
        }
    };
    add(folded.clone(), folded.len() <= 2);
    for candidate in component_candidates(kind, value) {
        add(candidate.chars().map(fold_char).collect(), true);
    }
    found
}

#[derive(Default, Clone)]
pub(crate) struct LiteralSet {
    by_first: HashMap<char, Vec<Needle>>,
    skeletons: HashMap<char, Vec<Skeleton>>,
    fragments: Vec<Vec<char>>,
}

impl LiteralSet {
    pub fn add(&mut self, kind: LiteralKind, value: &str) {
        for needle in needles(kind, value) {
            self.insert(needle);
        }
        let form_kind = if kind == LiteralKind::Secret {
            LiteralKind::Secret
        } else {
            LiteralKind::Name
        };
        let sources = std::iter::once(value.to_owned()).chain(component_candidates(kind, value));
        for source in sources {
            for form in forms_of(&source) {
                for needle in needles(form_kind, &form) {
                    self.insert(needle);
                }
            }
        }
        self.add_fragments(value);
    }

    fn add_fragments(&mut self, value: &str) {
        let folded: Vec<char> = value.trim().chars().map(fold_char).collect();
        let mut run: Vec<char> = Vec::new();
        for c in folded.iter().copied().chain(std::iter::once(' ')) {
            if c.is_alphabetic() || is_gap(c) {
                if run.len() >= FRAGMENT_MIN && !self.fragments.contains(&run) {
                    self.fragments.push(run.clone());
                }
                run.clear();
            } else {
                run.push(c);
            }
        }
    }

    pub fn add_forms(&mut self, _kind: LiteralKind, value: &str) {
        let value = value.trim();
        if !value.chars().any(|c| c.is_alphanumeric()) {
            return;
        }
        let folded = |text: &str| -> Vec<char> { text.chars().map(fold_char).collect() };
        let quoted = crate::value::json_string(value);
        if let Some(escaped) = quoted.strip_prefix('"').and_then(|q| q.strip_suffix('"')) {
            let escaped = folded(escaped);
            if escaped != folded(value) {
                self.insert(Needle {
                    chars: escaped,
                    whole_words: false,
                });
            }
        }
        let percent: String = value
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                    char::from(byte).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect();
        if percent != value {
            self.insert(Needle {
                chars: folded(&percent),
                whole_words: false,
            });
        }
        let short = value.chars().count() < 4;
        for variant in [value.to_owned(), value.to_lowercase(), value.to_uppercase()] {
            let utf16: String = variant
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let utf8: String = variant.bytes().map(|byte| format!("{byte:02x}")).collect();
            for hex in [utf16, utf8] {
                if hex.len() >= 4 {
                    self.insert(Needle {
                        chars: folded(&hex),
                        whole_words: short,
                    });
                }
            }
        }
    }

    pub fn merged(&self, other: &LiteralSet) -> LiteralSet {
        let mut together = self.clone();
        for needle in other.all() {
            together.insert(needle);
        }
        together
    }

    fn insert(&mut self, needle: Needle) {
        let Some(first) = needle.chars.first().copied() else {
            return;
        };
        let skeleton: Vec<char> = needle
            .chars
            .iter()
            .copied()
            .filter(|c| !is_gap(*c))
            .collect();
        if skeleton.len() >= SKELETON_MIN
            && let Some(skeleton_first) = skeleton.first().copied()
        {
            let entry = Skeleton {
                chars: skeleton,
                original: needle.chars.clone(),
                whole_words: needle.whole_words,
            };
            let list = self.skeletons.entry(skeleton_first).or_default();
            if !list.contains(&entry) {
                list.push(entry);
                list.sort_by_key(|s| std::cmp::Reverse(s.chars.len()));
            }
        }
        let list = self.by_first.entry(first).or_default();
        if !list.contains(&needle) {
            list.push(needle);
            list.sort_by_key(|n| std::cmp::Reverse(n.chars.len()));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.by_first.is_empty()
    }

    pub fn has_skeletons(&self) -> bool {
        !self.skeletons.is_empty()
    }

    pub fn has_fragments(&self) -> bool {
        !self.fragments.is_empty()
    }

    pub fn longest_skeleton_at(&self, skeleton: &[char], at: usize) -> Option<(usize, &Skeleton)> {
        let list = self.skeletons.get(skeleton.get(at)?)?;
        list.iter().find_map(|entry| {
            let end = at.checked_add(entry.chars.len())?;
            (skeleton.get(at..end) == Some(entry.chars.as_slice()))
                .then_some((entry.chars.len(), entry))
        })
    }

    pub fn longest_skeleton(&self) -> usize {
        self.skeletons
            .values()
            .flatten()
            .map(|entry| entry.chars.len())
            .max()
            .unwrap_or(0)
    }

    pub fn skeleton_in_progress(&self, rest: &[char]) -> bool {
        let Some(list) = rest.first().and_then(|first| self.skeletons.get(first)) else {
            return false;
        };
        list.iter()
            .any(|entry| entry.chars.len() > rest.len() && entry.chars.starts_with(rest))
    }

    pub fn fragment_run_at(&self, folded: &[char], at: usize) -> usize {
        let Some(rest) = folded.get(at..) else {
            return 0;
        };
        let Some(&first) = rest.first() else {
            return 0;
        };
        let mut best = 0_usize;
        for fragment in &self.fragments {
            for (start, c) in fragment.iter().enumerate() {
                if *c != first {
                    continue;
                }
                let tail = fragment.get(start..).unwrap_or_default();
                let run = tail
                    .iter()
                    .zip(rest.iter())
                    .take_while(|(a, b)| a == b)
                    .count();
                best = best.max(run);
            }
        }
        if best >= FRAGMENT_MIN { best } else { 0 }
    }

    pub fn longest_at(&self, folded: &[char], at: usize) -> Option<usize> {
        let list = self.by_first.get(folded.get(at)?)?;
        list.iter().find_map(|needle| {
            let end = at.checked_add(needle.chars.len())?;
            if folded.get(at..end) != Some(needle.chars.as_slice()) {
                return None;
            }
            if needle.whole_words {
                let before = at
                    .checked_sub(1)
                    .and_then(|i| folded.get(i))
                    .is_none_or(|c| !is_word_char(*c));
                let after = folded.get(end).is_none_or(|c| !is_word_char(*c));
                if !(before && after) {
                    return None;
                }
            }
            Some(needle.chars.len())
        })
    }

    pub fn partial_tail(&self, folded: &[char]) -> usize {
        let longest = self
            .by_first
            .values()
            .flatten()
            .map(|needle| needle.chars.len())
            .max()
            .unwrap_or(0);
        let first = folded.len().saturating_sub(longest.saturating_sub(1));
        (first..folded.len())
            .find(|start| {
                let tail = folded.get(*start..).unwrap_or_default();
                tail.first()
                    .and_then(|c| self.by_first.get(c))
                    .is_some_and(|list| {
                        list.iter().any(|needle| {
                            needle.chars.len() > tail.len() && needle.chars.starts_with(tail)
                        })
                    })
            })
            .map_or(0, |start| folded.len() - start)
    }

    pub fn partial_head(&self, folded: &[char]) -> usize {
        let longest = folded.len();
        (1..=longest)
            .rev()
            .find(|length| {
                let head = folded.get(..*length).unwrap_or_default();
                self.by_first
                    .values()
                    .flatten()
                    .any(|needle| needle.chars.len() > head.len() && needle.chars.ends_with(head))
            })
            .unwrap_or(0)
    }

    pub fn all(&self) -> Vec<Needle> {
        let mut all: Vec<Needle> = self.by_first.values().flatten().cloned().collect();
        all.sort_by_key(|n| std::cmp::Reverse(n.chars.len()));
        all
    }
}
