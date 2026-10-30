use crate::data::{
    Casing, DeWords, EnWords, Forms, Hundreds, KkWords, Layout, SlavicWords, Spell, SpellLang,
};
use crate::error::{LibError, msg};
use crate::format::number::round_to;
use crate::format::spec::pairs;
use crate::format::{FALLBACK_LANGUAGE, words};
use crate::grammar::{Capital, Grammar, Part, SubjectShape, TreeLayout};
use crate::locale::Locale;
use crate::locale_key::{KeyCase, language_of_key, locale_key};
use crate::plural::{Category, NumberKind, Operands, category};
use bsl_prim::Number;

const WORDS_MAX_INT_DIGITS: usize = 15;
const DEFAULT_FRACTION: usize = 2;
const MAX_FRACTION: usize = 15;

struct WordsSpec {
    locale: Option<String>,
    subject_name: bool,
    fraction_name: bool,
    fraction_in_words: bool,
}

impl Default for WordsSpec {
    fn default() -> WordsSpec {
        WordsSpec {
            locale: None,
            subject_name: true,
            fraction_name: true,
            fraction_in_words: false,
        }
    }
}

fn bool_value(v: &str) -> Option<bool> {
    match v.trim().to_lowercase().as_str() {
        "истина" | "true" | "1" => Some(true),
        "ложь" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn parse_words_spec(fmt: &[u16]) -> Result<WordsSpec, LibError> {
    let mut s = WordsSpec {
        locale: locale_key(fmt, KeyCase::AnyCase),
        ..WordsSpec::default()
    };
    for (key, v) in pairs(fmt) {
        let set = |slot: &mut bool| {
            if let Some(b) = bool_value(&v) {
                *slot = b;
            }
        };
        match key.to_lowercase().as_str() {
            "нп" | "sn" => set(&mut s.subject_name),
            "нд" | "fn" => set(&mut s.fraction_name),
            "дп" | "fs" => set(&mut s.fraction_in_words),
            "ии" | "au" => match v.trim().to_lowercase().as_str() {
                "использовать" | "неиспользовать" | "use" | "dontuse" => {
                }
                _ => return Err(LibError::new(msg::FORMAT_STRING)),
            },
            _ => {}
        }
    }
    Ok(s)
}

struct Subject {
    int: Vec<String>,
    int_gender: u8,
    frac: Vec<String>,
    frac_gender: u8,
    digits: usize,
    extra: Vec<String>,
    given: bool,
}

pub(crate) fn gender(g: &str) -> u8 {
    match g.to_lowercase().as_str() {
        "ж" | "ż" | "f" => 1,
        "с" | "ń" | "n" => 2,
        _ => 0,
    }
}

impl Subject {
    fn parse(text: Option<&[u16]>, forms: usize, gendered: bool) -> Subject {
        Self::by_shape(
            text,
            SubjectShape {
                int_forms: forms,
                frac_forms: forms,
                gendered,
                extra: 0,
            },
            gender,
        )
    }

    fn by_shape(text: Option<&[u16]>, shape: SubjectShape, gender: impl Fn(&str) -> u8) -> Subject {
        let text = text.map(String::from_utf16_lossy).unwrap_or_default();
        let int_len = shape.int_forms + usize::from(shape.gendered);
        let frac_len = shape.frac_forms + usize::from(shape.gendered);
        let count = int_len + frac_len;
        let fields: Vec<&str> = text
            .split(',')
            .take(count + 1 + shape.extra)
            .map(str::trim)
            .collect();
        let field = |i: usize| fields.get(i).copied().unwrap_or("");
        let names =
            |from: usize, len: usize| (from..from + len).map(|i| field(i).to_string()).collect();
        let gender_at = |i: usize| if shape.gendered { gender(field(i)) } else { 0 };
        Subject {
            int: names(0, shape.int_forms),
            int_gender: gender_at(shape.int_forms),
            frac: names(int_len, shape.frac_forms),
            frac_gender: gender_at(int_len + shape.frac_forms),
            digits: field(count)
                .parse()
                .ok()
                .filter(|&d| d <= MAX_FRACTION)
                .unwrap_or(DEFAULT_FRACTION),
            extra: names(count + 1, shape.extra),
            given: !text.trim().is_empty(),
        }
    }
}

fn nth(words: &[String], i: usize) -> &str {
    words.get(i).map_or("", String::as_str)
}

struct Rules<'a> {
    language: &'a str,
    forms: Forms,
}

impl Rules<'_> {
    fn form(&self, n: u64, count: usize) -> usize {
        let last = count.saturating_sub(1);
        let k = match self.forms {
            Forms::LastDigitOne => usize::from(n % 10 != 1 || n % 100 == 11),
            Forms::Cldr => {
                match category(self.language, NumberKind::Cardinal, &Operands::of_u64(n)) {
                    Category::One => 0,
                    Category::Few if count == 3 => 1,
                    _ => last,
                }
            }
        };
        k.min(last)
    }
}

pub(crate) fn triads(n: u64) -> [usize; 5] {
    let mut t = [0; 5];
    let mut n = n;
    for slot in &mut t {
        *slot = (n % 1000) as usize;
        n /= 1000;
    }
    t
}

pub(crate) fn digits3(t: usize) -> (usize, usize, usize) {
    (t / 100 % 10, t / 10 % 10, t % 10)
}

fn slavic_triad<'a>(w: &'a SlavicWords, t: usize, gender: u8, out: &mut Vec<&'a str>) {
    let (h, d, u) = digits3(t);
    if h > 0 {
        out.push(nth(&w.hundreds, h - 1));
    }
    match (d, u) {
        (1, 0) => out.push(nth(&w.tens, 0)),
        (1, u) => out.push(nth(&w.teens, u - 1)),
        _ => {
            if d > 1 {
                out.push(nth(&w.tens, d - 1));
            }
            match u {
                0 => {}
                1 => out.push(nth(&w.one, usize::from(gender))),
                2 => out.push(nth(&w.two, usize::from(gender == 1))),
                u => out.push(nth(&w.units, u - 3)),
            }
        }
    }
}

fn slavic(w: &SlavicWords, n: u64, gender: u8, r: &Rules<'_>) -> String {
    if n == 0 {
        return w.zero.clone();
    }
    let t = triads(n);
    let mut out = Vec::new();
    for (&k, g) in t[1..].iter().zip(&w.groups).rev() {
        if k > 0 {
            slavic_triad(w, k, g.gender, &mut out);
            out.push(nth(&g.forms, r.form(k as u64, 3)));
        }
    }
    slavic_triad(w, t[0], gender, &mut out);
    out.join(" ")
}

fn push_hundreds<'a>(
    units: &'a [String],
    hundreds: &'a Hundreds,
    h: usize,
    r: &Rules<'_>,
    out: &mut Vec<&'a str>,
) {
    match hundreds {
        Hundreds::List(l) => out.push(nth(l, h - 1)),
        Hundreds::Word(forms) => {
            out.push(nth(units, h));
            out.push(nth(forms, r.form(h as u64, forms.len())));
        }
    }
}

fn en_triad<'a>(w: &'a EnWords, t: usize, r: &Rules<'_>, out: &mut Vec<&'a str>) {
    let (h, d, u) = digits3(t);
    if h > 0 {
        push_hundreds(&w.units, &w.hundreds, h, r, out);
    }
    match (d, u) {
        (0, 0) => {}
        (0, u) => out.push(nth(&w.units, u)),
        (1, 0) => out.push(nth(&w.tens, 0)),
        (1, u) => out.push(nth(&w.teens, u - 1)),
        (d, 0) => out.push(nth(&w.tens, d - 1)),
        (d, u) => {
            out.push(nth(&w.tens, d - 1));
            out.push(nth(&w.units, u));
        }
    }
}

fn en(w: &EnWords, n: u64, r: &Rules<'_>) -> String {
    if n == 0 {
        return nth(&w.units, 0).to_string();
    }
    let t = triads(n);
    let mut out = Vec::new();
    for (&k, g) in t[1..].iter().zip(&w.groups).rev() {
        if k > 0 {
            en_triad(w, k, r, &mut out);
            out.push(nth(g, r.form(k as u64, 2)));
        }
    }
    en_triad(w, t[0], r, &mut out);
    out.join(" ")
}

fn de_triad(w: &DeWords, t: usize, gender: u8) -> String {
    let (h, d, u) = digits3(t);
    let mut s = String::new();
    if h > 0 {
        s.push_str(nth(&w.hundreds, h - 1));
    }
    let unit = |u: usize, g: u8| match u {
        1 => nth(&w.one, usize::from(g)),
        u => u.checked_sub(2).map_or("", |k| nth(&w.units, k)),
    };
    match (d, u) {
        (0, 0) => {}
        (0, u) => s.push_str(unit(u, gender)),
        (1, 0) => s.push_str(nth(&w.tens, 0)),
        (1, u) => s.push_str(nth(&w.teens, u - 1)),
        (d, 0) => s.push_str(nth(&w.tens, d - 1)),
        (d, u) => {
            s.push_str(unit(u, 0));
            s.push_str(&w.und);
            s.push_str(nth(&w.tens, d - 1));
        }
    }
    s
}

fn de(w: &DeWords, n: u64, gender: u8, r: &Rules<'_>) -> String {
    if n == 0 {
        return w.zero.clone();
    }
    let t = triads(n);
    let mut parts = Vec::new();
    for (&k, g) in t[2..].iter().zip(&w.groups).rev() {
        if k > 0 {
            let name = nth(&g.forms, r.form(k as u64, 2));
            parts.push(format!("{} {name}", de_triad(w, k, g.gender)));
        }
    }
    let mut tail = String::new();
    if t[1] > 0 {
        tail.push_str(&de_triad(w, t[1], w.thousand.1));
        tail.push_str(&w.thousand.0);
    }
    if t[0] > 0 {
        tail.push_str(&de_triad(w, t[0], gender));
    }
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts.join(" ")
}

fn kk_triad<'a>(w: &'a KkWords, t: usize, r: &Rules<'_>, out: &mut Vec<&'a str>) {
    let (h, d, u) = digits3(t);
    if h > 0 {
        push_hundreds(&w.units, &w.hundreds, h, r, out);
    }
    if d > 0 {
        out.push(nth(&w.tens, d - 1));
    }
    if u > 0 {
        out.push(nth(&w.units, u));
    }
}

fn kk(w: &KkWords, n: u64, r: &Rules<'_>) -> String {
    if n == 0 {
        return nth(&w.units, 0).to_string();
    }
    let t = triads(n);
    let mut out = Vec::new();
    for (&k, g) in t[1..].iter().zip(&w.groups).rev() {
        if k > 0 {
            kk_triad(w, k, r, &mut out);
            out.push(g);
        }
    }
    kk_triad(w, t[0], r, &mut out);
    out.join(" ")
}

fn split(n: &Number, d: usize) -> Option<(u64, String)> {
    let mut int = n.int_digits().into_bytes();
    if int.len() > WORDS_MAX_INT_DIGITS {
        return None;
    }
    let mut frac = n.frac_digits().into_bytes();
    round_to(&mut int, &mut frac, d);
    if int.len() > WORDS_MAX_INT_DIGITS {
        return None;
    }
    let int = std::str::from_utf8(&int).ok()?.parse().ok()?;
    Some((int, String::from_utf8(frac).ok()?))
}

fn cased(s: String, casing: Casing) -> String {
    match casing {
        Casing::Plain => s,
        Casing::Turkic => {
            let mut out = String::with_capacity(s.len());
            for c in s.chars() {
                match c {
                    'İ' => out.push('i'),
                    'I' => out.push('ı'),
                    c => out.extend(c.to_lowercase()),
                }
            }
            out
        }
    }
}

fn compose(
    sl: &SpellLang,
    language: &str,
    (int, frac): (u64, &str),
    s: &Subject,
    spec: &WordsSpec,
) -> String {
    let r = Rules {
        language,
        forms: sl.forms,
    };
    let spell = |n: u64, gender: u8| {
        let w = match &sl.words {
            Spell::Slavic(w) => slavic(w, n, gender, &r),
            Spell::En(w) => en(w, n, &r),
            Spell::De(w) => de(w, n, gender, &r),
            Spell::Kk(w) => kk(w, n, &r),
        };
        cased(w, sl.casing)
    };
    let frac_value: u64 = frac.parse().unwrap_or(0);
    let words = spell(int, s.int_gender);
    let frac_text = if spec.fraction_in_words && !frac.is_empty() {
        spell(frac_value, s.frac_gender)
    } else {
        frac.to_string()
    };
    let int_name = nth(&s.int, r.form(int, s.int.len()));
    let frac_name = nth(&s.frac, r.form(frac_value, s.frac.len()));
    match sl.layout {
        Layout::Spaced => {
            let mut out = words;
            let round = int.is_multiple_of(1000) && int != 0;
            if spec.subject_name && !(round && int_name.is_empty()) {
                out.push(' ');
                out.push_str(int_name);
            }
            if !frac.is_empty() {
                out.push(' ');
                out.push_str(&frac_text);
                if spec.fraction_name {
                    out.push(' ');
                    out.push_str(frac_name);
                }
            }
            out
        }
        Layout::Compact => {
            let named = spec.subject_name && !int_name.is_empty();
            let mut parts = vec![words.as_str()];
            if named {
                parts.push(int_name);
            }
            if !frac.is_empty() {
                if let (Spell::De(w), true) = (&sl.words, named) {
                    parts.push(&w.und);
                }
                parts.push(&frac_text);
                if spec.fraction_name && !frac_name.is_empty() {
                    parts.push(frac_name);
                }
            }
            parts.join(" ")
        }
    }
}

pub fn number_in_words(
    n: &Number,
    fmt: Option<&[u16]>,
    subject: Option<&[u16]>,
    session: &Locale,
) -> Result<String, LibError> {
    let err = || LibError::new(msg::PARAM_VALUE).with_arg(1);
    if n.int_digits().len() > WORDS_MAX_INT_DIGITS {
        return Err(err());
    }
    let spec = match fmt {
        Some(f) => parse_words_spec(f)?,
        None => WordsSpec::default(),
    };
    let language = language_of_key(spec.locale.as_deref(), session);
    let language = language.as_str();
    let all = &words().spell;
    if !all.contains_key(language)
        && let Some(grammar) = crate::grammar::grammar(language)
    {
        let subject = Subject::by_shape(subject, grammar.subject_shape(), |field| {
            grammar.gender(field)
        });
        let (int, frac) = split(n, subject.digits).ok_or_else(err)?;
        let text = compose_tree(grammar, (int, &frac), &subject, &spec);
        return Ok(finish(text, n, int, &frac, grammar.capital()));
    }
    let (language, sl) = all
        .get_key_value(language)
        .or_else(|| all.get_key_value(FALLBACK_LANGUAGE))
        .ok_or_else(err)?;
    let s = match &sl.words {
        Spell::Slavic(_) => Subject::parse(subject, 3, true),
        Spell::De(_) => Subject::parse(subject, 2, true),
        Spell::En(_) | Spell::Kk(_) => Subject::parse(subject, 2, false),
    };
    let (int, frac) = split(n, s.digits).ok_or_else(err)?;
    let text = compose(sl, language, (int, &frac), &s, &spec);
    let capital = match sl.casing {
        Casing::Plain => Capital::Upper,
        Casing::Turkic => Capital::Turkic,
    };
    Ok(finish(text, n, int, &frac, capital))
}

fn finish(text: String, n: &Number, int: u64, frac: &str, capital: Capital) -> String {
    let negative = n.is_negative() && (int != 0 || frac.bytes().any(|c| c != b'0'));
    let mut out = String::with_capacity(text.len() + 1);
    if negative {
        out.push('-');
    }
    let mut chars = text.chars();
    match (chars.next(), capital) {
        (Some('i'), Capital::Turkic) => out.push('İ'),
        (Some('ı'), Capital::Turkic) => out.push('I'),
        (Some(c), Capital::Keep) => out.push(c),
        (Some(c), _) => out.extend(c.to_uppercase()),
        (None, _) => {}
    }
    out.push_str(chars.as_str());
    out
}

fn compose_tree(
    g: &dyn Grammar,
    (int, frac): (u64, &str),
    s: &Subject,
    spec: &WordsSpec,
) -> String {
    let value = frac.parse().unwrap_or(0);
    let name = nth(&s.int, g.form(int, Part::Int));
    let part = if spec.fraction_in_words {
        Part::FractionWords
    } else {
        Part::FractionDigits
    };
    let frac_name = nth(&s.frac, g.form(value, part));
    let mut out = g.spell(int, s.int_gender, &s.extra);
    let fraction = if spec.fraction_in_words {
        g.fraction_words(frac, s.frac_gender, &s.extra)
    } else {
        g.fraction_digits(frac)
    };
    match g.layout() {
        TreeLayout::Single => {
            if spec.subject_name && !name.is_empty() {
                out.push(' ');
                out.push_str(name);
            }
            if !frac.is_empty() {
                if let Some(joint) = g.joint().filter(|_| s.given) {
                    out.push(' ');
                    out.push_str(joint);
                }
                out.push(' ');
                out.push_str(&fraction);
                if spec.fraction_name && !frac_name.is_empty() {
                    out.push(' ');
                    out.push_str(frac_name);
                }
            }
        }
        TreeLayout::Spaced { round } => {
            if spec.subject_name
                && !(round && int != 0 && int.is_multiple_of(1000) && name.is_empty())
            {
                out.push(' ');
                out.push_str(name);
            }
            if !frac.is_empty() {
                out.push(' ');
                out.push_str(&fraction);
                if spec.fraction_name {
                    out.push(' ');
                    out.push_str(frac_name);
                }
            }
        }
        TreeLayout::Comma => {
            let sep = if s.extra.first().is_some_and(|v| v == "Separate") {
                " "
            } else {
                ""
            };
            if spec.subject_name && !name.is_empty() {
                out.push_str(sep);
                out.push_str(name);
            }
            if !frac.is_empty() {
                out.push_str(", ");
                out.push_str(&fraction);
                if spec.fraction_name && !frac_name.is_empty() {
                    out.push_str(sep);
                    out.push_str(frac_name);
                }
            }
        }
        TreeLayout::Units => {
            if s.given && spec.subject_name {
                out.push_str(nth(&s.int, 0));
            } else {
                out.push('点');
            }
            if !frac.is_empty() {
                let digits = g.fraction_digits(frac);
                let mut chars = digits.chars();
                if let Some(first) = chars.next() {
                    out.push(first);
                }
                out.push_str(nth(&s.frac, 0));
                out.push_str(chars.as_str());
                out.push_str(nth(&s.frac, 1));
            }
        }
    }
    out
}
