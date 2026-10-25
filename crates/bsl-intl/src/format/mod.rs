mod date;
pub(crate) mod number;
pub mod spec;

use crate::data::{WordsData, decode_words};
use crate::locale::{Locale, locale};
use crate::present::bool_to_string;
use crate::words::language_of;
use bsl_prim::{BslDate, Number};
pub use spec::{FormatSpec, parse_spec};
use std::sync::OnceLock;

static WORDS_BIN: &[u8] = include_bytes!("../../data/words.bin");

pub const FALLBACK_LANGUAGE: &str = "en";

pub fn words() -> &'static WordsData {
    static WORDS: OnceLock<WordsData> = OnceLock::new();
    WORDS.get_or_init(|| decode_words(WORDS_BIN).unwrap_or_default())
}

#[derive(Clone, Copy, Debug)]
pub enum FmtValue<'a> {
    Num(&'a Number),
    Date(BslDate),
    Bool(bool),
}

pub struct FormatEnv<'a> {
    pub session: &'a Locale,
}

fn format_bool(b: bool, spec: &FormatSpec, session: &Locale) -> String {
    if let Some(t) = if b { &spec.bt } else { &spec.bf } {
        return t.clone();
    }
    let all = &words().bool_words;
    let found = match spec.locale.as_deref() {
        None => return bool_to_string(b).to_string(),
        Some(name) => match locale(name) {
            Some(loc) => all
                .get(loc.language())
                .or_else(|| all.get(FALLBACK_LANGUAGE)),
            None => all.get(&language_of(Some(name), session)),
        },
    };
    match found {
        Some((t, _)) if b => t.clone(),
        Some((_, Some(f))) => f.clone(),
        _ => bool_to_string(b).to_string(),
    }
}

pub fn format(v: FmtValue<'_>, fmt: &[u16], env: &FormatEnv<'_>) -> String {
    let spec = parse_spec(fmt);
    let loc = spec
        .locale
        .as_deref()
        .and_then(locale)
        .unwrap_or(env.session);
    match v {
        FmtValue::Num(n) => number::format_number(n, &spec, loc),
        FmtValue::Bool(b) => format_bool(b, &spec, env.session),
        FmtValue::Date(d) => date::format_date(d, &spec, loc),
    }
}
