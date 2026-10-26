#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

pub mod app;
mod application;
pub use application::application_variables;
pub mod builtins;
pub mod bytecode;
pub mod compat;
mod disasm;
pub mod error;
pub mod fragment;
pub mod ir;
pub mod ir_dump;
mod lower;
pub mod options;
pub mod pipeline;
mod resolve;
pub mod split;
pub mod sym;

pub use app::{
    AppGlobal, AppMethodSig, AppNames, ApplicationModule, ApplicationVariable, ContextValue, NoApp,
    ParamSig,
};
pub use bsl_prim::CompatMode;
pub use bsl_syntax::{ExecContext, MsgId};
pub use builtins::CatalogBuiltins;
pub use bytecode::{CompiledModule, Handler, MethodCode, Op, ParamCode};
pub use error::CompileError;
pub use fragment::{CompiledFragment, FragmentScope, compile_fragment, compile_fragment_ir};
pub use ir_dump::dump_ir;
pub use options::{CommonFlags, CompileOptions, ModuleKind, Reuse};
pub use pipeline::{compile, compile_ir};
pub use platform_catalog::messages::Lang;
pub use split::split_names_for_test;
pub use sym::{Interner, Sym};
