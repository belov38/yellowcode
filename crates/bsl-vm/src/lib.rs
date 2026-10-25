#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

mod access;
pub mod error;
mod exec;
mod fragment;
pub mod fs;
pub mod global;
pub mod host;
mod lang;
mod module;
pub mod object;
pub mod ops;
pub mod place;
pub mod registry;
pub mod runtime;
pub mod shared;
pub mod types;
pub mod value;

pub use error::{
    Category, CategorySet, Cause, ErrorInfo, ErrorWireIdentity, ErrorWireIdentityParts,
    ErrorWireMetadata, Exception, FatalError, FatalKind, HostError, MsgKey, MsgRef, StackEntry,
};
pub use fs::{FileSystem, FsError, FsErrorKind, FsStat};
pub use host::{
    AppHost, AppSerializer, AppXdto, AppXdtoProp, AppXdtoValue, ExecFlags, GuardedOp,
    ModuleContext, ModuleInstanceRef, ModuleRef, ReuseStore, SessionEnv,
};
pub use lang::{error_info_of, error_info_value};
pub use module::module_init_error;
pub use object::{CallError, CallResult, HostObject, creation_number};
pub use ops::{BinOp, CmpOp};
pub use place::{Arg, Place};
pub use registry::{AppTypeInfo, CtorFn, GlobalFn, GlobalProp, Presenter, Registry, TypeImpl};
pub use runtime::{CALL_UNIT, ExecOptions, Execution, Runtime, StackWeights};
pub use shared::{SharedObject, SharedValue};
pub use types::{MemberId, MemberKind, TypeId, TypeRegistry};
pub use value::{ObjRef, Value};

pub use brace;
pub use bsl_compiler::{AppGlobal, AppMethodSig, AppNames, CompiledModule, ContextValue, NoApp};
pub use bsl_prim::{BslDate, Collation, CompatMode, Number, Str};
pub use platform_catalog::{Catalog, TypeIdx, XdtoName, messages::Lang};
