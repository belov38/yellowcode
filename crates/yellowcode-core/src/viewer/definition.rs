use bsl_syntax::{SourceText, Token, TokenKind, tokenize};
use mdconf::{Configuration, MdKind, ModuleKind};
use serde::Serialize;

use super::module::{line_ranges, module_of};
use super::names::{fold_path_name, module_name};
use super::out::Lang;
use super::outline::{OutlineItem, outline};
use super::path;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: String,
    pub module: String,
    pub line: u32,
    pub name: String,
}

const SELF_NAMES: [&str; 4] = ["thisobject", "этотобъект", "thisform", "этаформа"];

fn same(a: &str, b: &str) -> bool {
    fold_path_name(a) == fold_path_name(b)
}

fn text_of(m: &mdconf::Module) -> String {
    m.text().into_owned()
}

fn method<'a>(
    items: &'a [OutlineItem],
    name: &str,
    exported_only: bool,
) -> Option<&'a OutlineItem> {
    items
        .iter()
        .find(|i| same(&i.name, name) && (i.export || !exported_only))
}

pub fn definition(
    cfg: &Configuration,
    path: &str,
    module: &str,
    line: u32,
    col: u32,
) -> Option<Location> {
    at(cfg, path, module, line, col).or_else(|| match col.checked_sub(1)? {
        0 => None,
        c => at(cfg, path, module, line, c),
    })
}

fn at(cfg: &Configuration, path: &str, module: &str, line: u32, col: u32) -> Option<Location> {
    let target = path::resolve(cfg, path).ok()?;
    let m = module_of(cfg, &target, Some(module)).ok()?;
    let text = text_of(m);
    let src = SourceText::from_str("", &text);
    let units = src.units();
    let (ls, le) = *line_ranges(units).get(usize::try_from(line.checked_sub(1)?).ok()?)?;
    let off = ls.checked_add(usize::try_from(col.checked_sub(1)?).ok()?)?;
    if off >= le {
        return None;
    }
    let off = u32::try_from(off).ok()?;
    let lexed = tokenize(&src);
    let toks = &lexed.tokens;
    let i = toks
        .iter()
        .position(|t| t.kind == TokenKind::Ident && t.span.start <= off && off < t.span.end)?;
    let name = src.text(toks.get(i)?.span);
    let qualifier = match i.checked_sub(1).and_then(|j| toks.get(j)) {
        Some(Token {
            kind: TokenKind::Dot,
            ..
        }) => {
            let q = i.checked_sub(2).and_then(|j| toks.get(j))?;
            if q.kind != TokenKind::Ident {
                return None;
            }
            let before_q = i.checked_sub(3).and_then(|j| toks.get(j));
            if matches!(before_q, Some(b) if b.kind == TokenKind::Dot) {
                return None;
            }
            Some(src.text(q.span))
        }
        _ => None,
    };
    let own = |name: &str| -> Option<Location> {
        let (items, _) = outline(&text);
        let it = method(&items, name, false)?;
        Some(Location {
            path: path.to_string(),
            module: module_name(m.kind, Lang::En),
            line: it.line,
            name: it.name.clone(),
        })
    };
    match qualifier {
        None => own(&name),
        Some(q) if SELF_NAMES.iter().any(|s| same(s, &q)) => own(&name),
        Some(q) => {
            let cm = cfg
                .objects
                .iter()
                .find(|o| o.kind == MdKind::CommonModule && same(&o.name, &q))?;
            let cmod = cm.modules.iter().find(|m| m.kind == ModuleKind::Module)?;
            let (items, _) = outline(&text_of(cmod));
            let it = method(&items, &name, true)?;
            Some(Location {
                path: format!("CommonModule.{}", cm.name),
                module: module_name(ModuleKind::Module, Lang::En),
                line: it.line,
                name: it.name.clone(),
            })
        }
    }
}
