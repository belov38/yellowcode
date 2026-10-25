use super::info::CallKind;
use super::{Category, CategorySet};
use platform_catalog::messages::{self, Wrapper};

pub(crate) fn wrapper<'a>(
    kind: &'a CallKind,
    name: &'a str,
    own_key: Option<&str>,
) -> Option<(Wrapper, &'a str)> {
    match kind {
        CallKind::Vm => None,
        CallKind::Global { builtin: true } if own_key.is_some_and(is_badargvalue) => None,
        CallKind::Global { builtin: true } => Some((Wrapper::BuiltinArg, name)),
        CallKind::Global { builtin: false } | CallKind::Method => {
            Some((Wrapper::ContextCall, name))
        }
        CallKind::Ctor { type_name } => Some((Wrapper::CtorCall, type_name)),
        CallKind::SetAttr => Some((Wrapper::SetAttr, name)),
        CallKind::GetAttr => Some((Wrapper::GetAttr, name)),
    }
}

fn is_badargvalue(key: &str) -> bool {
    key.strip_prefix("bsl:").unwrap_or(key) == "IDS_RTE_BADARGVALUE"
}

pub(crate) fn wrapper_key(w: Wrapper) -> Option<&'static str> {
    Some(match w {
        Wrapper::None => return None,
        Wrapper::ContextCall => "bsl:IDS_RTE_CONTEXTCALL",
        Wrapper::CtorCall => "bsl:IDS_RTE_CTORCALL",
        Wrapper::BuiltinArg => "bsl:IDS_RTE_BADARGVALUE",
        Wrapper::SetAttr => "bsl:IDS_RTE_CONTEXTSETPROPVAL",
        Wrapper::GetAttr => "bsl:IDS_RTE_CONTEXTGETPROPVAL",
        Wrapper::Compile => "bsl:IDS_RTE_COMPILE",
    })
}

pub(crate) fn own_categories(w: Wrapper) -> CategorySet {
    use Category::*;
    match w {
        Wrapper::None => CategorySet::of(&[AllErrors]),
        Wrapper::Compile => CategorySet::of(&[AllErrors, ScriptRuntimeError, ScriptUseError]),
        _ => CategorySet::of(&[AllErrors, ScriptRuntimeError]),
    }
}

pub(crate) fn chain_categories(w: Wrapper, inner: Option<&str>, has_cause: bool) -> CategorySet {
    const CONTEXT: [Wrapper; 3] = [Wrapper::ContextCall, Wrapper::SetAttr, Wrapper::GetAttr];
    if let Some(key) = inner {
        let observed = messages::categories(w, key).or_else(|| {
            CONTEXT
                .contains(&w)
                .then(|| CONTEXT.iter().find_map(|c| messages::categories(*c, key)))
                .flatten()
        });
        if let Some(bits) = observed {
            return CategorySet(bits).with(Category::AllErrors);
        }
    }
    default_categories(w, has_cause)
}

pub(crate) fn default_categories(w: Wrapper, has_cause: bool) -> CategorySet {
    use Category::*;
    let base = CategorySet::of(&[AllErrors, ScriptRuntimeError, ScriptUseError]);
    match w {
        Wrapper::None | Wrapper::ContextCall | Wrapper::SetAttr | Wrapper::GetAttr => base,
        Wrapper::BuiltinArg if has_cause => base.with(OtherError),
        Wrapper::BuiltinArg => base,
        Wrapper::CtorCall => base.with(OtherError),
        Wrapper::Compile => base.with(ScriptCompileError),
    }
}
