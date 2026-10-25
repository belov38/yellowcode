use super::L;
use query_syntax::ast::*;
use query_syntax::kw::Kw;
use query_syntax::{ScriptVariant, Span};

fn with(script: ScriptVariant, f: impl FnOnce(&mut L)) -> String {
    let mut l = L {
        script,
        ..L::default()
    };
    f(&mut l);
    l.flush();
    l.out
}

pub(crate) fn expr(e: &Expr, script: ScriptVariant) -> String {
    with(script, |l| l.expr(e))
}

pub(crate) fn item(it: &SelectItem, script: ScriptVariant) -> String {
    let mut it = it.clone();
    it.alias = None;
    with(script, |l| l.item(&it, 0))
}

pub(crate) fn path(p: &Path, script: ScriptVariant) -> String {
    with(script, |l| l.path(p))
}

pub(crate) fn param(p: &Param, script: ScriptVariant) -> String {
    with(script, |l| l.param(p))
}

pub(crate) fn vt_arg(a: &Option<VtArg>, script: ScriptVariant) -> String {
    let Some(a) = a else {
        return String::new();
    };
    let one = VtArgs {
        args: vec![Some(a.clone())],
        span: Span::default(),
    };
    let t = with(script, |l| l.vt_args(&one, 0));
    t.strip_prefix('(')
        .and_then(|t| t.strip_suffix(')'))
        .unwrap_or(&t)
        .trim()
        .to_string()
}

pub(crate) fn join_kind(k: JoinKind, script: ScriptVariant) -> String {
    with(script, |l| {
        l.kws(match k {
            JoinKind::Inner => &[Kw::Inner, Kw::Join],
            JoinKind::Left => &[Kw::Left, Kw::Join],
            JoinKind::Right => &[Kw::Right, Kw::Join],
            JoinKind::Full => &[Kw::Full, Kw::Join],
        })
    })
}

pub(crate) fn control_point(cp: &ControlPoint, script: ScriptVariant) -> String {
    with(script, |t| match cp {
        ControlPoint::Overall(_) => t.kw(Kw::Overall),
        ControlPoint::Field {
            expr,
            hierarchy,
            periods,
            alias,
            ..
        } => {
            t.expr(expr);
            match hierarchy {
                Some(Hierarchy::All(_)) => t.kw(Kw::Hierarchy),
                Some(Hierarchy::Only(_)) => t.kws(&[Kw::Only, Kw::Hierarchy]),
                None => {}
            }
            if let Some(p) = periods {
                t.kw(Kw::Periods);
                t.raw("(");
                t.tok(&p.unit.text);
                for bound in [&p.begin, &p.end].into_iter().flatten() {
                    t.raw(",");
                    t.expr(bound);
                }
                t.raw(")");
            }
            t.alias(alias.as_ref());
        }
    })
}

pub(crate) fn dcs_block(b: &DcsBlock, script: ScriptVariant) -> String {
    with(script, |l| l.dcs_block(b, 0))
}

pub(crate) fn group_set(set: &[Expr], script: ScriptVariant) -> String {
    with(script, |l| {
        l.raw("(");
        for (k, e) in set.iter().enumerate() {
            if k > 0 {
                l.raw(",");
            }
            l.expr(e);
        }
        l.raw(")");
    })
}
