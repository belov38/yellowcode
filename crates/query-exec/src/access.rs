use std::collections::HashMap;

use ibschema::names::StdField;
use ibschema::storage::{StorageMap, TableId};
use ibschema::store::{ReadTxn, Row};
use query_ir::convert::row_to_q;
use query_ir::host::HostError;
use query_ir::{
    AccessPredicate, BindCtx, BindHost, ExecError, PExpr, ParamValue, ParamValues, QTableId,
    QValue, QuerySchema, RelL, SessionValues, StorageBinding,
};
use query_syntax::{Lang, MsgId};

use crate::exec::execute;
use crate::source::ExecEnv;
use crate::store::StoreSource;

pub fn compile_restriction(
    schema: &QuerySchema,
    table: QTableId,
    text: &str,
    context: &str,
    lang: Lang,
) -> Result<AccessPredicate, HostError> {
    query_syntax::parse_access_condition(text)
        .and_then(|parsed| query_ir::compile_access_condition(&parsed, text, schema, table))
        .map_err(|error| {
            let detail = error.render(text, lang, schema.script());
            let restriction = HostError::new(format!(
                "{}\n{context}\n{detail}",
                MsgId::DBERROR_SECURITYINSTR.text(lang)
            ))
            .with_cause(HostError::new(detail));
            HostError::new(MsgId::SELECT_ERROR_RUNTIMEERROR.text(lang)).with_cause(restriction)
        })
}

pub struct RowView<'r> {
    table: TableId,
    row: &'r [ibschema::fields::FieldValue],
}

impl<'r> RowView<'r> {
    pub fn from_row(t: TableId, row: &'r Row) -> RowView<'r> {
        RowView {
            table: t,
            row: row.as_slice(),
        }
    }

    pub fn table(&self) -> TableId {
        self.table
    }
}

pub struct AccessCtx<'a> {
    pub schema: &'a QuerySchema,
    pub storage: &'a StorageBinding,
    pub map: &'a StorageMap,
    pub host: &'a dyn BindHost,
    pub sess: &'a dyn SessionValues,
}

struct SessionParams(HashMap<String, ParamValue>);

impl ParamValues for SessionParams {
    fn get(&self, name: &str) -> Option<ParamValue> {
        self.0.get(name).cloned()
    }
}

fn fatal(what: &str) -> ExecError {
    ExecError::Fatal(format!("access check: {what}"))
}

fn leftmost_scan(mut r: &mut RelL) -> Option<&mut RelL> {
    loop {
        if matches!(r, RelL::Scan { .. }) {
            return Some(r);
        }
        r = match r {
            RelL::Join { l, .. } => l,
            RelL::CheckAccess { input, .. }
            | RelL::RunningBalance { input, .. }
            | RelL::Filter { input, .. }
            | RelL::Project { input, .. }
            | RelL::Aggregate { input, .. }
            | RelL::Sort { input, .. }
            | RelL::Top { input, .. }
            | RelL::SemiJoin { input, .. }
            | RelL::HierarchyOrder { input, .. } => input,
            RelL::Distinct(input) => input,
            RelL::Empty
            | RelL::Scan { .. }
            | RelL::TempScan { .. }
            | RelL::Values { .. }
            | RelL::HierarchyClosure { .. }
            | RelL::RegisterRows(_)
            | RelL::Slice { .. }
            | RelL::Union { .. } => {
                return None;
            }
        };
    }
}

pub fn eval_access(
    p: &AccessPredicate,
    row: &RowView<'_>,
    txn: &dyn ReadTxn,
    ctx: &AccessCtx<'_>,
    env: &ExecEnv<'_>,
) -> Result<Option<bool>, ExecError> {
    let table = ctx
        .storage
        .table(p.table())
        .ok_or_else(|| fatal("the restricted table is not in the storage binding"))?;
    if table != row.table {
        return Err(fatal("the row is of another table than the condition's"));
    }
    let info = ctx
        .map
        .try_table(table)
        .ok_or_else(|| fatal("unknown table"))?;
    let mut values = HashMap::new();
    for name in p.session_params() {
        let v = ctx.sess.session_param(&name).map_err(ExecError::Host)?;
        values.insert(name, ParamValue::Value(v));
    }
    let params = SessionParams(values);
    let bctx = BindCtx {
        security: query_ir::host::SecurityCtx {
            access: &PredicateInput,
            session: ctx.sess,
        },
        schema: ctx.schema,
        storage: ctx.storage,
        map: ctx.map,
        params: &params,
        host: ctx.host,
    };
    let src = StoreSource::new(txn, ctx.map);
    let q = row_to_q(info, row.row);
    if p.is_full() {
        let id_field = info
            .field_by_std(StdField::Id)
            .ok_or_else(|| fatal("the full form needs a table with ID"))?;
        let id = q
            .get(usize::from(id_field.0))
            .ok_or_else(|| fatal("the row has no ID"))?;
        let l = p.bind_check(&bctx, Some(id))?;
        let r = execute(&l, &src, env)?;
        return Ok(Some(!r.rows.is_empty()));
    }
    let mut l = p.bind_check(&bctx, None)?;
    let leaf =
        leftmost_scan(&mut l.root).ok_or_else(|| fatal("no scan of the restricted table"))?;
    let RelL::Scan { table: t, fields } = &*leaf else {
        return Err(fatal("no scan of the restricted table"));
    };
    if *t != table {
        return Err(fatal("the leftmost scan is not of the restricted table"));
    }
    let lits = fields
        .iter()
        .map(|f| {
            q.get(usize::from(f.0))
                .map(|v| PExpr::Lit(v.clone()))
                .ok_or_else(|| fatal("the row is shorter than the table"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    *leaf = RelL::Project {
        input: Box::new(RelL::Empty),
        exprs: lits,
    };
    let r = execute(&l, &src, env)?;
    match r.rows.first().and_then(|r| r.first()) {
        Some(QValue::Bool(b)) => Ok(Some(*b)),
        Some(QValue::Null) => Ok(None),
        other => Err(fatal(&format!("the condition gave {other:?}"))),
    }
}

struct PredicateInput;
impl query_ir::host::AccessPolicy for PredicateInput {
    fn read_policy(
        &self,
        _: &query_ir::host::SourceTable,
        _: &query_ir::host::FieldSet,
    ) -> Result<query_ir::host::Access, query_ir::HostError> {
        Err(query_ir::HostError::new(
            "predicate input used by ordinary bind",
        ))
    }
}
