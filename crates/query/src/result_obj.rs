use crate::{QueryHostProvider, columns, errors, query_obj::member, selection, values};
use bsl_vm::{Arg, CallResult, Execution, HostObject, MemberId, TypeId, TypeIdx, Value};
use query_exec::{
    QueryResult,
    totals::RowRef,
    walk::{self, Walk},
};
use query_ir::{ColumnInfo, QValue};
use std::{any::Any, rc::Rc, sync::Arc};

pub(crate) struct ResultData {
    identity: u32,
    pub columns: Value,
    pub names: Vec<String>,
    pub result: Arc<QueryResult>,
    rows: Vec<Vec<Value>>,
    totals: Vec<Vec<Value>>,
    pub provider: Option<Arc<dyn QueryHostProvider>>,
    pub type_code_columns: Vec<bool>,
}
pub(crate) fn new(
    ex: &mut Execution,
    host: &dyn crate::QueryHost,
    cols: &[ColumnInfo],
    rows: &[Vec<QValue>],
) -> CallResult<Value> {
    let bytes = rows
        .iter()
        .fold(0u64, |n, r| n.saturating_add(query_exec::ops::row_bytes(r)));
    ex.reserve(
        bytes
            .saturating_add(query_exec::model::vec_block(
                rows.len(),
                std::mem::size_of::<Vec<QValue>>(),
            ))
            .saturating_add(query_exec::model::columns_model(cols)),
    )?;
    materialized(
        ex,
        host,
        Arc::new(QueryResult {
            columns: cols.to_vec(),
            rows: rows.to_vec(),
            totals: None,
        }),
        None,
    )
}
pub(crate) fn materialized(
    ex: &mut Execution,
    host: &dyn crate::QueryHost,
    result: Arc<QueryResult>,
    provider: Option<Arc<dyn QueryHostProvider>>,
) -> CallResult<Value> {
    let cols = &result.columns;
    let total_rows = result.totals.as_ref().map_or(0, |t| t.totals.len());
    ex.reserve(
        ((result.rows.len() + total_rows) as u64)
            .saturating_mul((cols.len() as u64).saturating_mul(64).saturating_add(24)),
    )?;
    let columns = columns::columns(ex, host, cols, false)?;
    let mut convert = |rows: &[Vec<QValue>]| -> CallResult<Vec<Vec<Value>>> {
        let mut converted = Vec::with_capacity(rows.len());
        for row in rows {
            let mut cells = Vec::with_capacity(cols.len());
            for q in row.iter().take(cols.len()) {
                cells.push(values::to_value(ex, host, q)?);
            }
            if cells.len() != cols.len() {
                return Err(errors::internal("query result row width"));
            }
            converted.push(cells);
        }
        Ok(converted)
    };
    let rows = convert(&result.rows)?;
    let totals = match &result.totals {
        Some(t) => convert(&t.totals)?,
        None => Vec::new(),
    };
    let names = cols.iter().map(|c| c.name.clone()).collect();
    let type_code_columns = cols
        .iter()
        .map(|c| c.ty.clone().without_null() == query_ir::QType::other(query_ir::Other::Type))
        .collect();
    from_data(Rc::new(ResultData {
        identity: bsl_vm::creation_number(),
        columns,
        names,
        rows,
        totals,
        result,
        provider,
        type_code_columns,
    }))
}
pub(crate) fn adapt(
    ex: &mut Execution,
    host: &dyn crate::QueryHost,
    out: query_exec::StmtOutcome,
    provider: Arc<dyn QueryHostProvider>,
) -> CallResult<Value> {
    let result = match out {
        query_exec::StmtOutcome::Dropped => return Ok(Value::Undefined),
        query_exec::StmtOutcome::Result(r) => r,
        query_exec::StmtOutcome::Count(n) => Arc::new(QueryResult {
            columns: vec![query_exec::count_column(match ex.script_variant() {
                bsl_vm::Lang::Ru => query_syntax::ScriptVariant::Russian,
                bsl_vm::Lang::En => query_syntax::ScriptVariant::English,
            })],
            rows: vec![vec![QValue::Number(n.into())]],
            totals: None,
        }),
    };
    materialized(ex, host, result, Some(provider))
}
impl ResultData {
    pub fn cell(&self, row: RowRef, col: usize) -> CallResult<Value> {
        match row {
            RowRef::Detail(i) => self.rows.get(i as usize),
            RowRef::Total(i) => self.totals.get(i as usize),
        }
        .and_then(|r| r.get(col))
        .cloned()
        .ok_or_else(|| columns::bounds().into())
    }
    pub fn with_collation<T>(
        &self,
        ex: &mut Execution,
        mut f: impl FnMut(&mut Execution, &dyn bsl_prim::Collation) -> CallResult<T>,
    ) -> CallResult<T> {
        if let Some(p) = &self.provider {
            crate::manager::with_host(ex, &**p, |ex, h| f(ex, h.env().collation))
        } else {
            f(ex, &bsl_prim::IcuCollation::new("ru_RU"))
        }
    }
}
struct ResultObject {
    ty: TypeIdx,
    data: Rc<ResultData>,
}
impl HostObject for ResultObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(self.data.identity as i32)
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Self>()
                .is_some_and(|o| Rc::ptr_eq(&self.data, &o.data)),
        )
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member(self.ty, m, false) {
            "Columns" => Ok(self.data.columns.clone()),
            _ => Err(columns::unavailable(self.ty, m, false).into()),
        }
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(columns::readonly().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(self.ty, m, true) {
            "IsEmpty" => Ok(Value::Bool(self.data.result.is_empty())),
            "Select" => {
                let (w, groups, fill) = selection::arguments(ex, args)?;
                let cursor = self.data.with_collation(ex, |ex, coll| {
                    walk::select(&self.data.result, w, &groups, &fill, coll)
                        .map_err(|e| errors::walk_error(ex, e))
                })?;
                selection::new(self.data.clone(), cursor)
            }
            "Unload" => {
                let walk = selection::traversal(ex, args)?;
                self.unload(ex, walk)
            }
            _ => Err(columns::unavailable(self.ty, m, true).into()),
        }
    }
}
impl ResultObject {
    fn unload(&self, ex: &mut Execution, walk: Walk) -> CallResult<Value> {
        crate::unload::unload(ex, &self.data, walk)
    }
}
pub(crate) fn from_data(data: Rc<ResultData>) -> CallResult<Value> {
    Ok(Value::Object(Rc::new(ResultObject {
        ty: columns::idx("QueryResult")?,
        data,
    })))
}
