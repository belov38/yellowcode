use super::{Arity, GlobalLookup};
use crate::app::AppNames;
use crate::compat;
use crate::ir::{Callee, GlobalRef, Place};
use crate::options::CompileOptions;
use crate::sym::Sym;
use bsl_prim::CompatMode;
use bsl_syntax::ExecContext;
use bsl_syntax::ast::Ident;
use platform_catalog::{Avail, Catalog, Context, GlobalMethod, TypeIdx, TypeKind};

pub(crate) fn platform_context(ctx: ExecContext) -> Context {
    match ctx {
        ExecContext::Server => Context::Server,
        ExecContext::ThinClient => Context::ThinClient,
        ExecContext::WebClient => Context::WebClient,
        ExecContext::MobileClient => Context::MobileClient,
        ExecContext::ThickClientManaged | ExecContext::ThickClientOrdinary => Context::ThickClient,
        ExecContext::ExternalConnection => Context::ExternalConnection,
        ExecContext::MobileAppClient => Context::MobileAppClient,
        ExecContext::MobileAppServer => Context::MobileAppServer,
        ExecContext::MobileStandaloneServer => Context::MobileStandaloneServer,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Platform<'a> {
    pub catalog: &'a Catalog,
    pub ctx: ExecContext,
    pub compat: CompatMode,
}

impl<'a> Platform<'a> {
    pub(crate) fn new(o: &CompileOptions<'a>) -> Platform<'a> {
        Platform {
            catalog: o.catalog,
            ctx: o.context,
            compat: o.compat,
        }
    }

    pub(crate) fn on_server(self) -> bool {
        self.ctx == ExecContext::Server
    }

    fn available(self, a: Avail) -> bool {
        a.has(platform_context(self.ctx))
    }

    pub(crate) fn property(self, name: &str) -> Option<u16> {
        let i = self.catalog.find_global_property_idx(name)?;
        let p = self.catalog.global_properties().get(usize::from(i))?;
        (!p.name_ru.contains('<') && self.available(p.avail)).then_some(i)
    }

    pub(crate) fn system_enum(self, name: &str) -> Option<TypeIdx> {
        self.catalog.find_types(name).iter().copied().find(|&t| {
            let info = self.catalog.type_info(t);
            if self.on_server() {
                match info.server_compile {
                    Some(s) => s.global_value,
                    None => info.kind == TypeKind::Enum && info.enum_global,
                }
            } else {
                info.kind == TypeKind::Enum && (info.enum_global || self.available(info.avail))
            }
        })
    }

    pub(crate) fn method(self, name: &str) -> Option<(u16, &'a GlobalMethod)> {
        let i = self.catalog.find_global_method_idx(name)?;
        let g = self.catalog.global_methods().get(usize::from(i))?;
        let known = if self.on_server() {
            g.server_known
        } else {
            (g.builtin && g.server_known) || self.available(g.info.avail)
        };
        (known && !compat::hides(self.compat, g)).then_some((i, g))
    }

    pub(crate) fn new_type(self, name: &str) -> Option<TypeIdx> {
        self.catalog.find_types(name).iter().copied().find(|&t| {
            let info = self.catalog.type_info(t);
            if self.on_server() {
                info.server_compile
                    .map_or(info.presentation_ru.is_some(), |s| s.type_known)
            } else {
                self.available(info.avail)
            }
        })
    }
}

pub(crate) struct Globals<'a> {
    app: &'a dyn AppNames,
    module: &'a str,
    platform: Platform<'a>,
}

impl<'a> Globals<'a> {
    pub(crate) fn new(o: &CompileOptions<'a>) -> Globals<'a> {
        Globals {
            app: o.app,
            module: o.module_name,
            platform: Platform::new(o),
        }
    }
}

impl GlobalLookup for Globals<'_> {
    fn value(&mut self, id: &Ident, sym: &Sym) -> Option<Place> {
        let name = id.name.as_str();
        if self.app.context_value(self.module, name).is_some() {
            return Some(Place::Context(sym.clone()));
        }
        let global = if let Some(g) = self.app.global_value(name, self.platform.ctx) {
            GlobalRef::App(g)
        } else if let Some(variable) = self.app.application_variable(name, self.platform.ctx) {
            GlobalRef::ApplicationVariable(variable)
        } else if let Some(i) = self.platform.property(name) {
            GlobalRef::PlatformProperty(i)
        } else {
            GlobalRef::SystemEnum(self.platform.system_enum(name)?)
        };
        Some(Place::Global(global))
    }

    fn method(&mut self, id: &Ident, sym: &Sym) -> Option<(Callee, Arity)> {
        let name = id.name.as_str();
        if self.app.context_method(self.module, name) {
            return Some((Callee::Context(sym.clone()), Arity::UNCHECKED));
        }
        if let Some(sig) = self.app.global_method(name, self.platform.ctx) {
            let arity = Arity::of_app(&sig);
            return Some((Callee::App(sig), arity));
        }
        let (i, g) = self.platform.method(name)?;
        Some((Callee::Platform(i), Arity::of_platform(g)))
    }
}
