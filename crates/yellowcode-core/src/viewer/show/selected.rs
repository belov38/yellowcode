use super::{ManagedProperties, properties_managed};
use crate::viewer::{
    Lang,
    body::ViewerBody,
    names::{ManagedNames, child_tag_en, sub_kind_en},
    path::{self, ManagedPathError, ManagedResolveError, ManagedTarget},
};
use mdconf::{
    ManagedEntrySource, SubKind,
    startup::{ManagedSelectionError, ManagedViewerOpen},
};
use memory_budget::{BudgetError, BudgetedBox, BudgetedText, Category};

#[derive(Debug)]
pub enum ManagedShowError<E> {
    Path(BudgetedBox<ManagedPathError>),
    Memory(BudgetError),
    Read(ManagedSelectionError<E>),
}
impl<E: std::fmt::Display> std::fmt::Display for ManagedShowError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(e) => e.fmt(f),
            Self::Memory(e) => e.fmt(f),
            Self::Read(e) => e.fmt(f),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for ManagedShowError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Path(e) => Some(&**e),
            Self::Memory(e) => Some(e),
            Self::Read(e) => Some(e),
        }
    }
}
impl<E> From<BudgetError> for ManagedShowError<E> {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
impl<E> From<ManagedSelectionError<E>> for ManagedShowError<E> {
    fn from(e: ManagedSelectionError<E>) -> Self {
        Self::Read(e)
    }
}
impl<E> From<ManagedResolveError> for ManagedShowError<E> {
    fn from(error: ManagedResolveError) -> Self {
        match error {
            ManagedResolveError::Memory(e) => Self::Memory(e),
            ManagedResolveError::Path(e) => Self::Path(e),
        }
    }
}
type Result<T, E> = std::result::Result<T, ManagedShowError<E>>;
fn missing<E>() -> ManagedShowError<E> {
    ManagedShowError::Read(ManagedSelectionError::MissingValidated)
}

pub fn show_managed<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    path: &str,
    lang: Lang,
    wire_limit: u64,
) -> Result<ViewerBody, S::Error> {
    with_properties(open, names, path, lang, |_, props| {
        Ok(props.to_body(wire_limit)?)
    })
}
pub(crate) fn with_properties<S: ManagedEntrySource, R>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    path: &str,
    lang: Lang,
    body: impl FnOnce(&ManagedTarget<'_>, ManagedProperties<'_>) -> Result<R, S::Error>,
) -> Result<R, S::Error> {
    let target = path::resolve_managed(open, names, path)?;
    if matches!(target, ManagedTarget::Configuration(_)) {
        let selected = open
            .context()
            .read_configuration()
            .map_err(ManagedSelectionError::Decode)?;
        return body(
            &target,
            properties_managed(
                &selected.props,
                |p| p.as_slice(),
                names,
                "Configuration",
                "Configuration",
                lang,
            )?,
        );
    }
    let ordinal = match &target {
        ManagedTarget::Object { ordinal, .. }
        | ManagedTarget::Child { ordinal, .. }
        | ManagedTarget::Sub { ordinal, .. }
        | ManagedTarget::Recalc { ordinal, .. } => *ordinal,
        ManagedTarget::Configuration(_) => return Err(missing()),
    };
    let selected = open.read_selected_object(open.object_key(ordinal).ok_or_else(missing)?)?;
    let object = &selected.object;
    let render = |props, kind, context: &str| -> Result<R, S::Error> {
        body(
            &target,
            properties_managed(object, |_| props, names, kind, context, lang)?,
        )
    };
    match &target {
        ManagedTarget::Object { object: fact, .. } => {
            if (fact.id, fact.kind, fact.name.as_str())
                != (object.id, object.kind, object.name.as_str())
            {
                return Err(missing());
            }
            render(
                object.props.as_slice(),
                object.kind.as_str(),
                object.kind.as_str(),
            )
        }
        ManagedTarget::Child { owner, chain, .. } => {
            let budget = open.context().index().memory_budget();
            let mut context = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
            context.try_push_str(owner.kind.as_str())?;
            let mut facts = owner.children.as_slice();
            let mut actual = object.children.as_slice();
            let mut last = None;
            for child in chain.iter() {
                let at = facts
                    .iter()
                    .position(|row| std::ptr::eq(row, *child))
                    .ok_or_else(missing)?;
                let native = actual.get(at).ok_or_else(missing)?;
                if (child.id, child.tag, child.name.as_str())
                    != (native.id, native.tag, native.name.as_str())
                {
                    return Err(missing());
                }
                context.try_push_str(".")?;
                context.try_push_str(child_tag_en(child.tag))?;
                facts = child.children.as_slice();
                actual = native.children.as_slice();
                last = Some(native);
            }
            let last = last.ok_or_else(missing)?;
            render(
                last.props.as_slice(),
                child_tag_en(last.tag),
                context.as_str(),
            )
        }
        ManagedTarget::Sub { owner, sub, .. } => {
            let (facts, actual) = match sub.kind {
                SubKind::Form => (owner.forms.as_slice(), object.forms.as_slice()),
                SubKind::Template => (owner.templates.as_slice(), object.templates.as_slice()),
            };
            let at = facts
                .iter()
                .position(|row| std::ptr::eq(row, *sub))
                .ok_or_else(missing)?;
            let native = actual.get(at).ok_or_else(missing)?;
            if (sub.id, sub.kind, sub.name.as_str())
                != (native.id, native.kind, native.name.as_str())
            {
                return Err(missing());
            }
            let kind = sub_kind_en(sub.kind);
            let mut context = BudgetedText::new(
                open.context().index().memory_budget(),
                Category::Metadata,
                u64::MAX,
            )?;
            context.try_push_str(owner.kind.as_str())?;
            context.try_push_str(".")?;
            context.try_push_str(kind)?;
            render(native.props.as_slice(), kind, context.as_str())
        }
        ManagedTarget::Recalc { owner, recalc, .. } => {
            let at = owner
                .recalculations
                .iter()
                .position(|row| std::ptr::eq(row, *recalc))
                .ok_or_else(missing)?;
            let native = object.recalculations.get(at).ok_or_else(missing)?;
            if (recalc.id, recalc.name.as_str()) != (native.id, native.name.as_str()) {
                return Err(missing());
            }
            render(native.props.as_slice(), "Recalculation", "Recalculation")
        }
        ManagedTarget::Configuration(_) => Err(missing()),
    }
}
