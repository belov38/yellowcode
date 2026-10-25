mod cast;
mod order;
mod qualifiers;
use crate::bind::{arg_value, type_mismatch, value_mismatch};
use crate::collections::{method_name, not_implemented, property_name};
use crate::enums::type_by_id;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostObject, MemberId, Registry, Runtime, SharedObject,
    SharedValue, TypeId, TypeIdx, TypeImpl, Value,
};
pub use cast::cast_value;
use qualifiers::Qualifier;
pub use qualifiers::{
    AllowedLength, AllowedSign, BinaryQualifiers, DateParts, DateQualifiers, NumberQualifiers,
    StringQualifiers,
};
use std::{any::Any, rc::Rc, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeDescription {
    types: Vec<TypeId>,
    keeps_undefined: bool,
    number: Option<NumberQualifiers>,
    string: Option<StringQualifiers>,
    date: Option<DateQualifiers>,
    binary: Option<BinaryQualifiers>,
}
impl TypeDescription {
    pub fn new(
        types: &[TypeId],
        number: Option<NumberQualifiers>,
        string: Option<StringQualifiers>,
        date: Option<DateQualifiers>,
        binary: Option<BinaryQualifiers>,
        rt: &Runtime,
    ) -> Self {
        let mut types: Vec<_> = types
            .iter()
            .copied()
            .filter(|t| *t != TypeId::UNDEFINED)
            .collect();
        order::sort(&mut types, rt);
        types.dedup();
        let mut td = Self {
            types,
            keeps_undefined: false,
            number,
            string,
            date,
            binary,
        };
        td.normalize();
        td
    }

    pub fn from_type_array(
        types: &[TypeId],
        number: Option<NumberQualifiers>,
        string: Option<StringQualifiers>,
        date: Option<DateQualifiers>,
        binary: Option<BinaryQualifiers>,
        rt: &Runtime,
    ) -> Self {
        let mut td = Self::new(types, number, string, date, binary, rt);
        td.keeps_undefined = true;
        if types.contains(&TypeId::UNDEFINED) {
            td.types.push(TypeId::UNDEFINED);
            order::sort(&mut td.types, rt);
        }
        td
    }

    fn normalize(&mut self) {
        if !self.contains(TypeId::NUMBER) || self.number == Some(NumberQualifiers::default()) {
            self.number = None;
        }
        if !self.contains(TypeId::STRING) || self.string == Some(StringQualifiers::default()) {
            self.string = None;
        }
        if !self.contains(TypeId::DATE) || self.date == Some(DateQualifiers::default()) {
            self.date = None;
        }
        if !self.contains(TypeId::of_catalog(idx("BinaryData")))
            || self.binary == Some(BinaryQualifiers::default())
        {
            self.binary = None;
        }
    }
    pub fn types(&self) -> &[TypeId] {
        &self.types
    }
    pub fn contains(&self, t: TypeId) -> bool {
        self.types.contains(&t)
    }
    pub fn number(&self) -> NumberQualifiers {
        self.number.unwrap_or_default()
    }
    pub fn string(&self) -> StringQualifiers {
        self.string.unwrap_or_default()
    }
    pub fn date(&self) -> DateQualifiers {
        self.date.unwrap_or_default()
    }
    pub fn binary(&self) -> BinaryQualifiers {
        self.binary.unwrap_or_default()
    }
}

#[allow(
    clippy::expect_used,
    reason = "standard types are an embedded catalog invariant"
)]
pub(crate) fn idx(id: &str) -> TypeIdx {
    type_by_id(id).expect("standard type in the embedded catalog")
}

pub fn new_type_description(td: TypeDescription) -> Value {
    Value::Object(Rc::new(td))
}
pub fn type_description_of(v: &Value) -> Option<TypeDescription> {
    Some(
        v.as_object()?
            .as_any()
            .downcast_ref::<TypeDescription>()?
            .clone(),
    )
}

impl HostObject for TypeDescription {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(idx("TypeDescription"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Ok(match property_name(idx("TypeDescription"), m) {
            Some("NumberQualifiers") => Qualifier::Number(self.number()).value(),
            Some("StringQualifiers") => Qualifier::String(self.string()).value(),
            Some("DateQualifiers") => Qualifier::Date(self.date()).value(),
            Some("BinaryDataQualifiers") => Qualifier::Binary(self.binary()).value(),
            _ => return Err(not_implemented(idx("TypeDescription"), m).into()),
        })
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(idx("TypeDescription"), m).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(idx("TypeDescription"), m) {
            Some("Types") => {
                ex.reserve(self.types.len() as u64 * 32)?;
                Ok(crate::new_array(
                    self.types.iter().copied().map(Value::Type).collect(),
                ))
            }
            Some("ContainsType") => {
                let Value::Type(t) = arg_value(ex, args.first())? else {
                    return Err(type_mismatch(1).into());
                };
                Ok(Value::Bool(self.contains(t)))
            }
            Some("AdjustValue") => {
                let v = arg_value(ex, args.first())?;
                Ok(cast_value(ex, self, v)?)
            }
            _ => Err(not_implemented(idx("TypeDescription"), m).into()),
        }
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let mut parts = Vec::new();
        for t in &self.types {
            parts.push(ex.present(&Value::Type(*t))?.to_string_lossy());
        }
        let text = parts.join(", ");
        ex.reserve(text.len() as u64 * 2)?;
        Ok(Some(Str::from(text)))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Self>()
                .is_some_and(|o| o == self),
        )
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(
            self.types
                .iter()
                .fold(0i32, |h, t| h.wrapping_mul(37).wrapping_add(t.0 as i32)),
        )
    }
    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(self.clone())))
    }
}
impl SharedObject for TypeDescription {
    fn to_value(&self, _: &mut Execution) -> Result<Value, Exception> {
        Ok(new_type_description(self.clone()))
    }
}

pub(crate) fn register(reg: &mut Registry) {
    reg.type_impl(
        idx("TypeDescription"),
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
    qualifiers::register(reg);
}

fn type_list(ex: &mut Execution, v: &Value, param: usize) -> CallResult<(Vec<TypeId>, bool)> {
    match v {
        Value::Undefined => Ok((Vec::new(), false)),
        Value::String(s) => {
            let mut out = Vec::new();
            for name in s
                .to_string_lossy()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let types = ex.runtime().types();
                let known = types.by_name(name).filter(|t| {
                    types.app_type(*t).is_some()
                        || types.catalog_type(*t).is_some_and(|i| {
                            let info = platform_catalog::catalog().type_info(i);
                            info.accepts_type_name(name).unwrap_or_else(|| {
                                info.avail.has(platform_catalog::Context::Server)
                            })
                        })
                });
                let Some(t) = known else {
                    return Err(value_mismatch(param).with_arg(name).into());
                };
                out.push(t);
            }
            Ok((out, false))
        }
        _ => {
            if let Some(td) = type_description_of(v) {
                return Ok((td.types, td.keeps_undefined));
            }
            let Some(items) = crate::array_items(v) else {
                return Err(type_mismatch(param).into());
            };
            let types = items
                .into_iter()
                .map(|v| {
                    if let Value::Type(t) = v {
                        Ok(t)
                    } else {
                        Err(type_mismatch(param))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((types, true))
        }
    }
}

fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let first = arg_value(ex, args.first())?;
    let base = type_description_of(&first);
    let (mut types, mut keeps) = type_list(ex, &first, 1)?;
    let start = if base.is_some() {
        let add = arg_value(ex, args.get(1))?;
        let sub = arg_value(ex, args.get(2))?;
        let (add, extra_keeps) = type_list(ex, &add, 2)?;
        let (sub, _) = type_list(ex, &sub, 3)?;
        types.extend(add);
        types.retain(|t| !sub.contains(t));
        keeps |= extra_keeps;
        3
    } else {
        1
    };
    let mut td =
        base.unwrap_or_else(|| TypeDescription::new(&[], None, None, None, None, ex.runtime()));
    td.types = types;
    td.keeps_undefined = keeps;
    if !keeps {
        td.types.retain(|t| *t != TypeId::UNDEFINED);
    }
    order::sort(&mut td.types, ex.runtime());
    td.types.dedup();
    for offset in 0..4 {
        let v = arg_value(ex, args.get(start + offset))?;
        if matches!(v, Value::Undefined) {
            continue;
        }
        let Some(q) = Qualifier::of(&v) else {
            return Err(type_mismatch(start + offset + 1).into());
        };
        match (offset, q) {
            (0, Qualifier::Number(n)) => td.number = Some(*n),
            (1, Qualifier::String(s)) => td.string = Some(*s),
            (2, Qualifier::Date(d)) => td.date = Some(*d),
            (3, Qualifier::Binary(b)) => td.binary = Some(*b),
            _ => return Err(type_mismatch(start + offset + 1).into()),
        }
    }
    td.normalize();
    Ok(new_type_description(td))
}
