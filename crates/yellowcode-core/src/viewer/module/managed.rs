use crate::viewer::{
    Lang,
    names::{ManagedNames, eq_path, module_label},
    path::{self, ManagedPathError, ManagedResolveError, ManagedTarget},
};
use mdconf::{
    ManagedEntrySource, ModuleKind, SubKind,
    startup::{ManagedModuleKey, ManagedViewerOpen, ModuleLocation},
};
use memory_budget::{BudgetError, BudgetedBox, BudgetedText, BudgetedVec, Category, MemoryBudget};
use std::fmt;

#[derive(Debug)]
pub enum ManagedModuleError {
    Path(BudgetedBox<ManagedPathError>),
    Memory(BudgetError),
    Selection(BudgetedText),
    InvalidInventory,
}
impl fmt::Display for ManagedModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(e) => e.fmt(f),
            Self::Memory(e) => e.fmt(f),
            Self::Selection(text) => f.write_str(text.as_str()),
            Self::InvalidInventory => f.write_str("validated module inventory is inconsistent"),
        }
    }
}
impl std::error::Error for ManagedModuleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Path(e) => Some(&**e),
            Self::Memory(e) => Some(e),
            Self::Selection(_) | Self::InvalidInventory => None,
        }
    }
}
impl From<BudgetError> for ManagedModuleError {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
impl From<ManagedResolveError> for ManagedModuleError {
    fn from(e: ManagedResolveError) -> Self {
        match e {
            ManagedResolveError::Memory(e) => Self::Memory(e),
            ManagedResolveError::Path(e) => Self::Path(e),
        }
    }
}
type Result<T> = std::result::Result<T, ManagedModuleError>;

pub fn modules_managed<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    path: &str,
) -> Result<BudgetedVec<ManagedModuleKey>> {
    let target = path::resolve_managed(open, names, path)?;
    let location = match &target {
        ManagedTarget::Configuration(_) => Some((None, ModuleLocation::Configuration)),
        ManagedTarget::Object { ordinal, .. } => Some((Some(*ordinal), ModuleLocation::Object)),
        ManagedTarget::Child {
            ordinal,
            owner,
            chain,
        } => {
            if let [child] = chain.as_slice() {
                let at = owner
                    .children
                    .iter()
                    .position(|row| std::ptr::eq(row, *child))
                    .ok_or(ManagedModuleError::InvalidInventory)?;
                Some((Some(*ordinal), ModuleLocation::Command(at)))
            } else {
                None
            }
        }
        ManagedTarget::Sub {
            ordinal,
            owner,
            sub,
        } => match sub.kind {
            SubKind::Form => {
                let at = owner
                    .forms
                    .iter()
                    .position(|row| std::ptr::eq(row, *sub))
                    .ok_or(ManagedModuleError::InvalidInventory)?;
                Some((Some(*ordinal), ModuleLocation::Form(at)))
            }
            SubKind::Template => None,
        },
        ManagedTarget::Recalc {
            ordinal,
            owner,
            recalc,
        } => {
            let at = owner
                .recalculations
                .iter()
                .position(|row| std::ptr::eq(row, *recalc))
                .ok_or(ManagedModuleError::InvalidInventory)?;
            Some((Some(*ordinal), ModuleLocation::Recalculation(at)))
        }
    };
    let matches = |object, at| location == Some((object, at));
    let count = open
        .modules()
        .iter()
        .filter(|row| matches(row.object, row.location))
        .count();
    let mut out = BudgetedVec::try_with_capacity(
        open.context().index().memory_budget(),
        Category::Metadata,
        count,
    )?;
    for (ordinal, row) in open.modules().iter().enumerate() {
        if matches(row.object, row.location) {
            out.try_push(
                open.module_key(ordinal)
                    .ok_or(ManagedModuleError::InvalidInventory)?,
            )?;
        }
    }
    Ok(out)
}
fn kind_matches(kind: ModuleKind, which: &str) -> bool {
    (kind == ModuleKind::FormModule && (eq_path(which, "form") || eq_path(which, "форма")))
        || [Lang::En, Lang::Ru]
            .into_iter()
            .any(|lang| eq_path(which, module_label(kind, lang)))
}
fn list<S: ManagedEntrySource>(
    out: &mut BudgetedText,
    open: &ManagedViewerOpen<S>,
    keys: &[ManagedModuleKey],
) -> Result<()> {
    for (at, key) in keys.iter().enumerate() {
        if at != 0 {
            out.try_push_str(", ")?;
        }
        let row = open
            .modules()
            .get(key.ordinal())
            .ok_or(ManagedModuleError::InvalidInventory)?;
        out.try_push_str(module_label(row.slot.kind, Lang::En))?;
    }
    Ok(())
}
fn selection_text(budget: &MemoryBudget) -> std::result::Result<BudgetedText, BudgetError> {
    BudgetedText::new(budget, Category::Metadata, u64::MAX)
}
pub fn module_of_managed<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    path: &str,
    which: Option<&str>,
) -> Result<ManagedModuleKey> {
    let keys = modules_managed(open, names, path)?;
    if let Some(which) = which {
        for key in keys.iter() {
            let row = open
                .modules()
                .get(key.ordinal())
                .ok_or(ManagedModuleError::InvalidInventory)?;
            if kind_matches(row.slot.kind, which) {
                return Ok(*key);
            }
        }
    } else if let [key] = keys.as_slice() {
        return Ok(*key);
    }
    let mut error = selection_text(open.context().index().memory_budget())?;
    if let Some(which) = which {
        error.try_push_str("no module ")?;
        error.try_push_str(which)?;
        if keys.is_empty() {
            error.try_push_str(": the object has no modules")?;
        } else {
            error.try_push_str(" (available: ")?;
            list(&mut error, open, &keys)?;
            error.try_push_str(")")?;
        }
    } else if keys.is_empty() {
        error.try_push_str("the object has no modules")?;
    } else {
        error.try_push_str("several modules, choose with --module: ")?;
        list(&mut error, open, &keys)?;
    }
    Err(ManagedModuleError::Selection(error))
}
