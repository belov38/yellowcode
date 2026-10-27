mod managed;
mod re;
pub use managed::{FindQuery, ManagedFindError};
pub use re::{ALLOWANCE as REGEX_ALLOWANCE, FindRegex, MAX_PATTERN_BYTES, RegexError};

use mdconf::{Child, Configuration, MdObject, Module, SubObject};
use serde::Serialize;

use super::module::split_lines;
use super::names::{child_tag_en, module_name, sub_kind_en};
use super::out::Lang;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub module: Option<String>,
    pub line: Option<u32>,
    pub text: String,
}

fn fold(s: &str) -> String {
    bsl_prim::fold_name(s).replace('ё', "е")
}

enum Matcher {
    Sub(String),
    Re(Box<FindRegex>),
}

impl Matcher {
    fn is_match(&mut self, s: &str) -> bool {
        match self {
            Matcher::Sub(p) => fold(s).contains(p.as_str()),
            Matcher::Re(r) => r.is_match(s),
        }
    }
}

struct Finder<'a> {
    m: Matcher,
    names_only: bool,
    hits: Vec<Hit>,
    limit: usize,
    total: usize,
    cfg: &'a Configuration,
}

impl Finder<'_> {
    fn add(&mut self, hit: Hit) {
        self.total += 1;
        if self.hits.len() < self.limit {
            self.hits.push(hit);
        }
    }

    fn name(&mut self, path: &str, name: &str, syn: &mdconf::LocalString) {
        let syns: Vec<&str> = syn
            .0
            .iter()
            .map(|(_, t)| t.as_str())
            .filter(|t| !t.is_empty())
            .collect();
        if self.m.is_match(name) || syns.iter().any(|s| self.m.is_match(s)) {
            let text = match syns.first() {
                Some(s) => format!("{name} ({s})"),
                None => name.to_string(),
            };
            self.add(Hit {
                path: path.to_string(),
                module: None,
                line: None,
                text,
            });
        }
    }

    fn module(&mut self, path: &str, m: &Module) {
        if self.names_only {
            return;
        }
        let text = m.text();
        for (i, l) in split_lines(&text).into_iter().enumerate() {
            if self.m.is_match(l) {
                self.add(Hit {
                    path: path.to_string(),
                    module: Some(module_name(m.kind, Lang::En)),
                    line: u32::try_from(i + 1).ok(),
                    text: l.trim().to_string(),
                });
            }
        }
    }

    fn child(&mut self, owner: &str, c: &Child) {
        let path = format!("{owner}.{}.{}", child_tag_en(c.tag), c.name);
        self.name(&path, &c.name, &c.synonym);
        if let Some(m) = &c.module {
            self.module(&path, m);
        }
        for c2 in &c.children {
            self.child(&path, c2);
        }
    }

    fn sub(&mut self, owner: &str, s: &SubObject) {
        let path = format!("{owner}.{}.{}", sub_kind_en(s.kind), s.name);
        self.name(&path, &s.name, &mdconf::LocalString::default());
        if let Some(m) = &s.module {
            self.module(&path, m);
        }
    }

    fn object(&mut self, path: &str, o: &MdObject) {
        self.name(path, &o.name, &o.synonym);
        for m in &o.modules {
            self.module(path, m);
        }
        for c in &o.children {
            self.child(path, c);
        }
        for s in o.forms.iter().chain(&o.templates) {
            self.sub(path, s);
        }
        for r in &o.recalculations {
            let p = format!("{path}.Recalculation.{}", r.name);
            self.name(&p, &r.name, &r.synonym);
            if let Some(m) = &r.module {
                self.module(&p, m);
            }
        }
        for s in &o.subsystems {
            self.object(&format!("{path}.Subsystem.{}", s.name), s);
        }
    }
}

pub fn find(
    cfg: &Configuration,
    pattern: &str,
    regex: bool,
    names_only: bool,
) -> Result<Vec<Hit>, crate::Error> {
    Ok(find_limited(cfg, pattern, regex, names_only, usize::MAX)?.0)
}

pub fn find_limited(
    cfg: &Configuration,
    pattern: &str,
    regex: bool,
    names_only: bool,
    limit: usize,
) -> Result<(Vec<Hit>, usize), crate::Error> {
    let m = if regex {
        Matcher::Re(Box::new(FindRegex::compile(pattern)?))
    } else {
        Matcher::Sub(fold(pattern))
    };
    let mut f = Finder {
        m,
        names_only,
        hits: Vec::new(),
        limit,
        total: 0,
        cfg,
    };
    for m in &f.cfg.modules {
        f.module("Configuration", m);
    }
    let cfg = f.cfg;
    for o in &cfg.objects {
        let path = format!("{}.{}", o.kind.as_str(), o.name);
        f.object(&path, o);
    }
    Ok((f.hits, f.total))
}

pub fn render_text(hits: &[Hit], out: &mut dyn std::io::Write) -> crate::Result<()> {
    for h in hits {
        match (&h.module, h.line) {
            (Some(m), Some(l)) => writeln!(out, "{}:{m}:{l}: {}", h.path, h.text)?,
            _ => writeln!(out, "{}: {}", h.path, h.text)?,
        }
    }
    Ok(())
}
