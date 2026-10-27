mod managed;
pub use managed::{ManagedPathError, ManagedResolveError, ManagedTarget, resolve_managed};

use std::fmt;

use mdconf::{Child, ChildTag, Configuration, MdKind, MdObject, Recalculation, SubKind, SubObject};

use super::names::{
    child_tag_from_name, child_tag_name, fold_path_name, kind_from_name, kind_name,
    sub_kind_from_name, sub_kind_name,
};
use super::out::Lang;

pub enum Target<'a> {
    Configuration(&'a Configuration),
    Object(&'a MdObject),
    Child {
        owner: &'a MdObject,
        chain: Vec<&'a Child>,
    },
    Sub {
        owner: &'a MdObject,
        sub: &'a SubObject,
    },
    Recalc {
        owner: &'a MdObject,
        recalc: &'a Recalculation,
    },
}

#[derive(Debug)]
pub struct PathError {
    pub path: String,
    pub suggestions: Vec<String>,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no such object: {}", self.path)?;
        if !self.suggestions.is_empty() {
            write!(f, " (did you mean: {})", self.suggestions.join(", "))?;
        }
        Ok(())
    }
}

impl std::error::Error for PathError {}

enum Seg {
    Recalc,
    Child(ChildTag),
    Sub(SubKind),
    Subsystem,
}

fn seg_of(s: &str) -> Option<Seg> {
    if is_recalc_name(s) {
        Some(Seg::Recalc)
    } else if let Some(t) = child_tag_from_name(s) {
        Some(Seg::Child(t))
    } else if let Some(k) = sub_kind_from_name(s) {
        Some(Seg::Sub(k))
    } else if kind_from_name(s) == Some(MdKind::Subsystem) {
        Some(Seg::Subsystem)
    } else {
        None
    }
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev.get(j).copied().unwrap_or(0) + usize::from(ca != cb);
            let del = prev.get(j + 1).copied().unwrap_or(0) + 1;
            let ins = cur.last().copied().unwrap_or(0) + 1;
            cur.push(sub.min(del).min(ins));
        }
        prev = cur;
    }
    prev.last().copied().unwrap_or(0)
}

fn nearest(bad: &str, cands: &[String]) -> Vec<String> {
    if bad.trim().is_empty() {
        return cands.iter().take(5).cloned().collect();
    }
    let b: Vec<char> = fold_path_name(bad).chars().collect();
    let mut scored: Vec<(usize, &String)> = cands
        .iter()
        .map(|c| {
            let cc: Vec<char> = fold_path_name(c).chars().collect();
            (levenshtein(&b, &cc), c)
        })
        .filter(|(d, c)| *d <= (b.len().max(c.chars().count()) / 2).max(2))
        .collect();
    scored.sort_by(|x, y| x.0.cmp(&y.0).then_with(|| x.1.cmp(y.1)));
    scored.into_iter().take(5).map(|(_, c)| c.clone()).collect()
}

fn lang_of(s: &str) -> Lang {
    if s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)) {
        Lang::Ru
    } else {
        Lang::En
    }
}

struct Walk<'p> {
    segs: Vec<&'p str>,
}

impl Walk<'_> {
    fn lang_at(&self, at: usize) -> Lang {
        self.segs.get(at).copied().map_or(Lang::En, lang_of)
    }

    fn fail(&self, at: usize, cands: Vec<String>) -> PathError {
        let typed = |n: usize| self.segs.iter().take(n).copied().collect::<Vec<_>>();
        let bad = self.segs.get(at).copied().unwrap_or_default();
        let prefix = typed(at);
        let rest: Vec<&str> = self.segs.iter().skip(at + 1).copied().collect();
        let join = |last: &str| {
            let mut v = prefix.clone();
            v.push(last);
            v.extend(rest.iter().copied());
            v.join(".")
        };
        PathError {
            path: self.segs.join("."),
            suggestions: nearest(bad, &cands).into_iter().map(|c| join(&c)).collect(),
        }
    }
}

fn is_recalc_name(s: &str) -> bool {
    let f = fold_path_name(s);
    f == fold_path_name(RECALC_EN) || f == fold_path_name(RECALC_RU)
}

pub const RECALC_EN: &str = "Recalculation";
pub const RECALC_RU: &str = "Перерасчет";

fn eq_name(a: &str, b: &str) -> bool {
    fold_path_name(a) == fold_path_name(b)
}

pub fn resolve<'a>(cfg: &'a Configuration, path: &str) -> Result<Target<'a>, PathError> {
    let segs: Vec<&str> = path.split('.').map(str::trim).collect();
    let w = Walk { segs };
    let seg = |i: usize| w.segs.get(i).copied();

    if let [only] = w.segs.as_slice()
        && kind_from_name(only) == Some(MdKind::Configuration)
    {
        return Ok(Target::Configuration(cfg));
    }
    let mut kind_cands: Vec<String> = MdKind::all()
        .iter()
        .filter(|&&k| cfg.objects.iter().any(|o| o.kind == k))
        .map(|&k| kind_name(k, w.lang_at(0)))
        .collect();
    kind_cands.push(kind_name(MdKind::Configuration, w.lang_at(0)));
    let kind = seg(0)
        .and_then(kind_from_name)
        .ok_or_else(|| w.fail(0, kind_cands))?;
    let object_names = |objs: &[MdObject], k: MdKind| -> Vec<String> {
        objs.iter()
            .filter(|o| o.kind == k)
            .map(|o| o.name.clone())
            .collect()
    };
    let name = seg(1).ok_or_else(|| w.fail(1, object_names(&cfg.objects, kind)))?;
    let mut owner: &MdObject = cfg
        .objects
        .iter()
        .find(|o| o.kind == kind && eq_name(&o.name, name))
        .ok_or_else(|| w.fail(1, object_names(&cfg.objects, kind)))?;

    let mut chain: Vec<&Child> = Vec::new();
    let mut i = 2;
    while let Some(tag) = seg(i) {
        let current: &[Child] = chain
            .last()
            .map_or(owner.children.as_slice(), |c| c.children.as_slice());
        let name_at = i + 1;
        match seg_of(tag) {
            Some(Seg::Child(t)) => {
                let names = || -> Vec<String> {
                    current
                        .iter()
                        .filter(|c| c.tag == t)
                        .map(|c| c.name.clone())
                        .collect()
                };
                let n = seg(name_at).ok_or_else(|| w.fail(name_at, names()))?;
                let c = current
                    .iter()
                    .find(|c| c.tag == t && eq_name(&c.name, n))
                    .ok_or_else(|| w.fail(name_at, names()))?;
                chain.push(c);
            }
            Some(Seg::Recalc) if chain.is_empty() && !owner.recalculations.is_empty() => {
                let names = || {
                    owner
                        .recalculations
                        .iter()
                        .map(|r| r.name.clone())
                        .collect::<Vec<_>>()
                };
                let n = seg(name_at).ok_or_else(|| w.fail(name_at, names()))?;
                let recalc = owner
                    .recalculations
                    .iter()
                    .find(|r| eq_name(&r.name, n))
                    .ok_or_else(|| w.fail(name_at, names()))?;
                if seg(name_at + 1).is_some() {
                    return Err(w.fail(name_at + 1, Vec::new()));
                }
                return Ok(Target::Recalc { owner, recalc });
            }
            Some(Seg::Sub(k)) if chain.is_empty() => {
                let subs = match k {
                    SubKind::Form => &owner.forms,
                    SubKind::Template => &owner.templates,
                };
                let names = || subs.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
                let n = seg(name_at).ok_or_else(|| w.fail(name_at, names()))?;
                let sub = subs
                    .iter()
                    .find(|s| eq_name(&s.name, n))
                    .ok_or_else(|| w.fail(name_at, names()))?;
                if seg(name_at + 1).is_some() {
                    return Err(w.fail(name_at + 1, Vec::new()));
                }
                return Ok(Target::Sub { owner, sub });
            }
            Some(Seg::Subsystem) if chain.is_empty() && owner.kind == MdKind::Subsystem => {
                let names = || object_names(&owner.subsystems, MdKind::Subsystem);
                let n = seg(name_at).ok_or_else(|| w.fail(name_at, names()))?;
                owner = owner
                    .subsystems
                    .iter()
                    .find(|s| eq_name(&s.name, n))
                    .ok_or_else(|| w.fail(name_at, names()))?;
            }
            _ => {
                let mut cands: Vec<String> = current
                    .iter()
                    .map(|c| child_tag_name(c.tag, w.lang_at(i)))
                    .collect();
                if chain.is_empty() {
                    if !owner.forms.is_empty() {
                        cands.push(sub_kind_name(SubKind::Form, w.lang_at(i)));
                    }
                    if !owner.templates.is_empty() {
                        cands.push(sub_kind_name(SubKind::Template, w.lang_at(i)));
                    }
                    if !owner.recalculations.is_empty() {
                        let ru = w.lang_at(i) == Lang::Ru;
                        cands.push((if ru { RECALC_RU } else { RECALC_EN }).to_string());
                    }
                }
                cands.sort();
                cands.dedup();
                return Err(w.fail(i, cands));
            }
        }
        i = name_at + 1;
    }
    if chain.is_empty() {
        Ok(Target::Object(owner))
    } else {
        Ok(Target::Child { owner, chain })
    }
}
