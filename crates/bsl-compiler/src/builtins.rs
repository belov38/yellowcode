use bsl_syntax::{BuiltinSig, BuiltinTable};
use platform_catalog::Catalog;

pub struct CatalogBuiltins<'a>(pub &'a Catalog);

impl BuiltinTable for CatalogBuiltins<'_> {
    fn builtin(&self, name: &str) -> Option<BuiltinSig> {
        self.0.builtin(name).map(|g| BuiltinSig {
            min: g.min_args,
            max: g.max_args,
        })
    }
}
