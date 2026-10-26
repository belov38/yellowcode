#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
#![forbid(unsafe_code)]
mod host;
pub use host::{QueryHost, QueryHostProvider, VmExecHost};
mod errors;
mod query_obj;

pub fn register(reg: &mut bsl_vm::Registry, provider: std::sync::Arc<dyn QueryHostProvider>) {
    use bsl_vm::TypeImpl;
    use std::sync::Arc;
    if let Some(ty) = platform_catalog::catalog().find_type("Query") {
        reg.type_impl(
            ty,
            TypeImpl {
                ctor: Some(Arc::new(move |ex, args| {
                    query_obj::Query::construct(ex, args, ty, provider.clone())
                })),
            },
        );
    }
    if let Some(ty) = platform_catalog::catalog().find_type("TempTablesManager") {
        reg.type_impl(
            ty,
            TypeImpl {
                ctor: Some(Arc::new(move |_, _| Ok(manager::Manager::construct(ty)))),
            },
        );
    }
}

pub mod values;

mod columns;
mod manager;
mod params_desc;
mod result_obj;
mod selection;

mod unload;
