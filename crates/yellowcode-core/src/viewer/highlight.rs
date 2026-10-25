use bsl_syntax::{SourceText, TokenKind, tokenize};

use super::module::line_ranges;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Plain,
    Keyword,
    Str,
    Number,
    Comment,
    Preproc,
    Annotation,
}

impl Class {
    fn code(self) -> &'static str {
        match self {
            Class::Plain => "",
            Class::Keyword => "\x1b[34m",
            Class::Str => "\x1b[32m",
            Class::Number => "\x1b[36m",
            Class::Comment => "\x1b[90m",
            Class::Preproc => "\x1b[35m",
            Class::Annotation => "\x1b[33m",
        }
    }
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Plain => "",
            Class::Keyword => "kw",
            Class::Str => "str",
            Class::Number => "num",
            Class::Comment => "com",
            Class::Preproc => "pre",
            Class::Annotation => "ann",
        }
    }
}

const RESET: &str = "\x1b[0m";

fn paint(classes: &mut [Class], start: usize, end: usize, c: Class) {
    if let Some(s) = classes.get_mut(start..end.min(classes.len())) {
        s.fill(c);
    }
}

fn classify(src: &SourceText) -> Vec<Class> {
    let units = src.units();
    let mut classes = vec![Class::Plain; units.len()];
    let lexed = tokenize(src);
    let toks = &lexed.tokens;
    let mut gap_start = 0usize;
    for (i, t) in toks.iter().enumerate() {
        let (s, e) = (t.span.start as usize, t.span.end as usize);
        comments_in(units, gap_start, s, &mut classes);
        gap_start = e.max(gap_start);
        let c = match t.kind {
            TokenKind::Kw(_) => Class::Keyword,
            TokenKind::Str(_) => Class::Str,
            TokenKind::Number | TokenKind::Date(_) => Class::Number,
            TokenKind::Directive => Class::Preproc,
            TokenKind::Amp => {
                match toks.get(i + 1) {
                    Some(n) if n.kind == TokenKind::Ident && n.span.start == t.span.end => {
                        paint(&mut classes, s, n.span.end as usize, Class::Annotation);
                    }
                    _ => {}
                }
                continue;
            }
            _ => continue,
        };
        paint(&mut classes, s, e, c);
    }
    classes
}

fn comments_in(units: &[u16], from: usize, to: usize, classes: &mut [Class]) {
    let to = to.min(units.len());
    let mut j = from;
    while j < to {
        if units.get(j) == Some(&0x2F) && units.get(j + 1) == Some(&0x2F) {
            let mut k = j;
            while k < to && !matches!(units.get(k), Some(0x0A | 0x0D)) {
                k += 1;
            }
            paint(classes, j, k, Class::Comment);
            j = k;
        } else {
            j += 1;
        }
    }
}

fn runs(units: &[u16], classes: &[Class], s: usize, e: usize) -> Vec<(Class, String)> {
    let mut out = Vec::new();
    let mut i = s;
    while i < e {
        let c = classes.get(i).copied().unwrap_or(Class::Plain);
        let mut j = i + 1;
        while j < e && classes.get(j).copied().unwrap_or(Class::Plain) == c {
            j += 1;
        }
        out.push((
            c,
            String::from_utf16_lossy(units.get(i..j).unwrap_or_default()),
        ));
        i = j;
    }
    out
}

pub fn highlight(text: &str, color: bool) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let src = SourceText::from_str("", text);
    let units = src.units();
    let classes = if color { classify(&src) } else { Vec::new() };
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let ranges = line_ranges(units);
    let width = ranges.len().to_string().len().max(4);
    for (n, (s, e)) in ranges.into_iter().enumerate() {
        if color {
            out.push_str(&format!("\x1b[90m{:>width$} │{RESET} ", n + 1));
        } else {
            out.push_str(&format!("{:>width$} │ ", n + 1));
        }
        for (c, piece) in runs(units, &classes, s, e) {
            if c == Class::Plain {
                out.push_str(&piece);
            } else {
                out.push_str(c.code());
                out.push_str(&piece);
                out.push_str(RESET);
            }
        }
        out.push('\n');
    }
    out
}

pub fn classified_lines(text: &str) -> Vec<Vec<(&'static str, String)>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let src = SourceText::from_str("", text);
    let units = src.units();
    let classes = classify(&src);
    line_ranges(units)
        .into_iter()
        .map(|(s, e)| {
            runs(units, &classes, s, e)
                .into_iter()
                .map(|(c, t)| (c.name(), t))
                .collect()
        })
        .collect()
}
