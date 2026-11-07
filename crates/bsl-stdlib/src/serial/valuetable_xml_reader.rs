use crate::collections::ValueTable;
use crate::typedesc::TypeDescription;
use crate::xdto::{ScalarProjection, XdtoCtx, XdtoData};
use bsl_lib::xdto::QName;
use bsl_prim::{Str, fold_name};
use bsl_vm::{Exception, Execution, HostError, TypeId, Value};

const CORE: &str = "http://v8.1c.ru/8.1/data/core";
const XS: &str = "http://www.w3.org/2001/XMLSchema";
const MAX_COLUMNS: usize = (u32::MAX - 0x10000) as usize;

type Props = [(String, Vec<XdtoData>)];

pub(crate) fn is_root(d: &XdtoData) -> bool {
    object(d, "ValueTable").is_some()
}

fn object<'a>(data: &'a XdtoData, name: &str) -> Option<&'a Props> {
    match data {
        XdtoData::Object { ty, props } if ty.ns == CORE && ty.name == name => Some(props),
        _ => None,
    }
}

fn items<'a>(props: &'a Props, name: &str) -> &'a [XdtoData] {
    props
        .iter()
        .find(|(key, _)| key == name)
        .map_or(&[][..], |(_, values)| values.as_slice())
}

fn shape(props: &Props, allowed: &[&str], many: &[&str]) -> bool {
    props.iter().enumerate().all(|(i, (name, values))| {
        allowed.contains(&name.as_str())
            && !values.is_empty()
            && (values.len() == 1 || many.contains(&name.as_str()))
            && !props[..i].iter().any(|(other, _)| other == name)
    })
}

fn text<'a>(data: &'a XdtoData, names: &[&str]) -> Option<&'a Str> {
    match data {
        XdtoData::Value {
            ty,
            value: Value::String(s),
            ..
        } if ty.ns == XS && names.contains(&ty.name.as_str()) => Some(s),
        _ => None,
    }
}

fn unsupported(ex: &mut Execution) -> Exception {
    ex.error(HostError::not_implemented(
        "ТаблицаЗначений",
        "Чтение XML значения ячейки такого типа",
    ))
}

fn bytes_of<T>(ex: &Execution, count: usize) -> Result<u64, Exception> {
    count
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| ex.out_of_memory())
}

fn vector<T>(ex: &mut Execution, ctx: &mut XdtoCtx, count: usize) -> Result<Vec<T>, Exception> {
    ex.charge(count as u64)?;
    let bytes = bytes_of::<T>(ex, count)?;
    ctx.account(ex, bytes)?;
    let mut v = Vec::new();
    v.try_reserve_exact(count).map_err(|_| ex.out_of_memory())?;
    Ok(v)
}

fn fold(ex: &mut Execution, ctx: &mut XdtoCtx, name: &Str) -> Result<String, Exception> {
    let bytes = (name.len() as u64)
        .checked_mul(8)
        .and_then(|n| n.checked_add(64))
        .ok_or_else(|| ex.out_of_memory())?;
    ctx.account(ex, bytes)?;
    ex.charge(name.len() as u64 / 32 + 1)?;
    Ok(fold_name(&name.to_string_lossy()))
}

pub(crate) fn projection_bytes(ex: &mut Execution, root: &XdtoData) -> Result<u64, Exception> {
    const NODE: usize = std::mem::size_of::<XdtoData>();
    const SLOT: usize = std::mem::size_of::<&XdtoData>();
    const PROP: usize = std::mem::size_of::<(String, Vec<XdtoData>)>();
    fn add(ex: &Execution, total: u64, bytes: usize) -> Result<u64, Exception> {
        u64::try_from(bytes)
            .ok()
            .and_then(|b| total.checked_add(b))
            .ok_or_else(|| ex.out_of_memory())
    }
    fn mul(ex: &Execution, count: usize, size: usize) -> Result<usize, Exception> {
        count.checked_mul(size).ok_or_else(|| ex.out_of_memory())
    }
    ex.charge(1)?;
    let mut total = NODE as u64;
    let initial = mul(ex, 4, SLOT)?;
    ex.reserve(add(ex, total, initial)?)?;
    let mut stack: Vec<&XdtoData> = Vec::new();
    stack.try_reserve_exact(4).map_err(|_| ex.out_of_memory())?;
    stack.push(root);
    while let Some(node) = stack.pop() {
        ex.charge(1)?;
        match node {
            XdtoData::Nil => {}
            XdtoData::Value {
                projection,
                ty,
                lexical,
                value,
            } => {
                total = add(ex, total, ty.ns.capacity())?;
                total = add(ex, total, ty.name.capacity())?;
                total = add(ex, total, lexical.capacity())?;
                if let Some(ScalarProjection::Read { source_text, .. }) = projection {
                    total = add(ex, total, source_text.capacity())?;
                }
                total = match value {
                    Value::Number(n) => add(ex, total, mul(ex, n.backing_capacity_limbs(), 4)?)?,
                    Value::String(s) => add(ex, total, mul(ex, s.backing_capacity_units(), 2)?)?,
                    Value::Object(_) => {
                        let own = ex.measure_retained(total, |visit| visit.value(value))?;
                        total.checked_add(own).ok_or_else(|| ex.out_of_memory())?
                    }
                    _ => total,
                };
            }
            XdtoData::Object { ty, props } => {
                total = add(ex, total, ty.ns.capacity())?;
                total = add(ex, total, ty.name.capacity())?;
                total = add(ex, total, mul(ex, props.capacity(), PROP)?)?;
                ex.charge(props.len() as u64)?;
                for (name, values) in props {
                    total = add(ex, total, name.capacity())?;
                    total = add(ex, total, mul(ex, values.capacity(), NODE)?)?;
                    ex.charge(values.len() as u64)?;
                    let need = stack
                        .len()
                        .checked_add(values.len())
                        .ok_or_else(|| ex.out_of_memory())?;
                    if need > stack.capacity() {
                        let grown = need.max(stack.capacity().saturating_mul(2));
                        let both = mul(ex, grown.saturating_add(stack.capacity()), SLOT)?;
                        ex.reserve(add(ex, total, both)?)?;
                        stack
                            .try_reserve(values.len())
                            .map_err(|_| ex.out_of_memory())?;
                    }
                    stack.extend(values.iter());
                }
            }
        }
        let held = mul(ex, stack.capacity(), SLOT)?;
        ex.reserve(add(ex, total, held)?)?;
    }
    Ok(total)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Any,
    One(TypeId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Nil,
    Scalar { ty: TypeId, explicit: bool },
    Null,
    Type,
    Codec(TypeId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Restore {
    AsRead,
    Retype(&'static str),
}

struct ColumnPlan<'a> {
    name: &'a Str,
    value_type: &'a XdtoData,
    title: Option<&'a Str>,
    width: i64,
    expect: Expect,
}

fn scalar_name(t: TypeId) -> Option<&'static str> {
    match t {
        TypeId::NUMBER => Some("decimal"),
        TypeId::STRING => Some("string"),
        TypeId::BOOL => Some("boolean"),
        TypeId::DATE => Some("dateTime"),
        _ => None,
    }
}

fn builtin_xml_reader(ty: &QName) -> bool {
    (ty.ns == CORE && matches!(ty.name.as_str(), "ValueTable" | "ValueListType"))
        || (ty.ns == "http://v8.1c.ru/8.1/data/txtedt" && ty.name == "TextDocument")
        || (ty.ns == "http://v8.1c.ru/8.1/data/enterprise" && ty.name == "Bound")
}

fn classify(data: &XdtoData) -> Option<Cell> {
    let (ty, projection) = match data {
        XdtoData::Nil => return Some(Cell::Nil),
        XdtoData::Value { ty, projection, .. } => (ty, projection.as_ref()),
        XdtoData::Object { ty, .. } => (ty, None),
    };
    if matches!(data, XdtoData::Value { .. }) {
        if ty.ns == XS {
            let t = match ty.name.as_str() {
                "decimal" => TypeId::NUMBER,
                "string" => TypeId::STRING,
                "boolean" => TypeId::BOOL,
                "dateTime" => TypeId::DATE,
                _ => return None,
            };
            let explicit = !matches!(
                projection,
                Some(ScalarProjection::Read {
                    explicit_type: false,
                    ..
                })
            );
            return Some(Cell::Scalar { ty: t, explicit });
        }
        if ty.ns == CORE && ty.name == "Null" {
            return Some(Cell::Null);
        }
        if ty.ns == CORE && ty.name == "Type" {
            return Some(Cell::Type);
        }
    }
    if builtin_xml_reader(ty) {
        return None;
    }
    XdtoCtx::caller_codec_type(ty).map(|idx| Cell::Codec(TypeId::of_catalog(idx)))
}

fn column_expect(
    ex: &mut Execution,
    ctx: &mut XdtoCtx,
    value_type: &XdtoData,
) -> Result<Expect, Exception> {
    let XdtoData::Object { props, .. } = value_type else {
        return Err(ctx.mismatch(ex));
    };
    let mut one: Option<TypeId> = None;
    let mut several = false;
    for entry in items(props, "Type") {
        let XdtoData::Value { lexical, .. } = entry else {
            return Err(ctx.mismatch(ex));
        };
        let bytes = (lexical.len() as u64)
            .checked_mul(4)
            .and_then(|n| n.checked_add(512))
            .ok_or_else(|| ex.out_of_memory())?;
        ctx.json_admit_extra(ex, bytes, |_| Ok(()))?;
        let Some(t) = crate::xdto::type_from_qname(ex, lexical) else {
            let e = ctx.conversion_error(ex, lexical, &crate::xdto::core("TypeDescription"));
            return Err(e);
        };
        match one {
            None => one = Some(t),
            Some(first) if first != t => several = true,
            Some(_) => {}
        }
    }
    Ok(match one {
        Some(t) if !several => Expect::One(t),
        _ => Expect::Any,
    })
}

fn check_cell(
    ex: &mut Execution,
    ctx: &mut XdtoCtx,
    data: &XdtoData,
    expect: Expect,
) -> Result<Restore, Exception> {
    let Some(cell) = classify(data) else {
        return Err(unsupported(ex));
    };
    let Expect::One(t) = expect else {
        return Ok(Restore::AsRead);
    };
    let restore = match cell {
        Cell::Nil => return Err(ctx.mismatch(ex)),
        Cell::Scalar {
            explicit: false, ..
        } => match scalar_name(t) {
            Some(name) => Restore::Retype(name),
            None => return Err(unsupported(ex)),
        },
        Cell::Scalar { ty, explicit: true } if ty == t => Restore::AsRead,
        Cell::Null if t == TypeId::NULL => Restore::AsRead,
        Cell::Type if t == TypeId::TYPE => Restore::AsRead,
        Cell::Codec(c) if c == t => Restore::AsRead,
        _ => return Err(ctx.mismatch(ex)),
    };
    if let Restore::Retype(name) = restore {
        retype(ex, ctx, data, name)?;
    }
    Ok(restore)
}

fn retype(
    ex: &mut Execution,
    ctx: &mut XdtoCtx,
    data: &XdtoData,
    name: &str,
) -> Result<XdtoData, Exception> {
    let source = match data {
        XdtoData::Value {
            projection: Some(ScalarProjection::Read { source_text, .. }),
            ..
        } => source_text,
        _ => return Err(ctx.mismatch(ex)),
    };
    let bytes = (source.capacity() as u64)
        .checked_mul(8)
        .and_then(|n| n.checked_add(1024))
        .ok_or_else(|| ex.out_of_memory())?;
    ctx.json_admit_extra(ex, bytes, |_| Ok(()))?;
    ctx.scalar_from_text(ex, &QName::new(XS, name), source)
}

fn index_members(ex: &mut Execution, indexes: &[XdtoData]) -> Result<usize, Exception> {
    ex.charge(indexes.len() as u64)?;
    let mut total = 0usize;
    for index in indexes {
        let n = object(index, "ValueTableIndex").map_or(0, |p| items(p, "column").len());
        total = total.checked_add(n).ok_or_else(|| ex.out_of_memory())?;
    }
    Ok(total)
}

struct Plan<'a> {
    columns: Vec<ColumnPlan<'a>>,
    members: Vec<u32>,
    ends: Vec<usize>,
    rows: &'a [XdtoData],
}

fn validate<'a>(
    ex: &mut Execution,
    ctx: &mut XdtoCtx,
    data: &'a XdtoData,
) -> Result<Plan<'a>, Exception> {
    let props = object(data, "ValueTable").ok_or_else(|| ctx.mismatch(ex))?;
    if !shape(
        props,
        &["column", "index", "row"],
        &["column", "index", "row"],
    ) {
        return Err(ctx.mismatch(ex));
    }
    let columns = items(props, "column");
    let indexes = items(props, "index");
    let rows = items(props, "row");
    if columns.len() >= MAX_COLUMNS {
        return Err(ctx.mismatch(ex));
    }
    let mut plans = vector::<ColumnPlan<'a>>(ex, ctx, columns.len())?;
    for column in columns {
        let p = object(column, "ValueTableColumn").ok_or_else(|| ctx.mismatch(ex))?;
        if !shape(p, &["Name", "ValueType", "Title", "Width"], &[]) {
            return Err(ctx.mismatch(ex));
        }
        let name = match items(p, "Name") {
            [name] => text(name, &["string", "NMTOKEN"]),
            _ => None,
        }
        .ok_or_else(|| ctx.mismatch(ex))?;
        let value_type = match items(p, "ValueType") {
            [value_type] if super::valuelist_xml::policy_shape(value_type) => value_type,
            _ => return Err(ctx.mismatch(ex)),
        };
        let title = match items(p, "Title") {
            [] => None,
            [title] => Some(text(title, &["string"]).ok_or_else(|| ctx.mismatch(ex))?),
            _ => return Err(ctx.mismatch(ex)),
        };
        let width = match items(p, "Width") {
            [] => 0,
            [
                XdtoData::Value {
                    ty,
                    value: Value::Number(n),
                    ..
                },
            ] if ty.ns == XS && matches!(ty.name.as_str(), "decimal" | "nonNegativeInteger") => n
                .to_i64()
                .filter(|w| *w >= 0)
                .ok_or_else(|| ctx.mismatch(ex))?,
            _ => return Err(ctx.mismatch(ex)),
        };
        let expect = column_expect(ex, ctx, value_type)?;
        plans.push(ColumnPlan {
            name,
            value_type,
            title,
            width,
            expect,
        });
    }
    {
        let mut names = vector::<Str>(ex, ctx, plans.len())?;
        names.extend(plans.iter().map(|c| c.name.clone()));
        if !ValueTable::serial_names_valid(ex, &names)? {
            return Err(ctx.mismatch(ex));
        }
    }
    let total = index_members(ex, indexes)?;
    let mut members = vector::<u32>(ex, ctx, total)?;
    let mut ends = vector::<usize>(ex, ctx, indexes.len())?;
    if !indexes.is_empty() {
        let mut keys = vector::<String>(ex, ctx, plans.len())?;
        for plan in &plans {
            let key = fold(ex, ctx, plan.name)?;
            keys.push(key);
        }
        for index in indexes {
            let p = object(index, "ValueTableIndex").ok_or_else(|| ctx.mismatch(ex))?;
            if !shape(p, &["column"], &["column"]) {
                return Err(ctx.mismatch(ex));
            }
            let names = items(p, "column");
            if names.is_empty() {
                return Err(ctx.mismatch(ex));
            }
            let start = members.len();
            for name in names {
                let name = text(name, &["string", "NMTOKEN"]).ok_or_else(|| ctx.mismatch(ex))?;
                let key = fold(ex, ctx, name)?;
                ex.charge(keys.len() as u64 + 1)?;
                let position = keys
                    .iter()
                    .position(|k| *k == key)
                    .and_then(|i| u32::try_from(i).ok())
                    .ok_or_else(|| ctx.mismatch(ex))?;
                ex.charge((members.len() - start) as u64 + 1)?;
                if members[start..].contains(&position) {
                    return Err(ctx.mismatch(ex));
                }
                members.push(position);
            }
            ends.push(members.len());
        }
    }
    ex.charge(rows.len() as u64)?;
    for row in rows {
        let p = object(row, "ValueTableRow").ok_or_else(|| ctx.mismatch(ex))?;
        if !shape(p, &["Value"], &["Value"]) {
            return Err(ctx.mismatch(ex));
        }
        let cells = items(p, "Value");
        ex.charge(cells.len() as u64 + 1)?;
        if plans.is_empty() {
            if cells.iter().any(|cell| {
                !matches!(
                    classify(cell),
                    Some(Cell::Nil | Cell::Scalar { .. } | Cell::Null | Cell::Type)
                )
            }) {
                return Err(unsupported(ex));
            }
            continue;
        }
        if cells.len() != plans.len() {
            return Err(ctx.mismatch(ex));
        }
        for (cell, plan) in cells.iter().zip(&plans) {
            check_cell(ex, ctx, cell, plan.expect)?;
        }
    }
    Ok(Plan {
        columns: plans,
        members,
        ends,
        rows,
    })
}

pub(crate) fn read_xml_data(
    ex: &mut Execution,
    data: &XdtoData,
    ctx: &mut XdtoCtx,
) -> Result<Value, Exception> {
    let plan = validate(ex, ctx, data)?;
    ctx.json_admit_extra(ex, ValueTable::serial_new_bytes(), |_| Ok(()))?;
    let result = ValueTable::serial_new(ex)?;
    let root = result.clone();
    ctx.push_json_root(ex, move |visit| visit.value(&root))?;
    let entered = ctx.enter(ex, true);
    let restored = entered.and_then(|()| {
        let r = restore(ex, ctx, &plan, &result);
        ctx.leave();
        r
    });
    let restored = restored.and_then(|()| ctx.account(ex, 0));
    ctx.pop_json_root();
    restored.map(|()| result)
}

fn restore(
    ex: &mut Execution,
    ctx: &mut XdtoCtx,
    plan: &Plan<'_>,
    result: &Value,
) -> Result<(), Exception> {
    let table = result
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<ValueTable>())
        .ok_or_else(|| ctx.mismatch(ex))?;
    let columns = plan.columns.len();
    let rows = plan.rows.len();
    let indexes = plan.ends.len();
    let entries = columns
        .checked_add(rows)
        .and_then(|n| n.checked_add(indexes))
        .ok_or_else(|| ex.out_of_memory())?;
    let entries = bytes_of::<usize>(ex, entries)?;
    ctx.json_admit_extra(ex, entries, |_| Ok(()))?;
    let scratch = ctx.read_scratch_outside_roots(ex)?;
    table.serial_reserve_entries(ex, columns, rows, indexes, result, scratch)?;
    for (id, column) in plan.columns.iter().enumerate() {
        let value_type = ctx.from_data(ex, column.value_type)?;
        let description = value_type
            .as_object()
            .and_then(|o| o.as_any().downcast_ref::<TypeDescription>())
            .ok_or_else(|| ctx.mismatch(ex))?;
        let agrees = match (column.expect, description.types()) {
            (Expect::One(t), [only]) => *only == t,
            (Expect::One(_), _) => false,
            (Expect::Any, types) => types.len() != 1,
        };
        if !agrees {
            return Err(ctx.mismatch(ex));
        }
        let title = column.title.cloned().unwrap_or_else(Str::new);
        let bytes = ValueTable::serial_column_bytes(column.name.len(), title.len())
            .ok_or_else(|| ex.out_of_memory())?;
        ctx.json_admit_extra(ex, bytes, |visit| visit.value(&value_type))?;
        let id = u32::try_from(id).map_err(|_| ex.out_of_memory())?;
        table.serial_restore_column(
            ex,
            id,
            column.name.clone(),
            value_type,
            title,
            column.width,
        )?;
    }
    let next_column = u32::try_from(columns).map_err(|_| ex.out_of_memory())?;
    let next_row = u64::try_from(rows).map_err(|_| ex.out_of_memory())?;
    table.serial_restore_counters(next_column, next_row);
    let mut start = 0;
    for &end in &plan.ends {
        let ids = &plan.members[start..end];
        let bytes = bytes_of::<usize>(ex, ids.len())?
            .checked_mul(2)
            .and_then(|n| n.checked_add(512))
            .ok_or_else(|| ex.out_of_memory())?;
        ctx.json_admit_extra(ex, bytes, |_| Ok(()))?;
        table.serial_restore_index(ex, ids)?;
        start = end;
    }
    let row_bytes = ValueTable::serial_row_bytes(columns).ok_or_else(|| ex.out_of_memory())?;
    for (id, row) in plan.rows.iter().enumerate() {
        ctx.json_admit_extra(ex, row_bytes, |_| Ok(()))?;
        let id = u64::try_from(id).map_err(|_| ex.out_of_memory())?;
        table.serial_restore_row(ex, id, columns != 0)?;
        if columns == 0 {
            continue;
        }
        let cells = object(row, "ValueTableRow")
            .map(|p| items(p, "Value"))
            .ok_or_else(|| ctx.mismatch(ex))?;
        for (position, (cell, column)) in cells.iter().zip(&plan.columns).enumerate() {
            let value = match check_kind(ex, ctx, cell, column.expect)? {
                Restore::AsRead => ctx.from_data(ex, cell)?,
                Restore::Retype(name) => {
                    let data = retype(ex, ctx, cell, name)?;
                    ctx.from_data(ex, &data)?
                }
            };
            if let Expect::One(t) = column.expect
                && value.type_id() != t
            {
                return Err(ctx.mismatch(ex));
            }
            ctx.json_admit_extra(ex, 0, |visit| visit.value(&value))?;
            table.serial_restore_cell(ex, position, value)?;
        }
    }
    Ok(())
}

fn check_kind(
    ex: &mut Execution,
    ctx: &mut XdtoCtx,
    data: &XdtoData,
    expect: Expect,
) -> Result<Restore, Exception> {
    match (expect, classify(data)) {
        (
            Expect::One(t),
            Some(Cell::Scalar {
                explicit: false, ..
            }),
        ) => scalar_name(t)
            .map(Restore::Retype)
            .ok_or_else(|| unsupported(ex)),
        (_, Some(_)) => Ok(Restore::AsRead),
        (_, None) => Err(ctx.mismatch(ex)),
    }
}
