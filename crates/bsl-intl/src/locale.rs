use crate::data::{LocaleData, decode_locales};
use std::sync::OnceLock;

static LOCALES_BIN: &[u8] = include_bytes!("../data/locales.bin");

#[derive(Debug)]
pub struct Locale {
    data: LocaleData,
    language_len: usize,
}

impl Locale {
    fn new(data: LocaleData) -> Locale {
        let language_len = data.name.find('_').unwrap_or(data.name.len());
        Locale { data, language_len }
    }

    pub fn name(&self) -> &str {
        &self.data.name
    }

    pub fn data(&self) -> &LocaleData {
        &self.data
    }

    pub fn language(&self) -> &str {
        &self.data.name[..self.language_len]
    }
}

pub fn locales() -> &'static [Locale] {
    static LOCALES: OnceLock<Vec<Locale>> = OnceLock::new();
    LOCALES.get_or_init(|| {
        decode_locales(LOCALES_BIN)
            .map(|d| build(d.locales))
            .unwrap_or_default()
    })
}

fn build(data: Vec<LocaleData>) -> Vec<Locale> {
    let mut v: Vec<Locale> = data
        .into_iter()
        .filter(|d| d.months.iter().all(|m| m.len() == 12) && d.days.iter().all(|m| m.len() == 7))
        .map(Locale::new)
        .collect();
    v.sort_by(|a, b| a.data.name.cmp(&b.data.name));
    v
}

pub fn locale(name: &str) -> Option<&'static Locale> {
    let all = locales();
    all.binary_search_by(|l| l.data.name.as_str().cmp(name))
        .ok()
        .map(|i| &all[i])
}
