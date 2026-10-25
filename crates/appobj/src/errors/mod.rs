#[rustfmt::skip]
mod msg_gen;

use platform_catalog::messages::Lang;

pub use msg_gen::keys;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct L5Msg {
    pub key: &'static str,
    pub ru: &'static str,
    pub en: &'static str,
}

pub fn msg(key: &str) -> Option<&'static L5Msg> {
    let table = msg_gen::MESSAGES;
    table
        .binary_search_by(|m| m.key.cmp(key))
        .ok()
        .and_then(|i| table.get(i))
}

pub fn type_presentation_key(kind_en: &str, category: &str) -> Option<&'static str> {
    let table = msg_gen::TYPE_PRESENTATIONS;
    table
        .binary_search_by(|(k, c, _)| (*k, *c).cmp(&(kind_en, category)))
        .ok()
        .and_then(|i| table.get(i))
        .map(|(_, _, key)| *key)
}

pub fn render(key: &str, args: &[&str], lang: Lang) -> String {
    let Some(m) = msg(key) else {
        return key.to_string();
    };
    let tpl = match lang {
        Lang::Ru => m.ru,
        Lang::En => m.en,
    };
    let mut out = String::with_capacity(tpl.len());
    let mut next = 0usize;
    let mut it = tpl.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let arg = match it.peek() {
            Some('s' | 'd') => {
                let a = args.get(next).copied();
                next += 1;
                a
            }
            Some(d @ '1'..='9') => args.get(d.to_digit(10).unwrap_or(1) as usize - 1).copied(),
            _ => {
                out.push('%');
                continue;
            }
        };
        match arg {
            Some(a) => {
                it.next();
                out.push_str(a);
            }
            None => out.push('%'),
        }
    }
    out
}
