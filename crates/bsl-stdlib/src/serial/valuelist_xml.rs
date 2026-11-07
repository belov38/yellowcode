use crate::collections::ValueList;
use crate::typedesc::{TypeDescription, new_type_description};
use crate::xdto::{XdtoCtx, XdtoData};
use bsl_lib::xdto::QName;
use bsl_prim::Str;
use bsl_vm::{Exception, Execution, HostError, Value};

const CORE: &str = "http://v8.1c.ru/8.1/data/core";
const XS: &str = "http://www.w3.org/2001/XMLSchema";

type Props = [(String, Vec<XdtoData>)];

fn object<'a>(data: &'a XdtoData, name: &str) -> Option<&'a Props> {
    match data {
        XdtoData::Object { ty, props } if ty.ns == CORE && ty.name == name => Some(props),
        _ => None,
    }
}

fn single<'a>(props: &'a Props, name: &str) -> Option<&'a XdtoData> {
    match props.iter().find(|(key, _)| key == name) {
        Some((_, values)) if values.len() == 1 => values.first(),
        _ => None,
    }
}

fn integer(data: &XdtoData, names: &[&str]) -> Option<i64> {
    match data {
        XdtoData::Value {
            ty,
            value: Value::Number(n),
            ..
        } if ty.ns == XS && names.contains(&ty.name.as_str()) => n.to_i64(),
        _ => None,
    }
}

fn shape(props: &Props, allowed: &[&str]) -> bool {
    props.iter().all(|(name, values)| {
        allowed.contains(&name.as_str())
            && !values.is_empty()
            && (name == "item" || values.len() == 1)
    }) && props
        .iter()
        .enumerate()
        .all(|(i, (name, _))| !props[..i].iter().any(|(other, _)| other == name))
}

pub(crate) fn policy_shape(data: &XdtoData) -> bool {
    let Some(props) = object(data, "TypeDescription") else {
        return false;
    };
    for (i, (name, values)) in props.iter().enumerate() {
        if values.is_empty() || props[..i].iter().any(|(other, _)| other == name) {
            return false;
        }
        if name == "Type" {
            if !values.iter().all(|value| matches!(value, XdtoData::Value { ty, .. } if ty.ns == XS && ty.name == "QName")) {
                return false;
            }
            continue;
        }
        let fields: &[(&str, &str, &str)] = match name.as_str() {
            "NumberQualifiers" => &[
                ("Digits", XS, "decimal"),
                ("FractionDigits", XS, "decimal"),
                ("AllowedSign", CORE, "AllowedSign"),
            ],
            "StringQualifiers" | "BinaryDataQualifiers" => &[
                ("Length", XS, "decimal"),
                ("AllowedLength", CORE, "AllowedLength"),
            ],
            "DateQualifiers" => &[("DateFractions", CORE, "DateFractions")],
            _ => return false,
        };
        if values.len() != 1 {
            return false;
        }
        let Some(child) = object(&values[0], name) else {
            return false;
        };
        if child.len() != fields.len() {
            return false;
        }
        for (field, ns, ty) in fields {
            if !matches!(single(child, field), Some(XdtoData::Value { ty: actual, .. }) if actual.ns == *ns && actual.name == *ty)
            {
                return false;
            }
        }
    }
    true
}

fn preflight(ex: &mut Execution, ctx: &mut XdtoCtx, root: &XdtoData) -> Result<(), Exception> {
    ctx.account(ex, 512)?;
    let mut stack = Vec::new();
    stack.try_reserve_exact(1).map_err(|_| ex.out_of_memory())?;
    stack.push(root);
    while let Some(node) = stack.pop() {
        ex.charge(1)?;
        match node {
            XdtoData::Object { props, .. } => {
                for (name, values) in props {
                    let bytes = values
                        .len()
                        .checked_mul(512)
                        .and_then(|n| n.checked_add(name.len()))
                        .ok_or_else(|| ex.out_of_memory())?;
                    ctx.account(ex, u64::try_from(bytes).map_err(|_| ex.out_of_memory())?)?;
                    ex.charge(u64::try_from(values.len()).map_err(|_| ex.out_of_memory())?)?;
                    stack
                        .try_reserve(values.len())
                        .map_err(|_| ex.out_of_memory())?;
                    stack.extend(values);
                }
            }
            XdtoData::Value { lexical, value, .. } => {
                let backing = match value {
                    Value::Number(number) => number.backing_capacity_limbs().checked_mul(8),
                    Value::String(text) => text.backing_capacity_units().checked_mul(2),
                    _ => Some(0),
                }
                .ok_or_else(|| ex.out_of_memory())?;
                let bytes = lexical
                    .len()
                    .checked_mul(32)
                    .and_then(|n| n.checked_add(backing))
                    .and_then(|n| n.checked_add(512))
                    .ok_or_else(|| ex.out_of_memory())?;
                ctx.account(ex, u64::try_from(bytes).map_err(|_| ex.out_of_memory())?)?;
            }
            XdtoData::Nil => ctx.account(ex, 512)?,
        }
    }
    Ok(())
}

pub(crate) fn read_xml_data(
    ex: &mut Execution,
    data: &XdtoData,
    ctx: &mut XdtoCtx,
) -> Result<Value, Exception> {
    read_data(ex, data, ctx, RestoreMode::Xml)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestoreMode {
    Xml,
    Json,
}

pub(crate) fn read_data(
    ex: &mut Execution,
    data: &XdtoData,
    ctx: &mut XdtoCtx,
    mode: RestoreMode,
) -> Result<Value, Exception> {
    preflight(ex, ctx, data)?;
    let props = object(data, "ValueListType").ok_or_else(|| ctx.mismatch(ex))?;
    if !shape(props, &["valueType", "lastId", "item"]) {
        return Err(ctx.mismatch(ex));
    }
    let policy_data = single(props, "valueType").ok_or_else(|| ctx.mismatch(ex))?;
    if !policy_shape(policy_data) {
        return Err(ctx.mismatch(ex));
    }
    let items = props
        .iter()
        .find(|(name, _)| name == "item")
        .map_or(&[][..], |(_, values)| values.as_slice());
    let last = match single(props, "lastId") {
        Some(data) => Some(
            integer(data, &["long", "decimal"])
                .filter(|n| *n >= -1 && *n < i64::MAX)
                .ok_or_else(|| ctx.mismatch(ex))?,
        ),
        None => None,
    };
    let mut ids = crate::collections::vec_with_capacity(ex, items.len() as u64)?;
    for item in items {
        let p = object(item, "ValueListItemType").ok_or_else(|| ctx.mismatch(ex))?;
        if !shape(p, &["value", "presentation", "checkState", "id"]) || single(p, "value").is_none()
        {
            return Err(ctx.mismatch(ex));
        }
        if let Some(presentation) = single(p, "presentation")
            && !matches!(presentation, XdtoData::Value { ty, value: Value::String(_), .. } if ty.ns == XS && ty.name == "string")
        {
            return Err(ctx.mismatch(ex));
        }
        if let Some(check) = single(p, "checkState")
            && !match mode {
                RestoreMode::Xml => matches!(integer(check, &["decimal"]), Some(0..=2)),
                RestoreMode::Json => integer(check, &["decimal"]) == Some(1),
            }
        {
            return Err(ctx.mismatch(ex));
        }
        let id = match single(p, "id") {
            Some(value) => Some(
                integer(value, &["long", "decimal"])
                    .filter(|n| *n >= 0)
                    .ok_or_else(|| ctx.mismatch(ex))?,
            ),
            None => None,
        };
        ids.push(id);
    }
    let next_id = match last {
        Some(last) if ids.iter().all(Option::is_some) => {
            let mut sorted = crate::collections::vec_with_capacity(ex, ids.len() as u64)?;
            for id in &ids {
                let id = id.ok_or_else(|| ctx.mismatch(ex))?;
                if id > last {
                    return Err(ctx.mismatch(ex));
                }
                sorted.push(id);
            }
            let n = sorted.len() as u64;
            ex.charge(n.saturating_mul(u64::from(n.max(1).ilog2()) + 1))?;
            sorted.sort_unstable();
            if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(ctx.mismatch(ex));
            }
            (last + 1) as u64
        }
        None if mode == RestoreMode::Xml && ids.iter().all(Option::is_none) => {
            let count = i64::try_from(items.len()).map_err(|_| ex.out_of_memory())?;
            if count == i64::MAX {
                return Err(ctx.mismatch(ex));
            }
            count as u64
        }
        _ => return Err(ctx.mismatch(ex)),
    };
    let policy = ctx.from_data(ex, policy_data)?;
    let policy_retained = ex.measure_retained(ctx.read_scratch(0), |visit| visit.value(&policy))?;
    let description = policy
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<TypeDescription>())
        .ok_or_else(|| ctx.mismatch(ex))?;
    let copy_envelope = policy_retained
        .checked_add(description.backing_bytes())
        .and_then(|bytes| bytes.checked_add(256))
        .ok_or_else(|| ex.out_of_memory())?;
    ctx.account(ex, copy_envelope)?;
    let policy = description.session_copy().map_err(|_| ex.out_of_memory())?;
    if mode == RestoreMode::Json {
        let list_bytes = ValueList::serial_restore_bytes(items.len(), policy.backing_bytes())
            .ok_or_else(|| ex.out_of_memory())?;
        ctx.account(ex, list_bytes)?;
    }
    let list = ValueList::serial_restore(ex, items.len(), policy, next_id)?;
    if mode == RestoreMode::Json {
        let extra = list
            .serial_restore_capacity()
            .checked_sub(items.len())
            .and_then(|n| n.checked_mul(std::mem::size_of::<std::rc::Rc<ValueList>>()))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| ex.out_of_memory())?;
        if extra != 0 {
            ctx.account(ex, extra)?;
        }
        let root = list.clone();
        ctx.push_json_root(ex, move |visit| visit.value(&Value::Object(root.clone())))?;
    }
    let mut held = 0u64;
    let result = (|| {
        for (index, item) in items.iter().enumerate() {
            let p = object(item, "ValueListItemType").ok_or_else(|| ctx.mismatch(ex))?;
            let value_data = single(p, "value").ok_or_else(|| ctx.mismatch(ex))?;
            let value = ctx.from_data(ex, value_data)?;
            let presentation = match single(p, "presentation") {
                Some(XdtoData::Value {
                    value: Value::String(text),
                    ..
                }) => text.clone(),
                None => Str::from(""),
                _ => return Err(ctx.mismatch(ex)),
            };
            let check = single(p, "checkState").and_then(|d| integer(d, &["decimal"])) == Some(1);
            let id = match ids[index] {
                Some(id) => id as u64,
                None => index as u64,
            };
            if mode == RestoreMode::Json {
                ctx.json_admit_extra(ex, 0, |visit| {
                    visit.value(&value)?;
                    visit.string(&presentation)
                })?;
            } else {
                let scratch = ctx.read_scratch(held);
                let retained = ex.measure_retained(scratch, |visit| {
                    visit.value(&Value::Object(list.clone()))?;
                    visit.value(&value)?;
                    visit.string(&presentation)
                })?;
                ctx.replace_read_live(ex, held, retained)?;
                held = retained;
            }
            list.serial_push(id, value, presentation, check);
        }
        Ok(Value::Object(list.clone()))
    })();
    if mode == RestoreMode::Json {
        ctx.pop_json_root();
    } else {
        ctx.release_read_live(held);
    }
    result
}

fn field(name: &str, value: XdtoData) -> (String, Vec<XdtoData>) {
    (name.into(), vec![value])
}

fn text_admission(ex: &mut Execution, ctx: &mut XdtoCtx, text: &Str) -> Result<(), Exception> {
    let bytes = text
        .len()
        .checked_mul(128)
        .and_then(|n| n.checked_add(2048))
        .ok_or_else(|| ex.out_of_memory())?;
    ctx.account(ex, u64::try_from(bytes).map_err(|_| ex.out_of_memory())?)?;
    let invalid = bsl_lib::xml::xml_invalid_pos(text.units(), false);
    if invalid != 0 {
        return Err(ctx.fail(crate::xml::bad_char_error(
            invalid as usize - 1,
            &crate::xml::shown_text(text.units()),
        )));
    }
    Ok(())
}

pub(crate) fn xml_data(
    ex: &mut Execution,
    list: &ValueList,
    ctx: &mut XdtoCtx,
) -> Result<XdtoData, Exception> {
    let snapshot_bytes = list.serial_snapshot_bytes(ex)?;
    ctx.account(ex, snapshot_bytes.saturating_add(2048))?;
    let mut snapshot = list.serial_snapshot(ex)?;
    let held = snapshot.items.len().saturating_mul(2).saturating_add(1);
    ctx.hold(ex, held)?;
    let result = (|| {
        if !matches!(snapshot.available, Value::Undefined)
            || snapshot
                .items
                .iter()
                .any(|item| !matches!(item.picture, Value::Undefined))
        {
            return Err(ex.error(HostError::not_implemented(
                "СписокЗначений",
                "Запись XML картинки или доступных значений",
            )));
        }
        ctx.account(ex, (snapshot.items.len() as u64).saturating_mul(4096))?;
        let policy = snapshot
            .policy
            .take()
            .unwrap_or_else(|| TypeDescription::new(&[], None, None, None, None, ex.runtime()));
        let value_type = ctx.to_data(ex, &new_type_description(policy))?;
        list.serial_check_revision(ex, snapshot.revision)?;
        let last_id = i64::try_from(snapshot.next_id).map_err(|_| ex.out_of_memory())? - 1;
        let last_id = ctx.to_data(ex, &Value::from(last_id))?;
        let mut items = Vec::new();
        items
            .try_reserve_exact(snapshot.items.len())
            .map_err(|_| ex.out_of_memory())?;
        for item in &snapshot.items {
            if let Value::String(text) = &item.value {
                text_admission(ex, ctx, text)?;
            }
            if !item.presentation.is_empty() {
                text_admission(ex, ctx, &item.presentation)?;
            }
            ctx.enter(ex, false)?;
            let value = ctx.to_data(ex, &item.value);
            ctx.leave();
            let value = value?;
            list.serial_check_revision(ex, snapshot.revision)?;
            let mut props = Vec::with_capacity(4);
            props.push(field("value", value));
            if !item.presentation.is_empty() {
                props.push(field(
                    "presentation",
                    ctx.to_data(ex, &Value::String(item.presentation.clone()))?,
                ));
            }
            if item.check {
                props.push(field("checkState", ctx.to_data(ex, &Value::from(1i64))?));
            }
            let id = i64::try_from(item.id).map_err(|_| ex.out_of_memory())?;
            props.push(field("id", ctx.to_data(ex, &Value::from(id))?));
            items.push(XdtoData::Object {
                ty: QName::new(CORE, "ValueListItemType"),
                props,
            });
        }
        list.serial_check_revision(ex, snapshot.revision)?;
        let mut props = vec![field("valueType", value_type), field("lastId", last_id)];
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
