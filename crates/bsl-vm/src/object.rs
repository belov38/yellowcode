use crate::error::{Exception, HostError};
use crate::place::Arg;
use crate::runtime::Execution;
use crate::shared::SharedValue;
use crate::types::{MemberId, MemberKind, TypeId};
use crate::value::Value;
use bsl_prim::Str;
use std::any::Any;
use std::cell::Cell;

pub type CallResult<T> = Result<T, CallError>;

#[derive(Debug)]
pub enum CallError {
    Host(HostError),
    Exc(Exception),
}

impl From<HostError> for CallError {
    fn from(e: HostError) -> CallError {
        CallError::Host(e)
    }
}

impl From<Exception> for CallError {
    fn from(e: Exception) -> CallError {
        CallError::Exc(e)
    }
}

thread_local! {
    static CREATED: Cell<u32> = const { Cell::new(0) };
}

pub fn creation_number() -> u32 {
    CREATED.with(|c| {
        let n = c.get();
        c.set(n.wrapping_add(1));
        n
    })
}

pub(crate) fn restart_creation_numbers() {
    CREATED.with(|c| c.set(0));
}

const NOT_INDEXED: &str = "bsl:IDS_RTE_NOTINDEXED";
const NOT_ENUM: &str = "bsl:IDS_RTE_NOTENUM";

pub trait HostObject: Any {
    fn type_id(&self) -> TypeId;
    fn as_any(&self) -> &dyn Any;
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value>;
    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()>;
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value>;

    fn find_member(&self, _name: &str) -> Option<MemberId> {
        None
    }

    fn find_member_guarded(
        &self,
        _ex: &mut Execution,
        name: &str,
    ) -> Result<Option<MemberId>, Exception> {
        Ok(self.find_member(name))
    }

    fn member_kind(&self, _m: MemberId) -> Option<MemberKind> {
        None
    }

    fn index_get(&self, _ex: &mut Execution, _key: &Value) -> CallResult<Value> {
        Err(HostError::key(NOT_INDEXED).raw().into())
    }

    fn index_set(&self, _ex: &mut Execution, _key: &Value, _v: Value) -> CallResult<()> {
        Err(HostError::key(NOT_INDEXED).raw().into())
    }

    fn iter_len(&self, _ex: &mut Execution) -> Option<usize> {
        None
    }

    fn iter_get(&self, _ex: &mut Execution, _i: usize) -> CallResult<Value> {
        Err(HostError::key(NOT_ENUM).raw().into())
    }

    fn iter_keys(&self, _ex: &mut Execution) -> CallResult<Option<Vec<Value>>> {
        Ok(None)
    }

    fn iter_value(&self, _ex: &mut Execution, _key: &Value) -> CallResult<Option<Value>> {
        Ok(None)
    }

    fn presentation(&self, _ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(None)
    }

    fn boost_hash(&self) -> Option<i32> {
        None
    }

    fn equals(&self, _other: &dyn HostObject) -> Option<bool> {
        None
    }

    fn is_empty_value(&self) -> bool {
        false
    }

    fn to_shared(&self) -> Option<SharedValue> {
        None
    }
}
