use crate::{ApplicationModule, ApplicationVariable, CatalogBuiltins, ExecContext};
use bsl_syntax::{ParseMode, ParseOptions, SourceText};

pub fn application_variables(
    source: &SourceText,
    module: ApplicationModule,
    context: ExecContext,
) -> Vec<ApplicationVariable> {
    if ApplicationModule::for_context(context) != Some(module) {
        return Vec::new();
    }
    let builtins = CatalogBuiltins(platform_catalog::catalog());
    let parsed = bsl_syntax::parse(
        source,
        &ParseOptions {
            mode: ParseMode::Module,
            context,
            methods_only: false,
            extension: false,
            builtins: &builtins,
        },
    );
    let view = crate::split::view(&parsed.module, module.kind(), context);
    view.vars
        .iter()
        .flat_map(|declaration| &declaration.items)
        .filter(|item| item.export)
        .map(|item| ApplicationVariable {
            module,
            name: item.name.name.clone(),
        })
        .collect()
}
