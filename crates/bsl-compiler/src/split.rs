use crate::builtins::CatalogBuiltins;
use crate::ir::{Directive, Intercept};
use crate::options::{CompileOptions, ModuleKind};
use bsl_prim::fold_name;
use bsl_syntax::{ExecContext, ParseMode, ParseOptions, ast};
use std::collections::HashSet;

pub(crate) fn parse_options<'a>(
    o: &'a CompileOptions<'a>,
    builtins: &'a CatalogBuiltins<'a>,
) -> ParseOptions<'a> {
    ParseOptions {
        mode: ParseMode::Module,
        context: o.context,
        methods_only: o.kind.methods_only(),
        extension: o.extension.is_some(),
        builtins,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MethodRole {
    Compiled,
    CallableOnly,
}

pub(crate) struct ModuleView<'m> {
    pub vars: Vec<&'m ast::VarDecl>,
    pub methods: Vec<(&'m ast::Method, MethodRole)>,
    pub body: Vec<&'m ast::Stmt>,
    pub dup_var: Option<&'m ast::Ident>,
}

const DIRECTIVES: [(Directive, [&str; 2]); 5] = [
    (Directive::AtClient, ["наклиенте", "atclient"]),
    (Directive::AtServer, ["насервере", "atserver"]),
    (
        Directive::AtServerNoContext,
        ["насерверебезконтекста", "atservernocontext"],
    ),
    (
        Directive::AtClientAtServerNoContext,
        [
            "наклиентенасерверебезконтекста",
            "atclientatservernocontext",
        ],
    ),
    (
        Directive::AtClientAtServer,
        ["наклиентенасервере", "atclientatserver"],
    ),
];

pub fn directive_of(annotations: &[ast::Annotation]) -> Option<Directive> {
    annotations.iter().rev().find_map(|a| {
        let n = fold_name(&a.name.name);
        DIRECTIVES
            .iter()
            .find(|(_, names)| names.contains(&n.as_str()))
            .map(|(d, _)| *d)
    })
}

pub fn intercept_of(annotations: &[ast::Annotation]) -> Option<Intercept> {
    annotations.iter().rev().find_map(|a| {
        let n = fold_name(&a.name.name);
        let target = || {
            a.arg
                .as_deref()
                .map(String::from_utf16_lossy)
                .unwrap_or_default()
        };
        match n.as_str() {
            "перед" | "before" => Some(Intercept::Before(target())),
            "после" | "after" => Some(Intercept::After(target())),
            "вместо" | "around" => Some(Intercept::Around(target())),
            "изменениеиконтроль" | "changeandvalidate" => {
                Some(Intercept::ChangeAndValidate(target()))
            }
            _ => None,
        }
    })
}

fn client_side(kind: ModuleKind) -> bool {
    match kind {
        ModuleKind::Common(f) => f.client_managed || f.client_ordinary,
        ModuleKind::ManagedApplication | ModuleKind::OrdinaryApplication => true,
        _ => false,
    }
}

fn role(kind: ModuleKind, ctx: ExecContext, directive: Option<Directive>) -> Option<MethodRole> {
    use Directive::*;
    let split_server = matches!(kind, ModuleKind::Form | ModuleKind::Command);
    if !ctx.is_client() {
        return match directive {
            Some(AtClient) => None,
            _ => Some(MethodRole::Compiled),
        };
    }
    match directive {
        Some(AtClient) => (split_server || client_side(kind)).then_some(MethodRole::Compiled),
        Some(AtClientAtServerNoContext | AtClientAtServer) => {
            (split_server || client_side(kind)).then_some(MethodRole::Compiled)
        }
        Some(AtServer | AtServerNoContext) | None if split_server => Some(MethodRole::CallableOnly),
        None => client_side(kind).then_some(MethodRole::Compiled),
        Some(AtServer | AtServerNoContext) => None,
    }
}

pub(crate) fn view<'m>(m: &'m ast::Module, kind: ModuleKind, ctx: ExecContext) -> ModuleView<'m> {
    let compiled = |d: Option<Directive>| role(kind, ctx, d) == Some(MethodRole::Compiled);
    let vars: Vec<&ast::VarDecl> = m
        .vars
        .iter()
        .filter(|v| compiled(directive_of(&v.annotations)))
        .collect();
    let methods = m
        .methods
        .iter()
        .filter_map(|x| role(kind, ctx, directive_of(&x.annotations)).map(|r| (x, r)))
        .collect();
    let body = if compiled(None) {
        m.body.iter().collect()
    } else {
        Vec::new()
    };
    let mut seen = HashSet::new();
    let dup_var = vars
        .iter()
        .flat_map(|v| &v.items)
        .map(|i| &i.name)
        .find(|n| !seen.insert(fold_name(&n.name)));
    ModuleView {
        vars,
        methods,
        body,
        dup_var,
    }
}

#[doc(hidden)]
pub fn split_names_for_test(
    src: &str,
    kind: ModuleKind,
    ctx: ExecContext,
) -> Vec<(String, &'static str)> {
    let catalog = platform_catalog::catalog();
    let builtins = CatalogBuiltins(catalog);
    let opts = ParseOptions {
        mode: ParseMode::Module,
        context: ctx,
        methods_only: kind.methods_only(),
        extension: false,
        builtins: &builtins,
    };
    let out = bsl_syntax::parse(&bsl_syntax::SourceText::from_str("m", src), &opts);
    view(&out.module, kind, ctx)
        .methods
        .into_iter()
        .map(|(m, r)| {
            let role = match r {
                MethodRole::Compiled => "compiled",
                MethodRole::CallableOnly => "callable",
            };
            (m.name.name.clone(), role)
        })
        .collect()
}
