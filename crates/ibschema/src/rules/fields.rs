use md_model::Child;

use super::Names;
use super::ctx::Ctx;
use super::in_object;
use super::types::{self, RefTarget};
use crate::error::{Error, Result};
use crate::logical::{Field, LType, TypeCode, VAR};
use crate::names::{StdField, field as f, purpose};

pub(crate) fn field(name: impl Into<String>, nullable: bool, types: Vec<LType>) -> Field {
    Field::new(name, nullable, types)
}

fn one(name: &str, t: LType) -> Field {
    field(name, false, vec![t])
}

pub(crate) fn boolean(name: &str) -> Field {
    one(name, LType::new(TypeCode::L, 0, 0))
}

pub(crate) fn datetime(name: &str) -> Field {
    one(name, LType::new(TypeCode::T, 0, 0))
}

pub(crate) fn numeric(name: &str, digits: u32, fraction: u32) -> Field {
    one(name, LType::new(TypeCode::N, digits, fraction))
}

pub(crate) fn binary(name: &str, len: u32) -> Field {
    one(name, LType::new(TypeCode::B, len, 0))
}

pub(crate) fn string(name: &str, len: u32) -> Field {
    one(name, LType::new(TypeCode::S, VAR | len, 0))
}

pub(crate) fn id(own: &str) -> Field {
    one(f::ID, LType::reference(own, 2))
}

pub(crate) fn id_of(table: &str) -> Field {
    one(f::ID, LType::reference(table, 3))
}

pub(crate) fn version() -> Field {
    one(f::VERSION, LType::new(TypeCode::V, 0, 0))
}

pub(crate) fn marked() -> Field {
    boolean(f::MARKED)
}

pub(crate) fn predefined_id() -> Field {
    binary(f::PREDEFINED_ID, 16)
}

pub(crate) fn parent_id(own: &str) -> Field {
    one(f::PARENT_ID, LType::reference(own, 3))
}

pub(crate) fn folder() -> Field {
    boolean(f::FOLDER)
}

fn of_class(name: &str, c: &types::Class, what: &str, cx: &Ctx) -> Result<Field> {
    let t = types::logical_types(std::slice::from_ref(c), false)
        .map_err(|e| in_object(&format!("{}.{what}", cx.full_name), e))?;
    Ok(field(name, false, t))
}

pub(crate) fn recorder(cx: &Ctx, names: &mut Names) -> Result<Field> {
    let targets = cx
        .recorders
        .iter()
        .map(|d| types::main_target(d, names))
        .collect::<Result<Vec<_>>>()?;
    Ok(reference(f::RECORDER, &targets, RefForm::T, false))
}

pub(crate) fn code(cx: &Ctx) -> Result<Field> {
    of_class(f::CODE, &cx.code, "Code", cx)
}

pub(crate) fn description(len: u32) -> Field {
    string(f::DESCRIPTION, len)
}

pub(crate) fn date_time() -> Field {
    datetime(f::DATE_TIME)
}

pub(crate) fn number_prefix() -> Field {
    datetime(f::NUMBER_PREFIX)
}

pub(crate) fn number(cx: &Ctx) -> Result<Field> {
    of_class(f::NUMBER, &cx.number, "Number", cx)
}

pub(crate) fn posted() -> Field {
    boolean(f::POSTED)
}

pub(crate) fn period() -> Field {
    datetime(f::PERIOD)
}

pub(crate) fn active() -> Field {
    boolean(f::ACTIVE)
}

pub(crate) fn line_no(m: Option<u32>, digits: u32) -> Field {
    let name = match m {
        Some(m) => StdField::LineNo.numbered(m),
        None => f::LINE_NO.to_string(),
    };
    numeric(&name, digits, 0)
}

pub(crate) fn record_kind() -> Field {
    numeric(f::RECORD_KIND, 1, 0)
}

pub(crate) fn splitter() -> Field {
    numeric(f::SPLITTER, 10, 0)
}

pub(crate) fn message_no() -> Field {
    field(f::MESSAGE_NO, true, vec![LType::new(TypeCode::N, 10, 0)])
}

pub(crate) fn pd_initialized() -> Field {
    boolean(f::PD_INITIALIZED)
}

pub(crate) fn enum_order() -> Field {
    numeric(f::ENUM_ORDER, 10, 0)
}

pub(crate) fn value_type() -> Field {
    field(f::TYPE, true, vec![LType::new(TypeCode::B, VAR, 0)])
}

pub(crate) fn node() -> Field {
    reference(f::NODE, &[], RefForm::T, false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefForm {
    E,
    T,
}

pub(crate) fn reference(name: &str, targets: &[RefTarget], form: RefForm, nullable: bool) -> Field {
    let types = match (targets, form) {
        ([t], _) => vec![LType::reference(t.table.as_str(), 3)],
        (_, RefForm::E) => vec![LType::new(TypeCode::E, 0, 0), LType::reference("", 4)],
        (_, RefForm::T) => vec![LType::reference("", 4)],
    };
    field(name, nullable, types)
}

pub(crate) fn item_name(it: &Child, names: &mut Names) -> Result<String> {
    Ok(format!(
        "{}{}",
        purpose::FLD,
        names.number(it.id, purpose::FLD)?
    ))
}

pub(crate) fn item(
    cx: &Ctx,
    it: &Child,
    md_name: &str,
    nullable: bool,
    totals: bool,
    names: &mut Names,
) -> Result<Field> {
    let td = it
        .type_desc()
        .ok_or_else(|| Error::Unsupported(format!("{md_name}: no type")))?;
    let classes = types::classes(cx, td, md_name, names)?;
    let t = types::logical_types(&classes, totals).map_err(|e| in_object(md_name, e))?;
    Ok(field(item_name(it, names)?, nullable, t))
}
