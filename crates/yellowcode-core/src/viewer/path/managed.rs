use super::{RECALC_EN, RECALC_RU, Seg, lang_of};
use crate::viewer::{
    Lang,
    names::{ManagedNames, eq_path, folded_path},
};
use mdconf::{
    ManagedEntrySource, MdKind, SubKind,
    startup::{
        ChildFacts, ConfigurationIdentity, ManagedViewerOpen, ObjectFacts, RecalculationFacts,
        SubObjectFacts,
    },
};
use memory_budget::{
    BudgetError, BudgetedBox, BudgetedText, BudgetedVec, Category, LimitReason, MemoryBudget,
    MemoryLimit,
};
use std::fmt;

type Result<T> = std::result::Result<T, ManagedResolveError>;
pub enum ManagedTarget<'a> {
    Configuration(&'a ConfigurationIdentity),
    Object {
        ordinal: usize,
        object: &'a ObjectFacts,
    },
    Child {
        ordinal: usize,
        owner: &'a ObjectFacts,
        chain: BudgetedVec<&'a ChildFacts>,
    },
    Sub {
        ordinal: usize,
        owner: &'a ObjectFacts,
        sub: &'a SubObjectFacts,
    },
    Recalc {
        ordinal: usize,
        owner: &'a ObjectFacts,
        recalc: &'a RecalculationFacts,
    },
}
#[derive(Debug)]
pub struct ManagedPathError {
    pub path: BudgetedText,
    pub suggestions: BudgetedVec<BudgetedText>,
}
impl fmt::Display for ManagedPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no such object: {}", self.path.as_str())?;
        if !self.suggestions.is_empty() {
            f.write_str(" (did you mean: ")?;
            for (i, text) in self.suggestions.iter().enumerate() {
                if i != 0 {
                    f.write_str(", ")?;
                }
                f.write_str(text.as_str())?;
            }
            f.write_str(")")?;
        }
        Ok(())
    }
}
impl std::error::Error for ManagedPathError {}
#[derive(Debug)]
pub enum ManagedResolveError {
    Memory(BudgetError),
    Path(BudgetedBox<ManagedPathError>),
}
impl From<BudgetError> for ManagedResolveError {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
impl fmt::Display for ManagedResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(f),
            Self::Path(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ManagedResolveError {}

fn join<'a>(
    budget: &MemoryBudget,
    parts: impl Iterator<Item = &'a str> + Clone,
) -> std::result::Result<BudgetedText, BudgetError> {
    let mut capacity = 0usize;
    for (i, text) in parts.clone().enumerate() {
        capacity = capacity
            .checked_add(usize::from(i != 0))
            .and_then(|n| n.checked_add(text.len()))
            .ok_or(BudgetError::Overflow)?;
    }
    let mut out = BudgetedText::try_with_capacity(
        budget,
        Category::Metadata,
        u64::try_from(capacity).map_err(|_| BudgetError::Overflow)?,
        capacity,
    )?;
    for (i, text) in parts.enumerate() {
        if i != 0 {
            out.try_push_str(".")?;
        }
        out.try_push_str(text)?;
    }
    Ok(out)
}
struct Walk<'a> {
    segs: BudgetedVec<&'a str>,
    budget: &'a MemoryBudget,
}
impl Walk<'_> {
    fn seg(&self, at: usize) -> Option<&str> {
        self.segs.get(at).copied()
    }
    fn lang(&self, at: usize) -> Lang {
        self.seg(at).map_or(Lang::En, lang_of)
    }
    fn fail<'a>(
        &self,
        at: usize,
        candidates: impl Iterator<Item = &'a str>,
    ) -> Result<ManagedResolveError> {
        let bad = self.seg(at).unwrap_or_default();
        let mut best: [Option<(usize, &str)>; 5] = [None; 5];
        let mut previous = BudgetedVec::new(self.budget, Category::Metadata)?;
        let mut current = BudgetedVec::new(self.budget, Category::Metadata)?;
        for candidate in candidates {
            let score = if bad.trim().is_empty() {
                0
            } else {
                distance(bad, candidate, &mut previous, &mut current)?
            };
            if !bad.trim().is_empty()
                && score > (folded_path(bad).count().max(candidate.chars().count()) / 2).max(2)
            {
                continue;
            }
            let mut row = (score, candidate);
            for slot in &mut best {
                let precedes = slot.is_none_or(|old| !bad.trim().is_empty() && row < old);
                if precedes {
                    match slot.replace(row) {
                        Some(old) => row = old,
                        None => break,
                    }
                }
            }
        }
        let mut suggestions = BudgetedVec::try_with_capacity(
            self.budget,
            Category::Metadata,
            best.iter().flatten().count(),
        )?;
        for (_, text) in best.into_iter().flatten() {
            suggestions.try_push(join(
                self.budget,
                self.segs
                    .iter()
                    .take(at)
                    .copied()
                    .chain(std::iter::once(text))
                    .chain(self.segs.iter().skip(at + 1).copied()),
            )?)?;
        }
        Ok(ManagedResolveError::Path(BudgetedBox::try_new(
            self.budget,
            Category::Metadata,
            ManagedPathError {
                path: join(self.budget, self.segs.iter().copied())?,
                suggestions,
            },
        )?))
    }
}
fn distance(
    a: &str,
    b: &str,
    previous: &mut BudgetedVec<usize>,
    current: &mut BudgetedVec<usize>,
) -> std::result::Result<usize, BudgetError> {
    let len = folded_path(b)
        .count()
        .checked_add(1)
        .ok_or(BudgetError::Overflow)?;
    previous.try_resize(len, 0)?;
    previous.truncate(len);
    current.try_resize(len, 0)?;
    current.truncate(len);
    for (i, slot) in previous.iter_mut().enumerate() {
        *slot = i;
    }
    for (i, ca) in folded_path(a).enumerate() {
        *current.first_mut().ok_or(BudgetError::Overflow)? =
            i.checked_add(1).ok_or(BudgetError::Overflow)?;
        for (j, cb) in folded_path(b).enumerate() {
            let sub = previous
                .get(j)
                .copied()
                .ok_or(BudgetError::Overflow)?
                .checked_add(usize::from(ca != cb))
                .ok_or(BudgetError::Overflow)?;
            let del = previous
                .get(j + 1)
                .copied()
                .ok_or(BudgetError::Overflow)?
                .checked_add(1)
                .ok_or(BudgetError::Overflow)?;
            let ins = current
                .get(j)
                .copied()
                .ok_or(BudgetError::Overflow)?
                .checked_add(1)
                .ok_or(BudgetError::Overflow)?;
            *current.get_mut(j + 1).ok_or(BudgetError::Overflow)? = sub.min(del).min(ins);
        }
        std::mem::swap(previous, current);
    }
    previous.last().copied().ok_or(BudgetError::Overflow)
}
fn seg_of(names: &ManagedNames<'_>, text: &str) -> Option<Seg> {
    if eq_path(text, RECALC_EN) || eq_path(text, RECALC_RU) {
        Some(Seg::Recalc)
    } else if let Some(tag) = names.child_tag_from_name(text) {
        Some(Seg::Child(tag))
    } else if let Some(kind) = names.sub_kind_from_name(text) {
        Some(Seg::Sub(kind))
    } else if names.kind_from_name(text) == Some(MdKind::Subsystem) {
        Some(Seg::Subsystem)
    } else {
        None
    }
}

pub fn resolve_managed<'a, S: ManagedEntrySource>(
    open: &'a ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    path: &str,
) -> Result<ManagedTarget<'a>> {
    let budget = open.context().index().memory_budget();
    if !budget.same_scope(names.memory_budget()) {
        return Err(BudgetError::Limit(MemoryLimit {
            category: Category::Metadata,
            requested: 0,
            reason: LimitReason::InvalidSize,
        })
        .into());
    }
    let mut segs =
        BudgetedVec::try_with_capacity(budget, Category::Metadata, path.split('.').count())?;
    for part in path.split('.') {
        segs.try_push(part.trim())?;
    }
    let w = Walk { segs, budget };
    if let [only] = w.segs.as_slice()
        && names.kind_from_name(only) == Some(MdKind::Configuration)
    {
        return Ok(ManagedTarget::Configuration(open.identity()));
    }
    let objects = open.objects();
    let kind = match w.seg(0).and_then(|s| names.kind_from_name(s)) {
        Some(kind) => kind,
        None => {
            return Err(w.fail(
                0,
                MdKind::all()
                    .iter()
                    .copied()
                    .filter(|kind| {
                        objects
                            .iter()
                            .any(|row| row.parent.is_none() && row.object.kind == *kind)
                    })
                    .map(|kind| names.kind_name(kind, w.lang(0)))
                    .chain(std::iter::once(
                        names.kind_name(MdKind::Configuration, w.lang(0)),
                    )),
            )?);
        }
    };
    let object_names = || {
        objects
            .iter()
            .filter(|row| row.parent.is_none() && row.object.kind == kind)
            .map(|row| row.object.name.as_str())
    };
    let selected = w.seg(1).and_then(|name| {
        objects.iter().enumerate().find(|(_, row)| {
            row.parent.is_none() && row.object.kind == kind && eq_path(&row.object.name, name)
        })
    });
    let (mut ordinal, row) = match selected {
        Some(row) => row,
        None => return Err(w.fail(1, object_names())?),
    };
    let mut owner: &ObjectFacts = &row.object;
    let mut chain: BudgetedVec<&ChildFacts> = BudgetedVec::new(budget, Category::Metadata)?;
    let mut i = 2;
    while let Some(tag) = w.seg(i) {
        let current = chain
            .last()
            .map_or(owner.children.as_slice(), |child| child.children.as_slice());
        let name_at = i + 1;
        match seg_of(names, tag) {
            Some(Seg::Child(t)) => {
                let child = w.seg(name_at).and_then(|name| {
                    current
                        .iter()
                        .find(|c| c.tag == t && eq_path(&c.name, name))
                });
                let Some(child) = child else {
                    return Err(w.fail(
                        name_at,
                        current
                            .iter()
                            .filter(|c| c.tag == t)
                            .map(|c| c.name.as_str()),
                    )?);
                };
                chain.try_push(child)?;
            }
            Some(Seg::Recalc) if chain.is_empty() && !owner.recalculations.is_empty() => {
                let found = w
                    .seg(name_at)
                    .and_then(|name| owner.recalculations.iter().find(|r| eq_path(&r.name, name)));
                let Some(recalc) = found else {
                    return Err(w.fail(
                        name_at,
                        owner.recalculations.iter().map(|r| r.name.as_str()),
                    )?);
                };
                if w.seg(name_at + 1).is_some() {
                    return Err(w.fail(name_at + 1, std::iter::empty())?);
                }
                return Ok(ManagedTarget::Recalc {
                    ordinal,
                    owner,
                    recalc,
                });
            }
            Some(Seg::Sub(k)) if chain.is_empty() => {
                let subs = match k {
                    SubKind::Form => &owner.forms,
                    SubKind::Template => &owner.templates,
                };
                let found = w
                    .seg(name_at)
                    .and_then(|name| subs.iter().find(|s| eq_path(&s.name, name)));
                let Some(sub) = found else {
                    return Err(w.fail(name_at, subs.iter().map(|s| s.name.as_str()))?);
                };
                if w.seg(name_at + 1).is_some() {
                    return Err(w.fail(name_at + 1, std::iter::empty())?);
                }
                return Ok(ManagedTarget::Sub {
                    ordinal,
                    owner,
                    sub,
                });
            }
            Some(Seg::Subsystem) if chain.is_empty() && owner.kind == MdKind::Subsystem => {
                let found = w.seg(name_at).and_then(|name| {
                    objects.iter().enumerate().find(|(_, row)| {
                        row.parent == Some(ordinal) && eq_path(&row.object.name, name)
                    })
                });
                let Some((next, row)) = found else {
                    return Err(w.fail(
                        name_at,
                        objects
                            .iter()
                            .filter(|row| {
                                row.parent == Some(ordinal) && row.object.kind == MdKind::Subsystem
                            })
                            .map(|row| row.object.name.as_str()),
                    )?);
                };
                ordinal = next;
                owner = &row.object;
            }
            _ => {
                let mut candidates = BudgetedVec::new(budget, Category::Metadata)?;
                for child in current {
                    candidates.try_push(names.child_tag_name(child.tag, w.lang(i)))?;
                }
                if chain.is_empty() {
                    if !owner.forms.is_empty() {
                        candidates.try_push(names.sub_kind_name(SubKind::Form, w.lang(i)))?;
                    }
                    if !owner.templates.is_empty() {
                        candidates.try_push(names.sub_kind_name(SubKind::Template, w.lang(i)))?;
                    }
                    if !owner.recalculations.is_empty() {
                        candidates.try_push(if w.lang(i) == Lang::Ru {
                            RECALC_RU
                        } else {
                            RECALC_EN
                        })?;
                    }
                }
                candidates.as_mut_slice().sort_unstable();
                return Err(w.fail(
                    i,
                    candidates
                        .iter()
                        .enumerate()
                        .filter(|(j, s)| *j == 0 || candidates.get(j - 1) != Some(s))
                        .map(|(_, s)| *s),
                )?);
            }
        }
        i = name_at + 1;
    }
    if chain.is_empty() {
        Ok(ManagedTarget::Object {
            ordinal,
            object: owner,
        })
    } else {
        Ok(ManagedTarget::Child {
            ordinal,
            owner,
            chain,
        })
    }
}
