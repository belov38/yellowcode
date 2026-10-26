use super::AppNamesImpl;
use bsl_compiler::{ApplicationModule, ApplicationVariable, ExecContext};
use bsl_prim::fold_name;
use bsl_syntax::SourceText;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type Variables = Arc<HashMap<String, ApplicationVariable>>;
pub(super) type VariableCache = Mutex<HashMap<ExecContext, Variables>>;

impl AppNamesImpl {
    pub(super) fn application_variables(&self, context: ExecContext) -> Variables {
        if let Ok(cache) = self.application_vars.lock()
            && let Some(variables) = cache.get(&context)
        {
            return variables.clone();
        }
        let mut variables = HashMap::new();
        if let Some(application) = ApplicationModule::for_context(context) {
            let kind = match application {
                ApplicationModule::Managed => md_model::ModuleKind::ManagedApplicationModule,
                ApplicationModule::Ordinary => md_model::ModuleKind::OrdinaryApplicationModule,
            };
            if let Some(module) = self
                .model
                .config
                .modules
                .iter()
                .find(|module| module.kind == kind)
            {
                let source = SourceText::from_utf8(
                    "application",
                    module
                        .source
                        .strip_prefix(super::BOM)
                        .unwrap_or(&module.source),
                );
                for variable in bsl_compiler::application_variables(&source, application, context) {
                    variables
                        .entry(fold_name(&variable.name))
                        .or_insert(variable);
                }
            }
        }
        let variables = Arc::new(variables);
        if let Ok(mut cache) = self.application_vars.lock() {
            cache.insert(context, variables.clone());
        }
        variables
    }
}
