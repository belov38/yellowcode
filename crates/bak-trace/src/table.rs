use std::collections::HashSet;

const MAX_DIGITS: usize = 10;
const MAX_NAME: usize = 128;

const EXTENSION_HISTORY_TABLES: [&str; 4] = [
    "_DataHistoryLatestVerExt",
    "_DataHistoryMetadataExt",
    "_DataHistorySettingsExt",
    "_DataHistoryVersionsExt",
];

fn is_extension_history_base(base: &str) -> bool {
    EXTENSION_HISTORY_TABLES
        .iter()
        .any(|known| base.eq_ignore_ascii_case(known))
}

#[derive(Clone, PartialEq, Eq)]
pub struct ClearTable(String);

impl ClearTable {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ClearTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClearTable({:?})", self.0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct TableVocabulary {
    known: HashSet<String>,
    words: HashSet<String>,
}

impl TableVocabulary {
    pub fn new<'a>(known: impl IntoIterator<Item = &'a str>) -> Self {
        let mut vocabulary = TableVocabulary::default();
        for name in known {
            if !name.is_ascii() {
                continue;
            }
            vocabulary.known.insert(name.to_ascii_lowercase());
            if let Some(rest) = name.strip_prefix('_') {
                for run in rest.split(|c: char| !c.is_ascii_alphabetic()) {
                    if !run.is_empty() {
                        vocabulary.words.insert(run.to_owned());
                    }
                }
            }
        }
        vocabulary
    }

    pub fn classify(&self, name: &str) -> Option<ClearTable> {
        if name.is_empty() || name.len() > MAX_NAME || !name.is_ascii() {
            return None;
        }
        let folded = name.to_ascii_lowercase();
        if self.known.contains(&folded) {
            return Some(ClearTable(name.to_owned()));
        }
        if let Some(base) = strip_extension_suffix(name)
            && (self.known.contains(&base.to_ascii_lowercase()) || is_extension_history_base(base))
        {
            return Some(ClearTable(name.to_owned()));
        }
        self.generated(name).then(|| ClearTable(name.to_owned()))
    }

    fn generated(&self, name: &str) -> bool {
        let Some(rest) = name.strip_prefix('_') else {
            return false;
        };
        let body = strip_extension_suffix(name)
            .and_then(|base| base.strip_prefix('_'))
            .unwrap_or(rest);
        let bytes = body.as_bytes();
        let mut at = 0;
        let mut groups = 0;
        while at < bytes.len() {
            if groups > 0 && bytes.get(at) == Some(&b'_') {
                at += 1;
            }
            let start = at;
            while bytes.get(at).is_some_and(u8::is_ascii_alphabetic) {
                at += 1;
            }
            let Some(word) = body.get(start..at) else {
                return false;
            };
            if word.is_empty() || !self.words.contains(word) {
                return false;
            }
            let digits = at;
            while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
            let count = at - digits;
            if count == 0 || count > MAX_DIGITS {
                return false;
            }
            groups += 1;
        }
        groups > 0
    }
}

pub fn strip_extension_suffix(name: &str) -> Option<&str> {
    extension_suffix(name).map(|(base, _)| base)
}

pub fn extension_suffix(name: &str) -> Option<(&str, u32)> {
    let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || digits > MAX_DIGITS {
        return None;
    }
    let number_at = name.len() - digits;
    let x_at = number_at.checked_sub(1)?;
    if name.as_bytes().get(x_at) != Some(&b'X') {
        return None;
    }
    let base = name.get(..x_at)?;
    if !base.as_bytes().last().is_some_and(u8::is_ascii_digit) && !is_extension_history_base(base) {
        return None;
    }
    let number = name.get(number_at..)?.parse().ok()?;
    Some((base, number))
}
