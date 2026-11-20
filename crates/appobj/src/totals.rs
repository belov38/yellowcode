mod accum;

pub(crate) use accum::{service, std_value};

use crate::Application;
use crate::model::{FieldRole, ObjId, ObjKind, StdAttr};
use crate::txn::{CallTxn, TxnError};
use bsl_vm::HostError;
use ibschema::fields::FieldValue;
use ibschema::storage::TableId;
use ibschema::store::{ReadTxn, RowHandle, StoreResult, WriteTxn};
use ibschema::{DateTime, Numeric, Row};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    Receipt,
    Expense,
    Turnover,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Movement {
    pub period: DateTime,
    pub kind: RecordKind,
    pub active: bool,
    pub dims: Vec<FieldValue>,
    pub res: Vec<Numeric>,
}

impl Movement {
    pub fn of_row(app: &Application, reg: ObjId, row: &Row) -> Result<Movement, HostError> {
        let model = app.model();
        let def = model.obj(reg);
        let st = model
            .storage_of(reg)
            .ok_or_else(|| refusal(&def.name, "регистр без хранения"))?;
        let value = |i: usize| -> Option<&FieldValue> {
            let f = st.fields.get(i).copied().flatten()?;
            row.get(usize::from(f.0))
        };
        let std = |a: StdAttr| def.fields.iter().position(|f| f.std == Some(a));
        let period = match std(StdAttr::Period).and_then(value) {
            Some(FieldValue::Date(d)) => *d,
            _ => return Err(refusal(&def.name, "движение без периода")),
        };
        let active = matches!(
            std(StdAttr::Active).and_then(value),
            Some(FieldValue::Bool(true))
        );
        let kind = match std(StdAttr::RecordType) {
            None => RecordKind::Turnover,
            Some(i) => match value(i) {
                Some(FieldValue::Number(n)) if n.digits().iter().all(|d| *d == 0) => {
                    RecordKind::Receipt
                }
                Some(FieldValue::Number(n)) if n.eq_value(&Numeric::from_i64(1)) => {
                    RecordKind::Expense
                }
                _ => return Err(refusal(&def.name, "вид движения не прочитан")),
            },
        };
        let mut dims = Vec::new();
        let mut res = Vec::new();
        for (i, f) in def.fields.iter().enumerate() {
            match f.role {
                FieldRole::Dimension => dims.push(value(i).cloned().unwrap_or(FieldValue::Null)),
                FieldRole::Resource => match value(i) {
                    Some(FieldValue::Number(n)) => res.push(n.clone()),
                    _ => return Err(refusal(&def.name, "ресурс движения не число")),
                },
                _ => {}
            }
        }
        Ok(Movement {
            period,
            kind,
            active,
            dims,
            res,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Change<'a> {
    pub reg: ObjId,
    pub removed: &'a [Movement],
    pub added: &'a [Movement],
}

#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Update {
        table: TableId,
        row: RowHandle,
        values: Row,
    },
    Insert {
        table: TableId,
        values: Row,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TotalsPlan {
    pub edits: Vec<Edit>,
    pub commit_refusal: Option<String>,
}

pub(crate) fn refusal(reg: &str, what: &str) -> HostError {
    HostError::not_implemented(
        "РегистрНакопленияНаборЗаписей",
        &format!("Записать: итоги {reg}: {what}"),
    )
}

pub fn plan(
    app: &Application,
    view: &dyn ReadTxn,
    ch: &Change<'_>,
) -> Result<TotalsPlan, HostError> {
    let model = app.model();
    let def = model.obj(ch.reg);
    if def.kind != ObjKind::AccumulationRegister {
        return Err(refusal(&def.name, "не регистр накопления"));
    }
    if ch.removed.is_empty() && ch.added.is_empty() {
        return Ok(TotalsPlan::default());
    }
    accum::plan(app, view, def, ch)
}

pub fn write(w: &mut dyn WriteTxn, plan: &TotalsPlan) -> StoreResult<()> {
    for e in &plan.edits {
        match e {
            Edit::Update { table, row, values } => w.update(*table, *row, values)?,
            Edit::Insert { table, values } => {
                w.insert(*table, values)?;
            }
        }
    }
    Ok(())
}

pub fn apply(txn: &CallTxn, app: &Application, plan: &TotalsPlan) -> Result<(), TxnError> {
    if let Some(why) = &plan.commit_refusal {
        txn.refuse_commit(why.clone());
    }
    if plan.edits.is_empty() {
        return Ok(());
    }
    txn.with_write(app, |w| write(w, plan))
}
