use crate::{errors, query_obj::member, values};
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId, TypeIdx, Value,
};
use std::{any::Any, rc::Rc};

pub(crate) fn idx(name: &str) -> CallResult<TypeIdx> {
    platform_catalog::catalog()
        .find_type(name)
        .ok_or_else(|| errors::internal("missing query catalog type"))
}
pub(crate) fn readonly() -> HostError {
    HostError::key("bsl:IDS_RTE_INDEXED_READONLY")
}
pub(crate) fn bounds() -> HostError {
    HostError::key("bsl:IDS_RTE_INDEX_OFFBOUND")
}
pub(crate) fn unavailable(ty: TypeIdx, m: MemberId, method: bool) -> HostError {
    HostError::not_implemented(
        &platform_catalog::catalog().type_info(ty).name_ru,
        member(ty, m, method),
    )
}
pub(crate) fn position(key: &Value, names: &[String]) -> CallResult<usize> {
    match key {
        Value::String(s) => names
            .iter()
            .position(|n| fold_name(n) == fold_name(&s.to_string_lossy()))
            .ok_or_else(|| bounds().into()),
        Value::Number(n) => n
            .to_i64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|i| *i < names.len())
            .ok_or_else(|| bounds().into()),
        _ => Err(bsl_stdlib::bind::type_mismatch(1).into()),
    }
}
pub(crate) fn record(name: &str, fields: Vec<(&str, Value)>) -> CallResult<Value> {
    Ok(Value::Object(Rc::new(Record {
        ty: idx(name)?,
        fields: fields.into_iter().map(|(n, v)| (n.to_owned(), v)).collect(),
    })))
}
struct Record {
    ty: TypeIdx,
    fields: Vec<(String, Value)>,
}
impl HostObject for Record {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.fields
            .iter()
            .find(|(n, _)| n == member(self.ty, m, false))
            .map(|(_, v)| v.clone())
            .ok_or_else(|| unavailable(self.ty, m, false).into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(readonly().into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(unavailable(self.ty, m, true).into())
    }
}
pub(crate) fn collection(name: &str, names: Vec<String>, items: Vec<Value>) -> CallResult<Value> {
    Ok(Value::Object(Rc::new(Collection {
        ty: idx(name)?,
        names,
        items,
    })))
}
struct Collection {
    ty: TypeIdx,
    names: Vec<String>,
    items: Vec<Value>,
}
impl HostObject for Collection {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.items
            .get(
                usize::try_from(m.0)
                    .unwrap_or(usize::MAX)
                    .wrapping_sub(10000),
            )
            .cloned()
            .ok_or_else(|| unavailable(self.ty, m, false).into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(readonly().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(self.ty, m, true) {
            "Count" => Ok(Value::from(self.items.len() as i64)),
            "Get" => {
                let key = bsl_stdlib::bind::arg_value(ex, args.first())?;
                self.index_get(ex, &key)
            }
            "Find" => {
                let v = bsl_stdlib::bind::arg_value(ex, args.first())?;
                Ok(position(&v, &self.names)
                    .ok()
                    .and_then(|i| self.items.get(i))
                    .cloned()
                    .unwrap_or(Value::Undefined))
            }
            "IndexOf" => {
                let v = bsl_stdlib::bind::arg_value(ex, args.first())?;
                Ok(Value::from(
                    self.items
                        .iter()
                        .position(|x| bsl_vm::ops::equal(x, &v))
                        .map_or(-1, |i| i as i64),
                ))
            }
            _ => Err(unavailable(self.ty, m, true).into()),
        }
    }
    fn index_get(&self, _: &mut Execution, key: &Value) -> CallResult<Value> {
        self.items
            .get(position(key, &self.names)?)
            .cloned()
            .ok_or_else(|| bounds().into())
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        self.names
            .iter()
            .position(|n| fold_name(n) == fold_name(name))
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| MemberId(i + 10000))
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        (m.0 >= 10000).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.items.len())
    }
    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        self.items.get(i).cloned().ok_or_else(|| bounds().into())
    }
}
pub(crate) fn description(
    ex: &mut Execution,
    host: &dyn crate::QueryHost,
    ty: &query_ir::QType,
) -> CallResult<Value> {
    let mut ids = Vec::new();
    for (_, atom) in host.schema().atom_uuid(ty) {
        let Value::Type(t) = values::to_value(ex, host, &query_ir::QValue::Type(atom))? else {
            return Err(errors::internal("host type conversion did not return Type"));
        };
        ids.push(t);
    }
    let number = ty.number.map(|n| bsl_stdlib::NumberQualifiers {
        digits: u32::from(n.digits),
        fraction: u32::from(n.scale),
        sign: if n.nonneg {
            bsl_stdlib::AllowedSign::Nonnegative
        } else {
            bsl_stdlib::AllowedSign::Any
        },
    });
    let string = ty.string.map(|s| bsl_stdlib::StringQualifiers {
        length: s.len,
        allowed: if s.fixed {
            bsl_stdlib::AllowedLength::Fixed
        } else {
            bsl_stdlib::AllowedLength::Variable
        },
    });
    let date = ty.date.map(|d| bsl_stdlib::DateQualifiers {
        parts: match d {
            query_ir::DateQ::Date => bsl_stdlib::DateParts::Date,
            query_ir::DateQ::Time => bsl_stdlib::DateParts::Time,
            query_ir::DateQ::DateTime => bsl_stdlib::DateParts::DateTime,
        },
    });
    Ok(bsl_stdlib::new_type_description(
        bsl_stdlib::TypeDescription::new(&ids, number, string, date, None, ex.runtime()),
    ))
}
pub(crate) fn columns(
    ex: &mut Execution,
    host: &dyn crate::QueryHost,
    cols: &[query_ir::ColumnInfo],
    temp: bool,
) -> CallResult<Value> {
    let mut items = Vec::new();
    for c in cols {
        let mut fields = vec![
            ("Name", Value::from(c.name.as_str())),
            ("ValueType", description(ex, host, &c.ty)?),
        ];
        if !temp {
            fields.push(("Width", Value::from(i64::from(c.width))));
        }
        items.push(record(
            if temp {
                "QueryTempTableColumn"
            } else {
                "QueryResultColumn"
            },
            fields,
        )?);
    }
    collection(
        if temp {
            "QueryTempTableColumns"
        } else {
            "QueryResultColumnsCollection"
        },
        cols.iter().map(|c| c.name.clone()).collect(),
        items,
    )
}
