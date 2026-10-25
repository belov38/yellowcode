use crate::Application;
use crate::model::StdAttr;
use crate::predefined::{AreaKey, engine_error};
use crate::values::field::default_of;
use crate::values::refs::{RefValue, ref_value};
use bsl_prim::Str;
use bsl_vm::Value;
use ibschema::storage::TableId;
use ibschema::values::RefId;
use query_ir::{BindHost, HostError, PredefinedName, QTableId, QValue};

pub(crate) struct CallBindHost<'c> {
    app: &'c Application,
}

impl<'c> CallBindHost<'c> {
    pub fn new(app: &'c Application) -> CallBindHost<'c> {
        CallBindHost { app }
    }
}

impl BindHost for CallBindHost<'_> {
    fn week_start(&self) -> Result<u32, HostError> {
        let locale = &self.app.options().locale;
        bsl_prim::locale::first_weekday(locale)
            .ok_or_else(|| HostError::new(format!("unknown locale of the base: {locale}")))
    }

    fn predefined_ref(&self, p: &PredefinedName) -> Result<QValue, HostError> {
        let ctx = self
            .app
            .query_ctx_text()
            .map_err(|e| HostError::new(format!("internal: {e}")))?;
        let obj = ctx
            .schema
            .table(p.table)
            .object
            .and_then(|md| self.app.model().by_uuid(md))
            .ok_or_else(|| HostError::new("internal: the predefined item has no object"))?;
        let r = self
            .app
            .predefined()
            .by_name(self.app, obj, &AreaKey::default(), &p.name)
            .map_err(|e| engine_error(&e))?;
        let table = self
            .app
            .model()
            .storage_of(obj)
            .and_then(|s| s.ref_number)
            .ok_or_else(|| HostError::new("internal: the object has no reference table"))?;
        Ok(QValue::Ref { table, id: r.id.0 })
    }

    fn touch_table(&self, t: TableId) -> Result<(), HostError> {
        self.app.touch_table(t)
    }

    fn predefined_names(&self, table: QTableId) -> Result<Vec<([u8; 16], String)>, HostError> {
        let ctx = self
            .app
            .query_ctx_text()
            .map_err(|e| HostError::new(format!("internal: {e}")))?;
        let Some(obj) = ctx
            .schema
            .table(table)
            .object
            .and_then(|md| self.app.model().by_uuid(md))
        else {
            return Ok(Vec::new());
        };
        Ok(self.app.predefined().names(self.app, obj))
    }
}

pub(crate) fn exec_error(e: &query_ir::ExecError) -> bsl_vm::HostError {
    let t = e.to_string();
    bsl_vm::HostError::text(&t, &t)
}

fn ref_of(app: &Application, table: u32, id: RefId) -> Result<Value, bsl_vm::HostError> {
    let obj = app.model().obj_of_ref_number(table).ok_or_else(|| {
        let t = format!("internal: unknown reference table number {table}");
        bsl_vm::HostError::text(&t, &t)
    })?;
    ref_value(app, RefValue { obj, id })
}

pub(crate) fn to_value(
    app: &Application,
    v: &QValue,
    field: Option<&crate::model::FieldDef>,
) -> Result<Value, bsl_vm::HostError> {
    Ok(match v {
        QValue::Null => match field {
            Some(f)
                if f.usage == crate::model::Usage::ForFolderAndItem
                    && f.std != Some(StdAttr::PredefinedDataName) =>
            {
                default_of(app, f)?
            }
            Some(f) if f.std == Some(StdAttr::PredefinedDataName) => Value::from(""),
            _ => Value::Null,
        },
        QValue::Undefined => Value::Undefined,
        QValue::Bool(b) => Value::Bool(*b),
        QValue::Number(n) => Value::Number(n.clone()),
        QValue::String(s) => Value::String(Str::from(s.to_string_lossy().as_str())),
        QValue::Date(d) => Value::Date(*d),
        QValue::Uuid(b) => bsl_stdlib::values::new_uuid(*b),
        QValue::Ref { table, id } => ref_of(app, *table, RefId(*id))?,
        QValue::Type(_)
        | QValue::SysEnum(..)
        | QValue::Binary(_)
        | QValue::Storage(_)
        | QValue::PointInTime(_)
        | QValue::Boundary(_) => {
            return Err(bsl_vm::HostError::not_implemented(
                "Выборка",
                "значение этого типа",
            ));
        }
        QValue::Nested(_) => {
            return Err(bsl_vm::HostError::not_implemented(
                "Выборка",
                "вложенная таблица",
            ));
        }
    })
}
