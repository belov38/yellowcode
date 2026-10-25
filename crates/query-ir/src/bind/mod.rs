mod refid;
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
    pub params: Vec<PBound>,
    pub values: Vec<QValue>,
    pub tables: Vec<TableId>,
    pub constants: Vec<TableId>,
}

pub(crate) fn resolve<'s>(stmt: &'s StmtS, ctx: &BindCtx<'_>) -> Result<Bound<'s>, ExecError> {
    values::resolve(stmt, ctx)
}

pub fn bind(stmt: &StmtS, ctx: &BindCtx<'_>) -> Result<StmtL, ExecError> {
    let bound = resolve(stmt, ctx)?;
    crate::lower::lower(&bound, ctx)
}
