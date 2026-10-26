use crate::{columns, errors, query_obj::member, result_obj::ResultData};
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Execution, HostObject, MemberId, MemberKind, TypeId, TypeIdx, Value,
};
use query_exec::{
    totals::RecordType,
    walk::{Selection as Cursor, Walk},
};
use std::{any::Any, cell::RefCell, rc::Rc};

pub(crate) fn traversal(ex: &mut Execution, args: &[Arg]) -> CallResult<Walk> {
    let v = bsl_stdlib::bind::arg_value(ex, args.first())?;
    if matches!(v, Value::Undefined) {
        return Ok(Walk::Direct);
    }
    let e = bsl_stdlib::enums::as_enum_value(&v)
        .filter(|e| {
            Some(e.type_idx()) == platform_catalog::catalog().find_type("QueryResultIteration")
        })
        .ok_or_else(|| bsl_stdlib::bind::type_mismatch(1))?;
    match e.name_en() {
        "Linear" => Ok(Walk::Direct),
        "ByGroups" => Ok(Walk::ByGroups),
        "ByGroupsWithHierarchy" => Ok(Walk::ByGroupsWithHierarchy),
        _ => Err(bsl_stdlib::bind::type_mismatch(1).into()),
    }
}
pub(crate) fn arguments(ex: &mut Execution, args: &[Arg]) -> CallResult<(Walk, String, String)> {
    let walk = traversal(ex, args)?;
    let mut text = |i: usize| -> CallResult<String> {
        match bsl_stdlib::bind::arg_value(ex, args.get(i))? {
            Value::Undefined => Ok(String::new()),
            Value::String(s) => Ok(s.to_string_lossy()),
            _ => Err(bsl_stdlib::bind::type_mismatch(i + 1).into()),
        }
    };
    Ok((walk, text(1)?, text(2)?))
}
pub(crate) fn new(data: Rc<ResultData>, cursor: Cursor) -> CallResult<Value> {
    Ok(Value::Object(Rc::new(Selection {
        ty: columns::idx("QueryResultSelection")?,
        data,
        cursor: RefCell::new(cursor),
    })))
}
struct Selection {
    ty: TypeIdx,
    data: Rc<ResultData>,
    cursor: RefCell<Cursor>,
}
impl Selection {
    fn cell(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        let cursor = self.cursor.borrow();
        let source = cursor.source_row().map_err(|e| errors::walk_error(ex, e))?;
        if let Some(row) = source {
            return self.data.cell(row, i);
        }
        let value = cursor
            .value(i)
            .map_err(|e| errors::walk_error(ex, e))?
            .clone();
        drop(cursor);
        let provider = self
            .data
            .provider
            .as_ref()
            .ok_or_else(|| errors::internal("virtual query record without host"))?;
        crate::manager::with_host(ex, &**provider, |ex, h| {
            crate::values::to_value(ex, h, &value)
        })
    }
    fn find_next(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let filter = bsl_stdlib::bind::arg_value(ex, args.first())?;
        let fields = bsl_stdlib::structure_fields(&filter)
            .ok_or_else(|| bsl_stdlib::bind::type_mismatch(1))?;
        let filter = fields
            .into_iter()
            .map(|(name, value)| {
                columns::position(&Value::from(name.as_str()), &self.data.names).map(|i| (i, value))
            })
            .collect::<CallResult<Vec<_>>>()?;
        while self.cursor.borrow_mut().next() {
            ex.charge(1)?;
            let mut matches = true;
            for (i, value) in &filter {
                if !bsl_vm::ops::equal(&self.cell(ex, *i)?, value) {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    }
}
impl HostObject for Selection {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.cell(
            ex,
            usize::try_from(m.0.saturating_sub(10000)).unwrap_or(usize::MAX),
        )
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(columns::readonly().into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(self.ty, m, true) {
            "Next" => Ok(Value::Bool(self.cursor.borrow_mut().next())),
            "Count" => Ok(Value::from(self.cursor.borrow().count() as i64)),
            "Reset" => {
                self.cursor.borrow_mut().reset();
                Ok(Value::Undefined)
            }
            "Owner" => crate::result_obj::from_data(self.data.clone()),
            "Get" => {
                let key = bsl_stdlib::bind::arg_value(ex, args.first())?;
                self.cell(ex, columns::position(&key, &self.data.names)?)
            }
            "Level" => self
                .cursor
                .borrow()
                .level()
                .map(|n| Value::from(i64::from(n)))
                .map_err(|e| errors::walk_error(ex, e)),
            "Group" => self
                .cursor
                .borrow()
                .grouping()
                .map(Value::from)
                .map_err(|e| errors::walk_error(ex, e)),
            "RecordType" => {
                let kind = self
                    .cursor
                    .borrow()
                    .record_type()
                    .map_err(|e| errors::walk_error(ex, e))?;
                let name = match kind {
                    RecordType::Overall => "Overall",
                    RecordType::GroupTotal => "GroupTotal",
                    RecordType::HierarchyTotal => "TotalByHierarchy",
                    RecordType::Detail => "DetailRecord",
                };
                bsl_stdlib::enums::enum_value(columns::idx("QueryRecordType")?, name)
                    .ok_or_else(|| errors::internal("query record enum"))
            }
            "Select" => {
                let (walk, groups, fill) = arguments(ex, args)?;
                let cursor = self.data.with_collation(ex, |ex, coll| {
                    self.cursor
                        .borrow()
                        .select(walk, &groups, &fill, coll)
                        .map_err(|e| errors::walk_error(ex, e))
                })?;
                new(self.data.clone(), cursor)
            }
            "NextByFieldValue" => {
                let field = bsl_stdlib::bind::arg_value(ex, args.first())?;
                let Value::String(field) = field else {
                    return Err(bsl_stdlib::bind::type_mismatch(1).into());
                };
                self.cursor
                    .borrow_mut()
                    .next_by_field_value(&field.to_string_lossy())
                    .map(Value::Bool)
                    .map_err(|e| errors::walk_error(ex, e))
            }
            "FindNext" => self.find_next(ex, args),
            _ => Err(columns::unavailable(self.ty, m, true).into()),
        }
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        self.data
            .names
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
    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        self.cell(ex, columns::position(key, &self.data.names)?)
    }
}
