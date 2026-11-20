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
mod retained_workspace;
mod reuse;
pub mod runtime;
mod session_graph;
pub mod shared;
pub use session_graph::retire::{SessionBacking, SessionEdges};
pub use session_graph::{SessionExport, SessionImports, SessionObject, session_shared_bytes};
pub mod types;
pub mod value;

pub use error::{
    Category, CategorySet, Cause, ErrorInfo, ErrorWireIdentity, ErrorWireIdentityParts,
    ErrorWireMetadata, Exception, FatalError, FatalKind, HostError, MsgKey, MsgRef, StackEntry,
};
pub use fs::{FileSystem, FsError, FsErrorKind, FsStat};
pub use host::{
    AppHost, AppSerializer, AppXdto, AppXdtoProp, AppXdtoValue, ExecFlags, GuardedOp,
    IbRegionalSettings, ModuleContext, ModuleInstanceRef, ModuleRef, ReuseStore, SessionEnv,
};
pub use lang::{error_info_of, error_info_value};
pub use module::module_init_error;
pub use object::{CallError, CallResult, HostObject, RetainedMemory, creation_number};
pub use ops::{BinOp, CmpOp};
pub use place::{Arg, Place};
pub use registry::{AppTypeInfo, CtorFn, GlobalFn, GlobalProp, Presenter, Registry, TypeImpl};
pub use retained_workspace::{RetainedWorkspace, retained_hash_bytes};
pub use runtime::{
    CALL_UNIT, ExecOptions, Execution, MESSAGE_CAP_BYTES, MESSAGE_CAP_COUNT, Runtime, StackWeights,
    UserMessage,
};
pub use shared::{SharedObject, SharedValue};
pub use types::{MemberId, MemberKind, TypeId, TypeRegistry};
pub use value::{ObjRef, Value};

pub use brace;
pub use bsl_compiler::{AppGlobal, AppMethodSig, AppNames, CompiledModule, ContextValue, NoApp};
pub use bsl_prim::{BslDate, Collation, CompatMode, Number, Str};
pub use platform_catalog::{Catalog, TypeIdx, XdtoName, messages::Lang};

thread_local! {
    static DROP_FORM: std::cell::Cell<DropForm> = const { std::cell::Cell::new(DropForm::None) };
    static EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static RUNNING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static TEARDOWN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropForm {
    None,
    AssignedObject(u64),
    AssignedContainer,
    Observed,
    Unobserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Death {
    End,
    Assigned(u64),
    Body,
    Unobserved,
}

pub fn drop_form() -> DropForm {
    DROP_FORM.with(std::cell::Cell::get)
}

pub fn with_drop_form<R>(form: DropForm, f: impl FnOnce() -> R) -> R {
    struct Restore(DropForm);
    impl Drop for Restore {
        fn drop(&mut self) {
            DROP_FORM.with(|d| d.set(self.0));
        }
    }
    let _restore = Restore(DROP_FORM.with(|d| d.replace(form)));
    f()
}

pub fn in_body() -> bool {
    RUNNING.with(|r| r.get() > 0) && TEARDOWN.with(|t| t.get() == 0)
}

pub fn death() -> Death {
    match drop_form() {
        DropForm::AssignedObject(epoch) => Death::Assigned(epoch),
        DropForm::AssignedContainer | DropForm::Observed => Death::Body,
        DropForm::Unobserved => Death::Unobserved,
        DropForm::None if in_body() => Death::Unobserved,
        DropForm::None => Death::End,
    }
}

#[derive(Clone, Copy)]
enum Counter {
    Running,
    Teardown,
}

impl Counter {
    fn step(self, up: bool) {
        let step = |c: &std::cell::Cell<u32>| {
            c.set(if up {
                c.get().saturating_add(1)
            } else {
                c.get().saturating_sub(1)
            })
        };
        match self {
            Counter::Running => RUNNING.with(step),
            Counter::Teardown => TEARDOWN.with(step),
        }
    }
}

struct Depth(Counter);

impl Depth {
    fn enter(counter: Counter) -> Depth {
        counter.step(true);
        Depth(counter)
    }
}

impl Drop for Depth {
    fn drop(&mut self) {
        self.0.step(false);
    }
}

pub(crate) fn running<R>(f: impl FnOnce() -> R) -> R {
    let _depth = Depth::enter(Counter::Running);
    f()
}

pub(crate) fn teardown<R>(f: impl FnOnce() -> R) -> R {
    let _depth = Depth::enter(Counter::Teardown);
    f()
}

fn observed_container(v: &value::Value) -> bool {
    static TYPES: std::sync::OnceLock<(Option<TypeIdx>, Option<TypeIdx>)> =
        std::sync::OnceLock::new();
    let (array, structure) = *TYPES.get_or_init(|| {
        let c = platform_catalog::catalog();
        (c.find_type("Массив"), c.find_type("Структура"))
    });
    let value::Value::Object(o) = v else {
        return false;
    };
    let t = object::HostObject::type_id(&**o);
    [array, structure]
        .into_iter()
        .flatten()
        .any(|idx| t == TypeId::of_catalog(idx))
}

pub(crate) fn drop_assigned(old: value::Value) {
    let form = if observed_container(&old) {
        DropForm::AssignedContainer
    } else {
        DropForm::AssignedObject(EPOCH.with(|e| {
            let n = e.get().wrapping_add(1);
            e.set(n);
            n
        }))
    };
    with_drop_form(form, || drop(old));
}
