mod refid;
pub(crate) mod security;
mod values;

pub(crate) use values::{fold_chain, visit_expr, visit_stmt};

pub use refid::{ref_id, uuid_of};

use crate::exec_error::ExecError;
use crate::host::{HostError, PredefinedName};
use crate::ir::StmtS;
use crate::lower::StmtL;
use crate::schema::{QTableId, QuerySchema, StorageBinding};
use crate::temp::TempColumn;
use crate::value::QValue;
use ibschema::storage::{StorageMap, TableId};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Value(QValue),
    List(Vec<QValue>),
    Table {
        columns: Vec<TempColumn>,
        rows: Vec<Vec<QValue>>,
    },
}

pub trait ParamValues {
    fn get(&self, name: &str) -> Option<ParamValue>;
}

pub trait BindHost {
    fn predefined_ref(&self, p: &PredefinedName) -> Result<QValue, HostError>;
    fn touch_table(&self, t: TableId) -> Result<(), HostError>;
    fn predefined_names(&self, _table: QTableId) -> Result<Vec<([u8; 16], String)>, HostError> {
        Err(HostError {
            message: "BindHost::predefined_names is not implemented by the host".to_string(),
            cause: None,
            payload: None,
        })
    }
    fn week_start(&self) -> Result<u32, HostError>;
}

pub struct BindCtx<'a> {
    pub schema: &'a QuerySchema,
    pub storage: &'a StorageBinding,
    pub map: &'a StorageMap,
    pub params: &'a dyn ParamValues,
    pub host: &'a dyn BindHost,
    pub security: crate::host::SecurityCtx<'a>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValueSet {
    pub values: Vec<QValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValueRows {
    pub width: usize,
    pub rows: Vec<Vec<QValue>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PBound {
    Value(QValue),
    Set(Arc<ValueSet>),
    Rows(Arc<ValueRows>),
}

#[derive(Debug)]
pub struct Bound<'s> {
    pub stmt: &'s StmtS,
    pub(crate) security: security::Security,
    pub params: Vec<PBound>,
    pub values: Vec<QValue>,
    pub tables: Vec<TableId>,
    pub constants: Vec<TableId>,
}

pub(crate) fn resolve<'s>(stmt: &'s StmtS, ctx: &BindCtx<'_>) -> Result<Bound<'s>, ExecError> {
    values::resolve(stmt, ctx)
}

pub fn bind(stmt: &StmtS, ctx: &BindCtx<'_>) -> Result<StmtL, ExecError> {
    bind_with_policy_charge(stmt, ctx, &mut |_| Ok(()))
}

#[derive(Debug, Default)]
pub struct PolicyCompileStats {
    pub statement: crate::ir::stats::IrStats,
    pub predicates: crate::sema::stats::StmtStats,
}

pub fn bind_with_policy_charge(
    stmt: &StmtS,
    ctx: &BindCtx<'_>,
    charge: &mut dyn FnMut(&PolicyCompileStats) -> Result<(), ExecError>,
) -> Result<StmtL, ExecError> {
    let (secured, security) = security::prepare(stmt, ctx, charge)?;
    let stmt = secured.as_ref().unwrap_or(stmt);
    let mut bound = resolve(stmt, ctx)?;
    bound.security = security;
    crate::lower::lower(&bound, ctx)
}

pub fn preflight_virtual(stmt: &crate::CompiledStmt, ctx: &BindCtx<'_>) -> Result<(), ExecError> {
    security::preflight_virtual(stmt, ctx)
}
