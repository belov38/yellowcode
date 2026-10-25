use crate::model::DecodeError;
use std::collections::BTreeMap;

pub(crate) static MESSAGES_TSV: &str = include_str!("../data/messages.tsv");
pub(crate) static CATEGORIES_TSV: &str = include_str!("../data/categories.tsv");

fn embedded() -> &'static Messages {
    crate::resource::active().messages()
}

pub fn text(id: &str, lang: Lang) -> Option<&'static str> {
    embedded().text(id, lang)
}

pub fn render(id: &str, arg: Option<&str>, lang: Lang) -> Option<String> {
    embedded().render(id, arg, lang)
}

pub fn text_key(key: &str, lang: Lang) -> Option<&'static str> {
    embedded().text(bsl_id(key), lang)
}

pub fn render_key(key: &str, args: &[String], suffix: &str, lang: Lang) -> Option<String> {
    let mut text = fill(text_key(key, lang)?, args);
    text.push_str(suffix);
    Some(text)
}

pub fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut args = args.iter();
    let mut rest = template;
    while let Some(p) = rest.find('%') {
        out.push_str(&rest[..p]);
        let tail = &rest[p..];
        if (tail.starts_with("%d") || tail.starts_with("%s"))
            && let Some(a) = args.next()
        {
            out.push_str(a);
            rest = &tail[2..];
        } else {
            out.push('%');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn bsl_id(key: &str) -> &str {
    key.strip_prefix("bsl:").unwrap_or(key)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Wrapper {
    None,
    ContextCall,
    CtorCall,
    BuiltinArg,
    SetAttr,
    GetAttr,
    Compile,
}

impl Wrapper {
    pub const ALL: [Wrapper; 7] = [
        Wrapper::None,
        Wrapper::ContextCall,
        Wrapper::CtorCall,
        Wrapper::BuiltinArg,
        Wrapper::SetAttr,
        Wrapper::GetAttr,
        Wrapper::Compile,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Wrapper::None => "None",
            Wrapper::ContextCall => "ContextCall",
            Wrapper::CtorCall => "CtorCall",
            Wrapper::BuiltinArg => "BuiltinArg",
            Wrapper::SetAttr => "SetAttr",
            Wrapper::GetAttr => "GetAttr",
            Wrapper::Compile => "Compile",
        }
    }

    pub fn from_name(s: &str) -> Option<Wrapper> {
        Wrapper::ALL.into_iter().find(|w| w.name() == s)
    }

    pub fn key(self) -> Option<&'static str> {
        Some(match self {
            Wrapper::None => return None,
            Wrapper::ContextCall => "IDS_RTE_CONTEXTCALL",
            Wrapper::CtorCall => "IDS_RTE_CTORCALL",
            Wrapper::BuiltinArg => "IDS_RTE_BADARGVALUE",
            Wrapper::SetAttr => "IDS_RTE_CONTEXTSETPROPVAL",
            Wrapper::GetAttr => "IDS_RTE_CONTEXTGETPROPVAL",
            Wrapper::Compile => "IDS_RTE_COMPILE",
        })
    }
}

pub fn categories(wrapper: Wrapper, inner: &str) -> Option<u32> {
    embedded_categories().get(wrapper, bsl_id(inner))
}

fn embedded_categories() -> &'static CategoryTable {
    crate::resource::active().categories()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CategoryTable {
    map: BTreeMap<Wrapper, BTreeMap<String, u32>>,
}

impl CategoryTable {
    pub fn parse_tsv(text: &str) -> Result<CategoryTable, DecodeError> {
        let mut map = BTreeMap::new();
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = |what: &str| DecodeError(format!("line {}: {what}", i + 1));
            let mut parts = line.split('\t');
            let (Some(w), Some(id), Some(bits)) = (parts.next(), parts.next(), parts.next()) else {
                return Err(bad("expected wrapper, id and bits"));
            };
            let w = Wrapper::from_name(w).ok_or_else(|| bad("unknown wrapper"))?;
            if id.is_empty() {
                return Err(bad("empty id"));
            }
            let bits = bits
                .strip_prefix("0x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .ok_or_else(|| bad("bits are not 0x<hex>"))?;
            let ids: &mut BTreeMap<String, u32> = map.entry(w).or_default();
            if ids.insert(id.to_string(), bits).is_some() {
                return Err(bad("repeated wrapper and id"));
            }
        }
        Ok(CategoryTable { map })
    }

    pub fn get(&self, wrapper: Wrapper, id: &str) -> Option<u32> {
        self.map.get(&wrapper)?.get(id).copied()
    }

    pub fn len(&self) -> usize {
        self.map.values().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn rows(&self) -> impl Iterator<Item = (Wrapper, &str, u32)> {
        self.map
            .iter()
            .flat_map(|(w, ids)| ids.iter().map(|(id, bits)| (*w, id.as_str(), *bits)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Ru,
    En,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Messages {
    map: BTreeMap<String, (String, String)>,
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
}

fn unescape(s: &str, line: usize) -> Result<String, DecodeError> {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            _ => return Err(DecodeError(format!("line {line}: bad escape"))),
        }
    }
    Ok(out)
}

impl Messages {
    pub fn parse_tsv(text: &str) -> Result<Messages, DecodeError> {
        let mut map = BTreeMap::new();
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('\t');
            let (Some(id), Some(ru), Some(en), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(DecodeError(format!(
                    "line {}: expected 3 tab-separated fields",
                    i + 1
                )));
            };
            if id.is_empty() {
                return Err(DecodeError(format!("line {}: empty id", i + 1)));
            }
            map.insert(id.to_string(), (unescape(ru, i + 1)?, unescape(en, i + 1)?));
        }
        Ok(Messages { map })
    }

    pub fn to_tsv(&self) -> String {
        let mut out = String::new();
        for (id, (ru, en)) in &self.map {
            out.push_str(id);
            out.push('\t');
            escape(ru, &mut out);
            out.push('\t');
            escape(en, &mut out);
            out.push('\n');
        }
        out
    }

    pub fn from_rows(rows: Vec<(String, String, String)>) -> Messages {
        Messages {
            map: rows
                .into_iter()
                .map(|(id, ru, en)| (id, (ru, en)))
                .collect(),
        }
    }

    pub fn text(&self, id: &str, lang: Lang) -> Option<&str> {
        let (ru, en) = self.map.get(id)?;
        Some(match lang {
            Lang::Ru => ru,
            Lang::En => en,
        })
    }

    pub fn render(&self, id: &str, arg: Option<&str>, lang: Lang) -> Option<String> {
        let t = self.text(id, lang)?;
        Some(match arg {
            Some(a) => format!("{t} ({a})"),
            None => t.to_string(),
        })
    }
}
