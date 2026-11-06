use crate::collections::ValueList;
use crate::typedesc::{TypeDescription, new_type_description};
use crate::xdto::{XdtoCtx, XdtoData};
use bsl_lib::xdto::{Model, QName, TypeDef};
use bsl_vm::{CallError, CallResult, Exception, Execution, ObjRef, Value};

const CORE: &str = "http://v8.1c.ru/8.1/data/core";
const XS: &str = "http://www.w3.org/2001/XMLSchema";
type Props = [(String, Vec<XdtoData>)];

fn field(
    ex: &Execution,
    name: &str,
    value: XdtoData,
) -> Result<(String, Vec<XdtoData>), Exception> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(1)
        .map_err(|_| ex.out_of_memory())?;
    values.push(value);
    Ok((name.into(), values))
}

pub(crate) fn json_data(
    ex: &mut Execution,
    list: &ValueList,
    object: &ObjRef,
    ctx: &mut XdtoCtx,
) -> Result<XdtoData, Exception> {
    let (bytes, revision) = list.serial_json_snapshot_admission(ex)?;
    ctx.account(ex, bytes)?;
    let source = Value::Object(object.clone());
    let retained = ex.measure_retained(ctx.read_scratch(0), |visit| visit.value(&source))?;
    list.serial_check_revision(ex, revision)?;
    ctx.account(ex, retained.saturating_mul(2))?;
    let mut snapshot = list.serial_snapshot(ex)?;
    list.serial_check_revision(ex, revision)?;
    let held = snapshot.items.len().saturating_mul(2).saturating_add(1);
    if let Err(error) = ctx.hold(ex, held) {
        ctx.release(held);
        return Err(error);
    }
    let result = (|| {
        let policy = snapshot
            .policy
            .take()
            .unwrap_or_else(|| TypeDescription::new(&[], None, None, None, None, ex.runtime()));
        let policy = ctx.to_data(ex, &new_type_description(policy))?;
        list.serial_check_revision(ex, revision)?;
        let last = i64::try_from(snapshot.next_id).map_err(|_| ex.out_of_memory())? - 1;
        let last = ctx.to_data(ex, &Value::from(last))?;
        let mut items = Vec::new();
        items
            .try_reserve_exact(snapshot.items.len())
            .map_err(|_| ex.out_of_memory())?;
        for item in &snapshot.items {
            let value = ctx.to_data(ex, &item.value)?;
            list.serial_check_revision(ex, revision)?;
            let mut props = Vec::new();
            props.try_reserve_exact(4).map_err(|_| ex.out_of_memory())?;
            props.push(field(ex, "value", value)?);
            if !item.presentation.is_empty() {
                let p = ctx.to_data(ex, &Value::String(item.presentation.clone()))?;
                props.push(field(ex, "presentation", p)?);
            }
            if item.check {
                let check = ctx.to_data(ex, &Value::from(1i64))?;
                props.push(field(ex, "checkState", check)?);
            }
            let id = i64::try_from(item.id).map_err(|_| ex.out_of_memory())?;
            let id = ctx.to_data(ex, &Value::from(id))?;
            props.push(field(ex, "id", id)?);
            items.push(XdtoData::Object {
                ty: QName::new(CORE, "ValueListItemType"),
                props,
            });
        }
        list.serial_check_revision(ex, revision)?;
        let mut props = Vec::new();
        props.try_reserve_exact(3).map_err(|_| ex.out_of_memory())?;
        props.push(field(ex, "valueType", policy)?);
        props.push(field(ex, "lastId", last)?);
        if !items.is_empty() {
            props.push(("item".into(), items));
        }
        Ok(XdtoData::Object {
            ty: QName::new(CORE, "ValueListType"),
            props,
        })
    })();
    ctx.release(held);
    result
}

fn object<'a>(d: &'a XdtoData, name: &str) -> Option<&'a Props> {
    match d {
        XdtoData::Object { ty, props } if ty.ns == CORE && ty.name == name => Some(props),
        _ => None,
    }
}
fn entry<'a>(props: &'a Props, name: &str) -> Option<&'a [XdtoData]> {
    props
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_slice())
}
fn one<'a>(props: &'a Props, name: &str) -> Option<&'a XdtoData> {
    match entry(props, name)? {
        [v] => Some(v),
        _ => None,
    }
}
fn shape(ex: &mut Execution, props: &Props, names: &[&str], many: &str) -> Result<bool, Exception> {
    for (i, (name, values)) in props.iter().enumerate() {
        ex.charge((1 + name.len() as u64).saturating_mul(1 + i as u64))?;
        if !names.contains(&name.as_str())
            || values.is_empty()
            || (name != many && values.len() != 1)
            || props[..i].iter().any(|(n, _)| n == name)
        {
            return Ok(false);
        }
    }
    Ok(true)
}
fn scalar<'a>(d: &'a XdtoData, ns: &str, name: &str) -> Option<(&'a str, &'a Value)> {
    match d {
        XdtoData::Value {
            ty,
            lexical,
            value,
            projection: None,
        } if ty.ns == ns && ty.name == name => Some((lexical, value)),
        _ => None,
    }
}
fn integer(ex: &mut Execution, d: &XdtoData) -> Result<Option<i64>, Exception> {
    let Some((text, Value::Number(n))) = scalar(d, XS, "decimal") else {
        return Ok(None);
    };
    ex.charge(1 + text.len() as u64 + n.size_hint_limbs())?;
    if text.len() > 20 {
        return Ok(None);
    }
    let Some(n) = n.to_i64() else { return Ok(None) };
    Ok((text.parse::<i64>().ok() == Some(n)).then_some(n))
}
fn enum_value(d: &XdtoData, ty: &str, values: &[&str]) -> bool {
    matches!(scalar(d, CORE, ty), Some((text, Value::Undefined)) if values.contains(&text))
}
fn validate_policy(ex: &mut Execution, d: &XdtoData) -> Result<bool, Exception> {
    let Some(p) = object(d, "TypeDescription") else {
        return Ok(false);
    };
    if !shape(
        ex,
        p,
        &[
            "Type",
            "NumberQualifiers",
            "StringQualifiers",
            "DateQualifiers",
            "BinaryDataQualifiers",
        ],
        "Type",
    )? {
        return Ok(false);
    }
    let types = entry(p, "Type").unwrap_or_default();
    for t in types {
        let Some((text, Value::Type(id))) = scalar(t, XS, "QName") else {
            return Ok(false);
        };
        ex.charge(1 + text.len() as u64)?;
        ex.reserve((text.len() as u64).saturating_mul(8).saturating_add(2048))?;
        if crate::xdto::type_from_qname(ex, text) != Some(*id) {
            return Ok(false);
        }
    }
    for (id, name) in [
        (bsl_vm::TypeId::NUMBER, "NumberQualifiers"),
        (bsl_vm::TypeId::STRING, "StringQualifiers"),
        (bsl_vm::TypeId::DATE, "DateQualifiers"),
        (
            bsl_vm::TypeId::of_catalog(crate::collections::types().binary),
            "BinaryDataQualifiers",
        ),
    ] {
        ex.charge(types.len() as u64 + 1)?;
        let present = types.iter().any(
            |t| matches!(scalar(t, XS, "QName"), Some((_, Value::Type(actual))) if *actual == id),
        );
        if present != one(p, name).is_some() {
            return Ok(false);
        }
    }
    for (name, fields) in [
        (
            "NumberQualifiers",
            &["Digits", "FractionDigits", "AllowedSign"][..],
        ),
        ("StringQualifiers", &["Length", "AllowedLength"][..]),
        ("BinaryDataQualifiers", &["Length", "AllowedLength"][..]),
        ("DateQualifiers", &["DateFractions"][..]),
    ] {
        let Some(q) = one(p, name) else { continue };
        let Some(q) = object(q, name) else {
            return Ok(false);
        };
        if !shape(ex, q, fields, "")? || q.len() != fields.len() {
            return Ok(false);
        }
        let valid = match name {
            "NumberQualifiers" => {
                let (Some(digits), Some(fraction), Some(sign)) = (
                    one(q, "Digits"),
                    one(q, "FractionDigits"),
                    one(q, "AllowedSign"),
                ) else {
                    return Ok(false);
                };
                let a = integer(ex, digits)?;
                let b = integer(ex, fraction)?;
                matches!((a,b), (Some(a), Some(b)) if (0..=32).contains(&a) && b >= 0 && b <= a)
                    && enum_value(sign, "AllowedSign", &["Any", "Nonnegative"])
            }
            "StringQualifiers" | "BinaryDataQualifiers" => {
                let (Some(length), Some(allowed)) = (one(q, "Length"), one(q, "AllowedLength"))
                else {
                    return Ok(false);
                };
                matches!(integer(ex, length)?, Some(n) if (0..=u32::MAX as i64).contains(&n))
                    && enum_value(allowed, "AllowedLength", &["Variable", "Fixed"])
            }
            _ => one(q, "DateFractions").is_some_and(|value| {
                enum_value(value, "DateFractions", &["Date", "Time", "DateTime"])
            }),
        };
        if !valid {
            return Ok(false);
        }
        let expected = match name {
            "NumberQualifiers" => bsl_vm::TypeId::NUMBER,
            "StringQualifiers" => bsl_vm::TypeId::STRING,
            "DateQualifiers" => bsl_vm::TypeId::DATE,
            _ => bsl_vm::TypeId::of_catalog(crate::collections::types().binary),
        };
        if !types.iter().any(
            |t| matches!(scalar(t, XS, "QName"), Some((_, Value::Type(id))) if *id == expected),
        ) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn validate(ex: &mut Execution, model: &Model, data: &XdtoData) -> CallResult<()> {
    let valid = validate_inner(ex, model, data)?;
    if valid {
        Ok(())
    } else {
        Err(CallError::from(bsl_vm::HostError::not_implemented(
            "СписокЗначений",
            "Недопустимые данные JSON",
        )))
    }
}
fn validate_inner(ex: &mut Execution, model: &Model, data: &XdtoData) -> Result<bool, Exception> {
    let Some(p) = object(data, "ValueListType") else {
        return Ok(false);
    };
    if !shape(ex, p, &["valueType", "lastId", "item"], "item")? {
        return Ok(false);
    }
    let Some(policy) = one(p, "valueType") else {
        return Ok(false);
    };
    if !validate_policy(ex, policy)? {
        return Ok(false);
    }
    let Some(last) = one(p, "lastId") else {
        return Ok(false);
    };
    let Some(last) = integer(ex, last)? else {
        return Ok(false);
    };
    if !(-1..i64::MAX).contains(&last) {
        return Ok(false);
    }
    let items = entry(p, "item").unwrap_or_default();
    for (index, item) in items.iter().enumerate() {
        ex.charge(1)?;
        let Some(q) = object(item, "ValueListItemType") else {
            return Ok(false);
        };
        if !shape(ex, q, &["value", "presentation", "checkState", "id"], "")? {
            return Ok(false);
        }
        let Some(value) = one(q, "value") else {
            return Ok(false);
        };
        if let XdtoData::Value { ty, .. } | XdtoData::Object { ty, .. } = value {
            ex.charge(
                (ty.ns.len() as u64)
                    .saturating_add(ty.name.len() as u64)
                    .saturating_add(1),
            )?;
            ex.reserve(
                (ty.ns.len() as u64)
                    .saturating_add(ty.name.len() as u64)
                    .saturating_mul(4)
                    .saturating_add(1024),
            )?;
        }
        let known = match value {
            XdtoData::Nil => true,
            XdtoData::Value { ty, .. } => {
                matches!(model.type_def(ty), Some(TypeDef::Value(_)))
                    && crate::xdto::is_descendant(model, ty, &crate::xdto::any_type())
            }
            XdtoData::Object { ty, .. } => {
                matches!(model.type_def(ty), Some(TypeDef::Object(_)))
                    && crate::xdto::is_descendant(model, ty, &crate::xdto::any_type())
            }
        };
        if !known {
            return Ok(false);
        }
        if let Some(presentation) = one(q, "presentation") {
            let Some((text, Value::String(s))) = scalar(presentation, XS, "string") else {
                return Ok(false);
            };
            ex.charge(1 + text.len() as u64 + s.len() as u64)?;
            if !text.encode_utf16().eq(s.units().iter().copied()) {
                return Ok(false);
            }
        }
        if let Some(check) = one(q, "checkState")
            && integer(ex, check)? != Some(1)
        {
            return Ok(false);
        }
        let Some(id) = one(q, "id") else {
            return Ok(false);
        };
        let Some(id) = integer(ex, id)? else {
            return Ok(false);
        };
        if id < 0 || id > last {
            return Ok(false);
        }
        for other in &items[..index] {
            ex.charge(1)?;
            let Some(other) = object(other, "ValueListItemType").and_then(|p| one(p, "id")) else {
                return Ok(false);
            };
            if integer(ex, other)? == Some(id) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub(crate) fn data_envelope(
    ex: &mut Execution,
    source: &Value,
    root: &XdtoData,
    builder: u64,
    prefix: u64,
) -> CallResult<Option<u64>> {
    use crate::xdto::ScalarProjection;
    if !contains_list(ex, root, builder.saturating_add(prefix))? {
        return Ok(None);
    }
    let mut stack = Vec::new();
    let mut objects = Vec::new();
    let mut bytes = builder
        .saturating_add(prefix)
        .saturating_add(std::mem::size_of::<XdtoData>() as u64);
    ex.reserve(bytes.saturating_add(256))?;
    stack.try_reserve_exact(1).map_err(|_| ex.out_of_memory())?;
    stack.push((root, 0usize));
    while let Some((node, depth)) = stack.pop() {
        ex.charge(1)?;
        if depth >= crate::serial::depth::MAX_VALUE_DEPTH {
            return Err(crate::serial::cycle_fatal(ex).into());
        }
        match node {
            XdtoData::Nil => {}
            XdtoData::Value {
                ty,
                lexical,
                value,
                projection,
            } => {
                bytes = bytes
                    .saturating_add(ty.ns.capacity() as u64)
                    .saturating_add(ty.name.capacity() as u64)
                    .saturating_add(lexical.capacity() as u64);
                if let Some(ScalarProjection::Read { source_text, .. }) = projection {
                    bytes = bytes.saturating_add(source_text.capacity() as u64);
                }
                bytes = bytes.saturating_add(match value {
                    Value::Number(n) => (n.backing_capacity_limbs() as u64)
                        .saturating_mul(8)
                        .saturating_add(n.size_hint_limbs().saturating_mul(64))
                        .saturating_add(256),
                    Value::String(s) => (s.backing_capacity_units() as u64).saturating_mul(2),
                    _ => 0,
                });
                ex.charge(1 + lexical.len() as u64 / 64)?;
                if matches!(value, Value::Object(_)) {
                    ex.reserve(
                        bytes.saturating_add(
                            ((stack
                                .capacity()
                                .saturating_add(objects.capacity())
                                .saturating_add(2)) as u64)
                                .saturating_mul(32),
                        ),
                    )?;
                    objects
                        .try_reserve_exact(1)
                        .map_err(|_| ex.out_of_memory())?;
                    objects.push(value);
                }
            }
            XdtoData::Object { ty, props } => {
                bytes = bytes
                    .saturating_add(ty.ns.capacity() as u64)
                    .saturating_add(ty.name.capacity() as u64)
                    .saturating_add(
                        (props.capacity() as u64).saturating_mul(std::mem::size_of::<(
                            String,
                            Vec<XdtoData>,
                        )>()
                            as u64),
                    );
                for (name, values) in props {
                    ex.charge(1 + name.len() as u64 / 64)?;
                    ex.charge(values.len() as u64)?;
                    bytes = bytes.saturating_add(name.capacity() as u64).saturating_add(
                        (values.capacity() as u64)
                            .saturating_mul(std::mem::size_of::<XdtoData>() as u64),
                    );
                    ex.reserve(
                        bytes.saturating_add(
                            ((stack
                                .capacity()
                                .saturating_add(objects.capacity())
                                .saturating_add(values.len())
                                .saturating_add(2)) as u64)
                                .saturating_mul(32),
                        ),
                    )?;
                    stack
                        .try_reserve_exact(values.len())
                        .map_err(|_| ex.out_of_memory())?;
                    stack.extend(values.iter().map(|v| (v, depth + 1)));
                }
            }
        }
        ex.reserve(bytes.saturating_add(
            ((stack.capacity().saturating_add(objects.capacity())) as u64).saturating_mul(32),
        ))?;
    }
    let retained = ex.measure_retained(bytes, |visit| {
        visit.work((objects.len() as u64).saturating_add(1))?;
        visit.value(source)?;
        for value in &objects {
            visit.value(value)?;
        }
        Ok(())
    })?;
    Ok(Some(bytes.saturating_add(retained).saturating_sub(prefix)))
}

fn contains_list(ex: &mut Execution, root: &XdtoData, builder: u64) -> Result<bool, Exception> {
    if !matches!(root, XdtoData::Object { .. }) {
        return Ok(false);
    }
    let mut stack = Vec::new();
    ex.reserve(builder.saturating_add(128))?;
    stack.try_reserve_exact(1).map_err(|_| ex.out_of_memory())?;
    stack.push((root, 0usize));
    while let Some((node, depth)) = stack.pop() {
        ex.charge(1)?;
        if depth >= crate::serial::depth::MAX_VALUE_DEPTH {
            return Err(crate::serial::cycle_fatal(ex));
        }
        if let XdtoData::Object { ty, props } = node {
            ex.charge(
                (ty.ns.len() as u64)
                    .saturating_add(ty.name.len() as u64)
                    .saturating_div(64)
                    .saturating_add(1),
            )?;
            if ty.ns == CORE && ty.name == "ValueListType" {
                return Ok(true);
            }
            for (_, values) in props {
                ex.charge((values.len() as u64).saturating_add(1))?;
                ex.reserve(
                    builder.saturating_add(
                        ((stack
                            .capacity()
                            .saturating_add(values.len())
                            .saturating_add(2)) as u64)
                            .saturating_mul(32),
                    ),
                )?;
                stack
                    .try_reserve_exact(values.len())
                    .map_err(|_| ex.out_of_memory())?;
                stack.extend(values.iter().map(|v| (v, depth + 1)));
            }
        }
    }
    Ok(false)
}
