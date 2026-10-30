use crate::format::spec::pairs_with;
use crate::locale::Locale;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCase {
    Exact,
    AnyCase,
}

pub(crate) fn from_pairs(pairs: &[(String, String)], case: KeyCase) -> Option<String> {
    pairs
        .iter()
        .rev()
        .find(|(key, _)| match case {
            KeyCase::Exact => matches!(key.as_str(), "Л" | "L"),
            KeyCase::AnyCase => matches!(key.to_lowercase().as_str(), "л" | "l"),
        })
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
        .cloned()
}

pub fn locale_key(fmt: &[u16], case: KeyCase) -> Option<String> {
    from_pairs(&pairs_with(fmt, case == KeyCase::Exact), case)
}

pub fn language_of_key(key: Option<&str>, session: &Locale) -> String {
    match key {
        Some(name) => {
            let end = name.find(['_', '-']).unwrap_or(name.len());
            name[..end].to_ascii_lowercase()
        }
        None => session.language().to_string(),
    }
}
