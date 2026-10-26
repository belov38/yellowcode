use crate::{QueryHost, errors};
use bsl_prim::Str;
use bsl_stdlib::{AllowedLength, AllowedSign, DateParts, TypeDescription};
use bsl_vm::{CallResult, Execution, HostError, TypeId, Value};
use query_ir::{
    DateQ, NumQ, Other, ParamShape, ParamValue, QStr, QType, QValue, StrQ, TempColumn, TempSchema,
};
use std::sync::Arc;

pub struct ConvertedParam {
    pub value: Option<ParamValue>,
    pub shape: ParamShape,
}
fn unsupported(what: &str) -> bsl_vm::CallError {
    HostError::not_implemented("Запрос", what).into()
}
fn type_name(ex: &Execution, ty: TypeId) -> String {
    ex.runtime().types().name(ty, bsl_vm::Lang::En).to_owned()
}
fn query_type(ex: &mut Execution, host: &dyn QueryHost, ty: TypeId) -> CallResult<QType> {
    let name = type_name(ex, ty);
    if let Some(q) = host.schema().type_by_name(&[&name]) {
        return Ok(q);
    }
    match host.from_value(ex, &Value::Type(ty))? {
        Some(QValue::Type(q)) => Ok(q),
        _ => Err(unsupported("тип значения")),
    }
}
pub(crate) fn value_type(host: &dyn QueryHost, v: &QValue) -> CallResult<QType> {
    Ok(match v {
        QValue::Undefined => QType::undefined(),
        QValue::Null => QType::null_only(),
        QValue::Bool(_) => QType::boolean(),
        QValue::Number(_) => QType::num(NumQ {
            digits: 0,
            scale: 0,
            nonneg: false,
        }),
        QValue::String(_) => QType::str_(StrQ {
            len: 0,
            fixed: false,
        }),
        QValue::Date(_) => QType::date(DateQ::DateTime),
        QValue::Uuid(_) => QType::other(Other::Uuid),
        QValue::Binary(_) => QType::other(Other::Binary),
        QValue::Storage(_) => QType::other(Other::Storage),
        QValue::Type(_) => QType::other(Other::Type),
        QValue::SysEnum(t, _) => QType::other(Other::SysEnum(*t)),
        QValue::Nested(_) => QType::other(Other::QueryResult),
        QValue::PointInTime(_) => QType::other(Other::PointInTime),
        QValue::Boundary(_) => QType::other(Other::Boundary),
        QValue::Ref { table, .. } => host
            .storage()
            .ref_atoms()
            .into_iter()
            .find(|(n, _)| n == table)
            .map(|(_, r)| QType::reference(r))
            .ok_or_else(|| unsupported("неизвестный тип ссылки"))?,
    })
}
fn scalar(ex: &mut Execution, host: &dyn QueryHost, v: &Value) -> CallResult<Option<QValue>> {
    Ok(Some(match v {
        Value::Undefined => QValue::Undefined,
        Value::Null => QValue::Null,
        Value::Bool(b) => QValue::Bool(*b),
        Value::Number(n) => QValue::Number(n.clone()),
        Value::String(s) => QValue::String(QStr(s.units().into())),
        Value::Date(d) => QValue::Date(*d),
        Value::Type(t) => QValue::Type(query_type(ex, host, *t)?),
        _ => {
            if let Some(b) = bsl_stdlib::values::uuid_bytes(v) {
                QValue::Uuid(b)
            } else if let Some(b) = bsl_stdlib::values::binary_data_bytes(v) {
                QValue::Binary(b.to_vec().into())
            } else {
                return Ok(host.from_value(ex, v)?);
            }
        }
    }))
}
pub fn from_value(
    ex: &mut Execution,
    host: &dyn QueryHost,
    v: &Value,
) -> CallResult<ConvertedParam> {
    ex.charge(1)?;
    if let Some(q) = scalar(ex, host, v)? {
        return Ok(ConvertedParam {
            shape: ParamShape::Value(value_type(host, &q)?),
            value: Some(ParamValue::Value(q)),
        });
    }
    let list = bsl_stdlib::array_items(v).or_else(|| {
        bsl_stdlib::value_list_items(v).map(|l| l.into_iter().map(|(v, _, _)| v).collect())
    });
    if let Some(list) = list {
        ex.reserve((list.len() as u64).saturating_mul(128))?;
        let mut values = Vec::with_capacity(list.len());
        let mut ty = QType::undefined();
        for v in list {
            ex.charge(1)?;
            let q =
                scalar(ex, host, &v)?.ok_or_else(|| unsupported("элемент списка параметров"))?;
            ty = ty.merge(&value_type(host, &q)?);
            values.push(q);
        }
        return Ok(ConvertedParam {
            shape: ParamShape::List(ty),
            value: Some(ParamValue::List(values)),
        });
    }
    let name = type_name(ex, v.type_id());
    if name == "ValueTable" {
        return table_param(ex, host, v);
    }
    Ok(ConvertedParam {
        value: None,
        shape: if bsl_stdlib::structure_fields(v).is_some() {
            ParamShape::Structure
        } else if bsl_stdlib::type_description_of(v).is_some() {
            ParamShape::TypeDescription
        } else {
            ParamShape::Unsupported
        },
    })
}
fn table_param(ex: &mut Execution, host: &dyn QueryHost, v: &Value) -> CallResult<ConvertedParam> {
    let cols = ex.get_member(v, "Columns")?;
    let co = cols
        .as_object()
        .ok_or_else(|| errors::internal("table columns object"))?;
    let n = co
        .iter_len(ex)
        .ok_or_else(|| errors::internal("table columns iterator"))?;
    ex.reserve((n as u64).saturating_mul(256))?;
    let mut columns = Vec::with_capacity(n);
    for i in 0..n {
        ex.charge(1)?;
        let c = co.iter_get(ex, i)?;
        let Value::String(name) = ex.get_member(&c, "Name")? else {
            return Err(errors::internal("column name"));
        };
        let td = ex.get_member(&c, "ValueType")?;
        let td = bsl_stdlib::type_description_of(&td)
            .ok_or_else(|| errors::internal("column type description"))?;
        let ty = if td.types().is_empty() {
            None
        } else {
            Some(description_type(ex, host, &td)?)
        };
        columns.push(TempColumn {
            name: name.to_string_lossy(),
            ty,
            unknown: false,
            stored: None,
        });
    }
    let o = v
        .as_object()
        .ok_or_else(|| errors::internal("value table object"))?;
    let n = o
        .iter_len(ex)
        .ok_or_else(|| errors::internal("value table iterator"))?;
    ex.reserve(
        (n as u64).saturating_mul(
            (columns.len() as u64)
                .saturating_mul(128)
                .saturating_add(24),
        ),
    )?;
    let mut rows = Vec::with_capacity(n);
    for i in 0..n {
        let row = o.iter_get(ex, i)?;
        let mut cells = Vec::with_capacity(columns.len());
        for j in 0..columns.len() {
            ex.charge(1)?;
            let v = ex.index_get(&row, &Value::from(j as i64))?;
            cells.push(
                scalar(ex, host, &v)?
                    .ok_or_else(|| unsupported("значение ячейки таблицы параметра"))?,
            );
        }
        rows.push(cells);
    }
    Ok(ConvertedParam {
        shape: ParamShape::Table(Arc::new(TempSchema {
            name: String::new(),
            columns: columns.clone(),
            open: false,
        })),
        value: Some(ParamValue::Table { columns, rows }),
    })
}
fn description_type(
    ex: &mut Execution,
    host: &dyn QueryHost,
    td: &TypeDescription,
) -> CallResult<QType> {
    let mut q = QType::undefined();
    for t in td.types() {
        q = q.merge(&query_type(ex, host, *t)?);
    }
    if q.number.is_some() {
        let n = td.number();
        q.number = Some(NumQ {
            digits: u8::try_from(n.digits).map_err(|_| unsupported("числовой квалификатор"))?,
            scale: u8::try_from(n.fraction).map_err(|_| unsupported("числовой квалификатор"))?,
            nonneg: n.sign == AllowedSign::Nonnegative,
        });
    }
    if q.string.is_some() {
        let s = td.string();
        q.string = Some(StrQ {
            len: s.length,
            fixed: s.allowed == AllowedLength::Fixed,
        });
    }
    if q.date.is_some() {
        q.date = Some(match td.date().parts {
            DateParts::Date => DateQ::Date,
            DateParts::Time => DateQ::Time,
            DateParts::DateTime => DateQ::DateTime,
        });
    }
    Ok(q)
}
fn platform_type(ex: &Execution, name: &str) -> CallResult<Value> {
    ex.runtime()
        .types()
        .by_name(name)
        .map(Value::Type)
        .ok_or_else(|| errors::internal("missing catalog type"))
}
pub fn to_value(ex: &mut Execution, host: &dyn QueryHost, v: &QValue) -> CallResult<Value> {
    ex.charge(1)?;
    Ok(match v {
        QValue::Undefined => Value::Undefined,
        QValue::Null => Value::Null,
        QValue::Bool(b) => Value::Bool(*b),
        QValue::Number(n) => Value::Number(n.clone()),
        QValue::String(s) => Value::String(Str::from_units(&s.0)),
        QValue::Date(d) => Value::Date(*d),
        QValue::Uuid(b) => bsl_stdlib::values::new_uuid(*b),
        QValue::Binary(b) => bsl_stdlib::values::new_binary_data(b.to_vec()),
        QValue::Type(t) => {
            let name = if t == &QType::undefined() {
                Some("Undefined")
            } else if t.null {
                Some("Null")
            } else if t.boolean {
                Some("Boolean")
            } else if t.number.is_some() {
                Some("Number")
            } else if t.string.is_some() {
                Some("String")
            } else if t.date.is_some() {
                Some("Date")
            } else {
                t.others.first().and_then(|o| match o {
                    Other::Uuid => Some("UUID"),
                    Other::Binary => Some("BinaryData"),
                    Other::Storage => Some("ValueStorage"),
                    Other::Type => Some("Type"),
                    Other::QueryResult => Some("QueryResult"),
                    Other::ValueTable => Some("ValueTable"),
                    Other::TypeDescription => Some("TypeDescription"),
                    _ => None,
                })
            };
            if let Some(name) = name {
                platform_type(ex, name)?
            } else {
                host.to_value(ex, v)?
            }
        }
        QValue::Nested(n) => crate::result_obj::new(ex, host, &n.columns, &n.rows)?,
        _ => host.to_value(ex, v)?,
    })
}
