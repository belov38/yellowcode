use mdconf::{Configuration, Module, ModuleKind};

use super::names::{fold_path_name, module_name};
use super::out::Lang;
use super::path::Target;

pub fn modules_of<'a>(t: &Target<'a>) -> Vec<&'a Module> {
    match t {
        Target::Configuration(c) => {
            let c: &'a Configuration = c;
            c.modules.iter().collect()
        }
        Target::Recalc { recalc, .. } => {
            let r: &'a mdconf::Recalculation = recalc;
            r.module.iter().collect()
        }
        Target::Object(o) => {
            let o: &'a mdconf::MdObject = o;
            o.modules.iter().collect()
        }
        Target::Child { chain, .. } => chain
            .last()
            .and_then(|c| {
                let c: &'a mdconf::Child = c;
                c.module.as_ref()
            })
            .into_iter()
            .collect(),
        Target::Sub { sub, .. } => {
            let s: &'a mdconf::SubObject = sub;
            s.module.iter().collect()
        }
    }
}

fn kind_matches(kind: ModuleKind, which: &str) -> bool {
    let w = fold_path_name(which);
    if kind == ModuleKind::FormModule && (w == "form" || w == "форма") {
        return true;
    }
    [Lang::En, Lang::Ru]
        .into_iter()
        .any(|l| fold_path_name(&module_name(kind, l)) == w)
}

pub fn module_of<'a>(
    _cfg: &'a Configuration,
    t: &Target<'a>,
    which: Option<&str>,
) -> Result<&'a Module, crate::Error> {
    let mods = modules_of(t);
    let list = || {
        mods.iter()
            .map(|m| module_name(m.kind, Lang::En))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match which {
        Some(w) => mods
            .iter()
            .copied()
            .find(|m| kind_matches(m.kind, w))
            .ok_or_else(|| {
                if mods.is_empty() {
                    format!("no module {w}: the object has no modules").into()
                } else {
                    format!("no module {w} (available: {})", list()).into()
                }
            }),
        None => match mods.as_slice() {
            [m] => Ok(m),
            [] => Err("the object has no modules".into()),
            _ => Err(format!("several modules, choose with --module: {}", list()).into()),
        },
    }
}

pub fn line_ranges(units: &[u16]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut i, mut start) = (0usize, 0usize);
    while let Some(&c) = units.get(i) {
        if c == 0x0A {
            out.push((start, i));
            i += 1;
            start = i;
        } else if c == 0x0D {
            out.push((start, i));
            i += 1;
            if units.get(i) == Some(&0x0A) {
                i += 1;
            }
            start = i;
        } else {
            i += 1;
        }
    }
    if start < units.len() {
        out.push((start, units.len()));
    }
    out
}

pub fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c == '\n' || c == '\r' {
            out.push(text.get(start..i).unwrap_or_default());
            if c == '\r'
                && let Some(&(_, '\n')) = it.peek()
            {
                it.next();
            }
            start = it.peek().map_or(text.len(), |&(j, _)| j);
        }
    }
    if start < text.len() {
        out.push(text.get(start..).unwrap_or_default());
    }
    out
}
