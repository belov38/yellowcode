use super::accounting_turnovers::{
    AccountFilterSpec, AccountingTurnoversRecorderL, find_index_with_seps,
};
use super::accounting_turnovers_payload::{TurnoversValueCheckL, value_check_with_later};
use super::layout::{R, Row, internal, p16};
use super::rel::Lower;
use super::running::RunningBalanceL;
use super::{
    AccountingChartLayoutL, AggL, BalanceAreaExactL, BalanceAreaFieldL, BalanceAreasL, BalanceDimL,
    BalanceInputsL, BalanceResourceL, BalanceTableAreasL, Dec, PExpr, RegisterRowsKindL,
    RegisterRowsL, RelL,
};
use crate::ir::{
    ColSel, Expr, ExprKind, RegisterVtS, Rel, SrcId, SrcKind, StmtS, raw_layout as raw,
};
use crate::schema::{
    AccountingFlagBinding, Complement, FieldRole, PeriodField, Periodicity, QFieldId, QTableId,
    QuerySchema, StdRole, StorageBinding, TableKind, VtKind,
};
use crate::{DateQ, ExecError, NumQ, QType, QValue};
use brace::Guid;
use ibschema::logical::{LType, TypeCode};
use ibschema::names::{Side, StdField, role};
use ibschema::storage::{FieldId, IndexId, StorageMap, TableId, TableInfo};
use mdschema::MdKind;
use query_syntax::{Func, Kw, ast::CmpOp};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingBalanceAndTurnoversAccRgEdL {
    pub table: TableId,
    pub main_period: FieldId,
    pub main_recorder: FieldId,
    pub main_line: FieldId,
    pub recorder_tags: Vec<u32>,
    pub section_dim_kind: FieldId,
    pub kind_ref_table: u32,
    pub ed_period: FieldId,
    pub ed_recorder: FieldId,
    pub ed_line: FieldId,
    pub ed_correspond: Option<FieldId>,
    pub ed_kind: FieldId,
    pub ed_value: FieldId,
    pub main_by_period: Option<IndexId>,
    pub ed_by_period: Option<IndexId>,
    pub ed_by_recorder: Option<(IndexId, bool)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingBalanceAndTurnoversExtSlotL {
    pub ordinal: u8,
    pub output: QFieldId,
    pub values: Option<[FieldId; 2]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingBalanceAndTurnoversExtL {
    pub section: TableId,
    pub owner: FieldId,
    pub ordinal: FieldId,
    pub turnover_only: FieldId,
    pub dim_kind: Option<FieldId>,
    pub kind_ref_table: Option<u32>,
    pub requested_kinds: Option<Vec<[u8; 16]>>,
    pub physical_values: Vec<[FieldId; 2]>,
    pub ext_flag_fields: Vec<FieldId>,
    pub slots: Vec<AccountingBalanceAndTurnoversExtSlotL>,
    pub value_check: TurnoversValueCheckL,
    pub acc_rg_ed: Option<Box<AccountingBalanceAndTurnoversAccRgEdL>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingBalanceAndTurnoversL {
    pub inputs: BalanceInputsL,
    pub areas: BalanceAreasL,
    pub chart_input: Option<Box<RelL>>,
    pub chart_layout: Option<AccountingChartLayoutL>,
    pub policy_fields: Vec<FieldId>,
    pub secured: bool,
    pub dims: Vec<BalanceDimL>,
    pub ext_dims: Option<AccountingBalanceAndTurnoversExtL>,
    pub resources: Vec<BalanceResourceL>,
    pub bounds: [Option<PExpr>; 2],
    pub period: FieldId,
    pub recorder: Option<AccountingTurnoversRecorderL>,
    pub line: Option<FieldId>,
    pub periodic: bool,
    pub need_opening: bool,
    pub need_turnovers: bool,
    pub correspondence: bool,
    pub record_kind: Option<FieldId>,
    pub account_filter: Option<AccountFilterSpec>,
}

impl AccountingBalanceAndTurnoversL {
    pub fn ext_count(&self) -> usize {
        self.ext_dims.as_ref().map_or(0, |e| e.slots.len())
    }
}

fn fatal(what: impl std::fmt::Display) -> ExecError {
    ExecError::Fatal(format!("accounting BalanceAndTurnovers: {what}"))
}

fn later(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q6c" }
}

fn areas_later() -> ExecError {
    ExecError::Later {
        what: "Q6c-AccountingBalanceAndTurnoversAreas",
        stage: "Q7c",
    }
}

pub(crate) fn is_accounting_balance_and_turnovers(schema: &QuerySchema, table: QTableId) -> bool {
    let t = schema.table(table);
    t.kind == TableKind::Virtual(VtKind::BalanceAndTurnovers)
        && t.owner
            .is_some_and(|o| schema.table(o).kind == TableKind::Object(MdKind::AccountingRegister))
}

pub(crate) fn accounting_bt_chart(schema: &QuerySchema, table: QTableId) -> R<QTableId> {
    let vt = schema.table(table);
    if !is_accounting_balance_and_turnovers(schema, table) {
        return Err(fatal("not an accounting BalanceAndTurnovers source"));
    }
    let mut fields = vt
        .fields
        .iter()
        .filter(|f| f.role == FieldRole::Std(StdRole::Account));
    let field = fields.next().ok_or_else(|| fatal("missing Account"))?;
    if fields.next().is_some() || field.ty.atom_count() != 1 || field.ty.refs.len() != 1 {
        return Err(fatal("ambiguous account reference target"));
    }
    let atom = *field
        .ty
        .refs
        .first()
        .ok_or_else(|| fatal("missing account reference atom"))?;
    let chart = schema.ref_info(atom).table;
    if schema.table(chart).kind != TableKind::Object(MdKind::ChartOfAccounts) {
        return Err(fatal("account does not target a chart of accounts"));
    }
    Ok(chart)
}

pub(crate) fn accounting_bt_ext_section_identity(
    schema: &QuerySchema,
    table: QTableId,
) -> Option<QTableId> {
    if !is_accounting_balance_and_turnovers(schema, table) {
        return None;
    }
    let chart = accounting_bt_chart(schema, table).ok()?;
    let owner = schema.table(chart);
    let reference = schema.ref_info(owner.reference?);
    if reference.kind != MdKind::ChartOfAccounts
        || reference.table != chart
        || Some(reference.object) != owner.object
    {
        return None;
    }
    let mut fields = owner
        .fields
        .iter()
        .filter(|field| field.role == FieldRole::Std(StdRole::ExtDimensionTypes));
    let field = fields.next()?;
    if fields.next().is_some() || field.name_en != "ExtDimensionTypes" {
        return None;
    }
    let section = field.nested?;
    let nested = schema.table(section);
    if nested.owner != Some(chart) || nested.kind != TableKind::Tabular || nested.object.is_some() {
        return None;
    }
    Some(section)
}

fn has_inline_ext_values(info: &TableInfo) -> bool {
    info.fields.iter().any(|field| {
        matches!(
            field.std,
            Some(StdField::ValueDt | StdField::ValueCt | StdField::Value)
        ) || field.name.starts_with("ValueDt")
            || field.name.starts_with("ValueCt")
            || field.name.starts_with("Value")
    })
}

pub(crate) fn accounting_bt_ext_ed_table(
    schema: &QuerySchema,
    map: &StorageMap,
    table: QTableId,
) -> R<Option<TableId>> {
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing register owner"))?;
    let metadata = schema
        .table(owner)
        .object
        .ok_or_else(|| fatal("missing register UUID"))?;
    let object = map
        .object(metadata)
        .ok_or_else(|| fatal("missing register storage object"))?;
    let main_info = map
        .try_table(object.main)
        .ok_or_else(|| fatal("missing main table descriptor"))?;
    if has_inline_ext_values(main_info) {
        return Ok(None);
    }
    let mut eds = object
        .service
        .iter()
        .filter(|(purpose, _)| purpose == role::EXT_DIMENSIONS_VALUES)
        .map(|(_, id)| *id);
    let Some(ed_id) = eds.next() else {
        return Ok(None);
    };
    if eds.next().is_some() {
        return Err(fatal("ambiguous AccRgED service table"));
    }
    Ok(Some(ed_id))
}

pub(crate) fn accounting_bt_uses_ext_dims(
    stmt: &StmtS,
    schema: &QuerySchema,
    table: QTableId,
) -> bool {
    if !is_accounting_balance_and_turnovers(schema, table) {
        return false;
    }
    let mut used = false;
    query_syntax::with_deep_stack(stmt.max_depth, || {
        crate::bind::visit_stmt(stmt, &mut |expr, _| {
            if used {
                return;
            }
            let ExprKind::Col { src, col } = expr.kind else {
                return;
            };
            let Some(source) = stmt.sources.get(src.0 as usize) else {
                return;
            };
            let SrcKind::Virtual {
                table: actual,
                ref cols,
            } = source.kind
            else {
                return;
            };
            if actual != table {
                return;
            }
            let field = match col {
                ColSel::Field(field) => Some(field),
                ColSel::Column(index) => cols.get(usize::from(index)).copied().flatten(),
            };
            if let Some(field) = field {
                used |= schema
                    .field(table, field)
                    .is_some_and(|f| matches!(f.role, FieldRole::Std(StdRole::ExtDimension(_))));
            }
        });
    });
    used
}

fn standard(info: &TableInfo, std: StdField, code: TypeCode) -> R<FieldId> {
    let mut matches = info
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.std == Some(std));
    let (i, f) = matches
        .next()
        .ok_or_else(|| fatal(format_args!("missing {std:?} in {}", info.logical)))?;
    if matches.next().is_some() || !matches!(f.types.as_slice(), [ty] if ty.code == code) {
        return Err(fatal(format_args!(
            "ambiguous or wrongly typed {std:?} in {}",
            info.logical
        )));
    }
    u16::try_from(i)
        .map(FieldId)
        .map_err(|_| fatal("field index overflow"))
}

fn ty(info: &TableInfo, field: FieldId) -> R<&LType> {
    match info.field(field).map(|f| f.types.as_slice()) {
        Some([ty]) => Ok(ty),
        _ => Err(fatal("ambiguous field type")),
    }
}

pub(crate) fn accounting_bt_inputs(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
) -> R<(BalanceInputsL, Option<FieldId>)> {
    let chart_table = accounting_bt_chart(schema, table)?;
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing owner"))?;
    let meta = schema
        .meta(owner)
        .ok_or_else(|| fatal("missing register metadata"))?;
    let metadata = schema
        .table(owner)
        .object
        .ok_or_else(|| fatal("missing register UUID"))?;
    let object = map
        .object(metadata)
        .ok_or_else(|| fatal("missing register storage object"))?;
    let main = object.main;
    let mut options = object
        .service
        .iter()
        .filter(|(r, _)| r == role::ACCOUNT_REGISTER_TOTALS_OPTIONS);
    let options_id = options
        .next()
        .map(|(_, id)| *id)
        .ok_or_else(|| fatal("missing options role"))?;
    if options.next().is_some()
        || object.service.iter().any(|(r, _)| r == role::MAIN)
        || storage.table(owner) != Some(main)
        || main == options_id
    {
        return Err(fatal("ambiguous main/options binding"));
    }
    let chart = storage
        .table(chart_table)
        .ok_or_else(|| fatal("unbound account chart"))?;
    if chart == main || chart == options_id {
        return Err(fatal("aliased chart/main/options"));
    }
    let m = map
        .try_table(main)
        .ok_or_else(|| fatal("missing main table"))?;
    let o = map
        .try_table(options_id)
        .ok_or_else(|| fatal("missing options table"))?;
    let c = map
        .try_table(chart)
        .ok_or_else(|| fatal("missing chart table"))?;
    if m.md != Some(metadata)
        || m.purpose != role::MAIN
        || o.purpose != role::ACCOUNT_REGISTER_TOTALS_OPTIONS
        || c.md != schema.table(chart_table).object
        || c.purpose != role::MAIN
    {
        return Err(fatal("inconsistent input ownership or purpose"));
    }
    let active = standard(m, StdField::Active, TypeCode::L)?;
    let (accounts, record_kind) = if meta.corr {
        (
            [
                standard(m, StdField::AccountDt, TypeCode::R)?,
                standard(m, StdField::AccountCt, TypeCode::R)?,
            ],
            None,
        )
    } else {
        let acct = standard(m, StdField::Account, TypeCode::R)?;
        let rk = standard(m, StdField::Correspond, TypeCode::N)?;
        ([acct, acct], Some(rk))
    };
    for field in accounts {
        let t = ty(m, field)?;
        if t.ref_kind != 3 || map.table_by_name(&t.ref_table) != Some(chart) {
            return Err(fatal("account storage target differs from metadata"));
        }
    }
    let chart_id = standard(c, StdField::Id, TypeCode::R)?;
    let id_ty = ty(c, chart_id)?;
    if id_ty.ref_kind != 2 || map.table_by_name(&id_ty.ref_table) != Some(chart) {
        return Err(fatal("chart ID target mismatch"));
    }
    let chart_kind = standard(c, StdField::Kind, TypeCode::N)?;
    if (ty(c, chart_kind)?.length, ty(c, chart_kind)?.precision) != (1, 0) {
        return Err(fatal("wrong chart Kind qualifiers"));
    }
    for (std, code) in [
        (StdField::RegId, TypeCode::B),
        (StdField::Period, TypeCode::T),
        (StdField::ActualPeriod, TypeCode::L),
        (StdField::UseTotals, TypeCode::N),
        (StdField::MinPeriod, TypeCode::T),
        (StdField::UseSplitter, TypeCode::L),
    ] {
        let field = standard(o, std, code)?;
        let t = ty(o, field)?;
        if (std == StdField::RegId && t.length != 16)
            || (std == StdField::UseTotals && (t.length, t.precision) != (1, 0))
        {
            return Err(fatal("wrong options qualifiers"));
        }
    }
    if o.fields
        .iter()
        .any(|f| f.std == Some(StdField::MinCalculatedPeriod))
    {
        standard(o, StdField::MinCalculatedPeriod, TypeCode::T)?;
    }
    let ref_table = map
        .ref_number(chart)
        .ok_or_else(|| fatal("missing chart reference number"))?;
    Ok((
        BalanceInputsL {
            main,
            options: options_id,
            chart,
            chart_id,
            chart_kind,
            active,
            accounts,
            ref_table,
        },
        record_kind,
    ))
}

pub(crate) fn accounting_bt_bind_areas(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    with_section: bool,
    filters: &[crate::host::SeparatorFilter],
) -> R<BalanceAreasL> {
    let (inputs, _) = accounting_bt_inputs(schema, storage, map, table)?;
    let section_id = if with_section {
        accounting_bt_ext_section_identity(schema, table).and_then(|q| storage.table(q))
    } else {
        None
    };
    let ed_id = if with_section {
        accounting_bt_ext_ed_table(schema, map, table)?
    } else {
        None
    };
    let table_fields = |id: TableId| -> R<Vec<BalanceAreaFieldL>> {
        let info = map
            .try_table(id)
            .ok_or_else(|| fatal("missing required input"))?;
        let mut fields = Vec::with_capacity(info.separators.len());
        for sep in &info.separators {
            if sep.common_attribute == Guid::NIL
                || !schema.is_data_separator(sep.common_attribute)
                || sep.use_field.is_some()
            {
                return Err(areas_later());
            }
            if fields
                .iter()
                .any(|e: &BalanceAreaFieldL| e.common_attribute == sep.common_attribute)
            {
                return Err(areas_later());
            }
            let field = info.field(sep.field).ok_or_else(areas_later)?;
            if field.nullable || field.std.is_some() {
                return Err(areas_later());
            }
            fields.push(BalanceAreaFieldL {
                common_attribute: sep.common_attribute,
                field: sep.field,
            });
        }
        Ok(fields)
    };
    let main = table_fields(inputs.main)?;
    let options = table_fields(inputs.options)?;
    let chart = table_fields(inputs.chart)?;
    let section = match section_id {
        Some(id) => table_fields(id)?,
        None => Vec::new(),
    };
    let ext_dims = match ed_id {
        Some(id) => table_fields(id)?,
        None => Vec::new(),
    };
    if main.is_empty()
        && options.is_empty()
        && chart.is_empty()
        && section.is_empty()
        && ext_dims.is_empty()
    {
        return Ok(BalanceAreasL::None);
    }
    if filters.is_empty() {
        return Ok(BalanceAreasL::Unseparated(BalanceTableAreasL {
            main,
            options,
            chart,
            section,
            ext_dims,
        }));
    }
    let bind_table = |id: TableId, fields: Vec<BalanceAreaFieldL>| -> R<Vec<BalanceAreaExactL>> {
        let info = map
            .try_table(id)
            .ok_or_else(|| fatal("missing required input"))?;
        let mut out = Vec::with_capacity(fields.len());
        for entry in fields {
            let mut matches = filters
                .iter()
                .filter(|f| f.common_attribute == entry.common_attribute);
            let filter = matches.next().ok_or_else(areas_later)?;
            if matches.next().is_some() {
                return Err(areas_later());
            }
            let stored = info.field(entry.field).ok_or_else(areas_later)?;
            let [ltype] = stored.types.as_slice() else {
                return Err(areas_later());
            };
            let matches_type = match (ltype.code, &filter.value) {
                (TypeCode::N, QValue::Number(_)) => ltype.precision == 0,
                (TypeCode::S, QValue::String(_)) => ltype.precision == 0,
                (TypeCode::L, QValue::Bool(_)) | (TypeCode::T, QValue::Date(_)) => true,
                (TypeCode::R, QValue::Ref { table, .. }) => {
                    ltype.ref_kind == 3
                        && map.ref_table(*table) == map.table_by_name(&ltype.ref_table)
                }
                _ => false,
            };
            if !matches_type {
                return Err(areas_later());
            }
            out.push(BalanceAreaExactL {
                common_attribute: entry.common_attribute,
                field: entry.field,
                value: filter.value.clone(),
            });
        }
        Ok(out)
    };
    Ok(BalanceAreasL::Exact(BalanceTableAreasL {
        main: bind_table(inputs.main, main)?,
        options: bind_table(inputs.options, options)?,
        chart: bind_table(inputs.chart, chart)?,
        section: match section_id {
            Some(id) => bind_table(id, section)?,
            None => Vec::new(),
        },
        ext_dims: match ed_id {
            Some(id) => bind_table(id, ext_dims)?,
            None => Vec::new(),
        },
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BtComponent {
    OpeningBalance,
    OpeningBalanceDr,
    OpeningBalanceCr,
    OpeningExpandedDr,
    OpeningExpandedCr,
    Turnover,
    TurnoverDr,
    TurnoverCr,
    ClosingBalance,
    ClosingBalanceDr,
    ClosingBalanceCr,
    ClosingExpandedDr,
    ClosingExpandedCr,
}

impl BtComponent {
    fn parse(name_ru: &str, item: &str) -> Option<Self> {
        let suffix = name_ru.strip_prefix(item)?;
        Some(match suffix {
            "НачальныйОстаток" => Self::OpeningBalance,
            "НачальныйОстатокДт" => Self::OpeningBalanceDr,
            "НачальныйОстатокКт" => Self::OpeningBalanceCr,
            "НачальныйРазвернутыйОстатокДт" => Self::OpeningExpandedDr,
            "НачальныйРазвернутыйОстатокКт" => Self::OpeningExpandedCr,
            "Оборот" => Self::Turnover,
            "ОборотДт" => Self::TurnoverDr,
            "ОборотКт" => Self::TurnoverCr,
            "КонечныйОстаток" => Self::ClosingBalance,
            "КонечныйОстатокДт" => Self::ClosingBalanceDr,
            "КонечныйОстатокКт" => Self::ClosingBalanceCr,
            "КонечныйРазвернутыйОстатокДт" => Self::ClosingExpandedDr,
            "КонечныйРазвернутыйОстатокКт" => Self::ClosingExpandedCr,
            _ => return None,
        })
    }

    fn raw_component(self) -> usize {
        match self {
            Self::OpeningBalance
            | Self::OpeningBalanceDr
            | Self::OpeningBalanceCr
            | Self::OpeningExpandedDr
            | Self::OpeningExpandedCr => raw::BALANCE,
            Self::Turnover => raw::TURNOVER,
            Self::TurnoverDr => raw::RECEIPT,
            Self::TurnoverCr => raw::EXPENSE,
            Self::ClosingBalance
            | Self::ClosingBalanceDr
            | Self::ClosingBalanceCr
            | Self::ClosingExpandedDr
            | Self::ClosingExpandedCr => raw::CLOSING,
        }
    }

    fn needs_balance(self) -> bool {
        matches!(self.raw_component(), raw::BALANCE | raw::CLOSING)
    }

    fn needs_turnover(self) -> bool {
        matches!(
            self.raw_component(),
            raw::TURNOVER | raw::RECEIPT | raw::EXPENSE
        )
    }
}

fn bt_dims(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
    corr: bool,
    used: &[Guid],
) -> R<Vec<BalanceDimL>> {
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing owner"))?;
    let meta = schema
        .meta(owner)
        .ok_or_else(|| fatal("missing accounting metadata"))?;
    let main = map
        .try_table(inputs.main)
        .ok_or_else(|| fatal("missing main"))?;
    let mut result = Vec::new();
    for metadata in used {
        let Some(item) = meta.dims.iter().find(|d| d.id == *metadata) else {
            continue;
        };
        let (fields, nullable) = if item.balance || !corr {
            let mut matches = main
                .fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.md == Some(*metadata));
            let (index, field) = matches
                .next()
                .ok_or_else(|| fatal("missing balanced main dimension"))?;
            if matches.next().is_some() || field.side.is_some() {
                return Err(fatal("ambiguous balanced dimension UUID"));
            }
            let common = FieldId(u16::try_from(index).map_err(|_| fatal("field index overflow"))?);
            ([common, common], field.nullable)
        } else {
            let mut slots = [None; 2];
            for (index, field) in main
                .fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.md == Some(*metadata))
            {
                let slot = match field.side {
                    Some(Side::Dr) => &mut slots[0],
                    Some(Side::Cr) => &mut slots[1],
                    None => return Err(fatal("unsided nonbalanced dimension UUID")),
                };
                let id = FieldId(u16::try_from(index).map_err(|_| fatal("field index overflow"))?);
                if slot.replace(id).is_some() {
                    return Err(fatal("ambiguous nonbalanced dimension side"));
                }
            }
            let [Some(dr), Some(cr)] = slots else {
                return Err(fatal("missing nonbalanced dimension side"));
            };
            let nullable = main.field(dr).is_some_and(|f| f.nullable);
            ([dr, cr], nullable)
        };
        for field in fields {
            let stored = main
                .field(field)
                .ok_or_else(|| fatal("missing main dimension field"))?;
            super::accounting_balance_payload::validate_shape(&item.ty, stored, storage, map)?;
        }
        result.push(BalanceDimL {
            metadata: *metadata,
            main: fields,
            nullable,
        });
    }
    Ok(result)
}

fn bt_resources(
    schema: &QuerySchema,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
    corr: bool,
    used: &[Guid],
) -> R<Vec<BalanceResourceL>> {
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing owner"))?;
    let meta = schema
        .meta(owner)
        .ok_or_else(|| fatal("missing accounting metadata"))?;
    let main = map
        .try_table(inputs.main)
        .ok_or_else(|| fatal("missing main"))?;
    let mut result = Vec::with_capacity(used.len());
    for metadata in used {
        let Some(item) = meta.resources.iter().find(|r| r.id == *metadata) else {
            continue;
        };
        let qualifiers = item
            .ty
            .number
            .ok_or_else(|| later("Q6c-AccountingBalanceAndTurnoversResourceShape"))?;
        if !item.ty.only_number()
            || qualifiers.digits == 0
            || qualifiers.digits > 38
            || qualifiers.scale > qualifiers.digits
        {
            return Err(later("Q6c-AccountingBalanceAndTurnoversResourceShape"));
        }
        let (fields, null_zero) = if item.balance || !corr {
            let mut matches = main
                .fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.md == Some(*metadata));
            let (index, field) = matches
                .next()
                .ok_or_else(|| fatal("missing balanced main resource"))?;
            if matches.next().is_some() || field.side.is_some() {
                return Err(fatal("ambiguous balanced resource UUID"));
            }
            let common = FieldId(u16::try_from(index).map_err(|_| fatal("field index overflow"))?);
            let has_flag = matches!(
                item.accounting_flags.accounting,
                AccountingFlagBinding::Present { .. }
            ) || matches!(
                item.accounting_flags.ext_dimension,
                AccountingFlagBinding::Present { .. }
            );
            let nullable = field.nullable && has_flag;
            if field.nullable && !nullable {
                return Err(later("Q6c-AccountingBalanceAndTurnoversNullableResource"));
            }
            ([common, common], [nullable; 2])
        } else {
            let mut slots = [None; 2];
            for (index, field) in main
                .fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.md == Some(*metadata))
            {
                let slot = match field.side {
                    Some(Side::Dr) => &mut slots[0],
                    Some(Side::Cr) => &mut slots[1],
                    None => return Err(fatal("unsided nonbalanced resource UUID")),
                };
                let id = FieldId(u16::try_from(index).map_err(|_| fatal("field index overflow"))?);
                if slot.replace(id).is_some() {
                    return Err(fatal("ambiguous nonbalanced resource side"));
                }
            }
            let [Some(dr), Some(cr)] = slots else {
                return Err(fatal("missing nonbalanced resource side"));
            };
            let nullable = matches!(
                item.accounting_flags.accounting,
                AccountingFlagBinding::Present { .. }
            ) || matches!(
                item.accounting_flags.ext_dimension,
                AccountingFlagBinding::Present { .. }
            );
            for field in [dr, cr] {
                if main
                    .field(field)
                    .is_none_or(|field| field.nullable != nullable)
                {
                    return Err(fatal("resource nullability contradicts flag provenance"));
                }
            }
            ([dr, cr], [nullable; 2])
        };
        for field in fields {
            let stored = main
                .field(field)
                .ok_or_else(|| fatal("missing main resource field"))?;
            if !matches!(
                stored.types.as_slice(),
                [t] if t.code == TypeCode::N
                    && t.length == u32::from(qualifiers.digits)
                    && t.precision == u32::from(qualifiers.scale)
            ) {
                return Err(fatal("resource numeric type contradicts metadata"));
            }
        }
        result.push(BalanceResourceL {
            metadata: *metadata,
            main: fields,
            null_zero,
            ext_flag: None,
            dec: Dec {
                p: qualifiers.digits,
                s: qualifiers.scale,
            },
        });
    }
    Ok(result)
}

fn find_inline_ext_pair(main: &TableInfo, ordinal: u8, corr: bool) -> R<[FieldId; 2]> {
    if corr {
        let dt_name = format!("ValueDt{ordinal}");
        let ct_name = format!("ValueCt{ordinal}");
        let mut dt = None;
        let mut ct = None;
        for (idx, field) in main.fields.iter().enumerate() {
            let fid = FieldId(u16::try_from(idx).map_err(|_| fatal("field index overflow"))?);
            if field.name == dt_name
                || (field.std == Some(StdField::ValueDt)
                    && field.name.ends_with(&ordinal.to_string()))
            {
                if dt.replace(fid).is_some() {
                    return Err(fatal("duplicate inline ValueDt slot"));
                }
            } else if (field.name == ct_name
                || (field.std == Some(StdField::ValueCt)
                    && field.name.ends_with(&ordinal.to_string())))
                && ct.replace(fid).is_some()
            {
                return Err(fatal("duplicate inline ValueCt slot"));
            }
        }
        match (dt, ct) {
            (Some(d), Some(c)) => Ok([d, c]),
            _ => Err(fatal(format_args!(
                "missing inline ValueDt/ValueCt for ordinal {ordinal}"
            ))),
        }
    } else {
        let v_name = format!("Value{ordinal}");
        let mut found = None;
        for (idx, field) in main.fields.iter().enumerate() {
            let fid = FieldId(u16::try_from(idx).map_err(|_| fatal("field index overflow"))?);
            if (field.name == v_name
                || (field.std == Some(StdField::Value)
                    && field.name.ends_with(&ordinal.to_string())))
                && found.replace(fid).is_some()
            {
                return Err(fatal("duplicate inline Value slot"));
            }
        }
        let f = found
            .ok_or_else(|| fatal(format_args!("missing inline Value for ordinal {ordinal}")))?;
        Ok([f, f])
    }
}

fn resolve_ext_flag_field(
    schema: &QuerySchema,
    map: &StorageMap,
    table: QTableId,
    section: TableId,
    chart_md: Guid,
    child_md: Guid,
) -> R<FieldId> {
    let chart = accounting_bt_chart(schema, table)?;
    if schema.table(chart).object != Some(chart_md) {
        return Err(fatal("ExtDimensionAccountingFlag chart mismatch"));
    }
    let section_info = map
        .try_table(section)
        .ok_or_else(|| fatal("missing ExtDimensionTypes table"))?;
    let mut matches = section_info
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.md == Some(child_md));
    let (idx, field) = matches
        .next()
        .ok_or_else(|| fatal("missing ExtDimensionAccountingFlag column in section"))?;
    if matches.next().is_some() || field.nullable || field.side.is_some() {
        return Err(fatal(
            "invalid ExtDimensionAccountingFlag column in section",
        ));
    }
    if !matches!(
        field.types.as_slice(),
        [t] if t.code == TypeCode::L && t.length == 0 && t.precision == 0
    ) {
        return Err(fatal("ExtDimensionAccountingFlag column is not Boolean"));
    }
    u16::try_from(idx)
        .map(FieldId)
        .map_err(|_| fatal("field index overflow"))
}

fn forced_period(periodicity: Periodicity, field: PeriodField) -> bool {
    match periodicity {
        Periodicity::Unit(_) => field == PeriodField::Period,
        Periodicity::Recorder => matches!(field, PeriodField::Period | PeriodField::Recorder),
        Periodicity::Record => matches!(
            field,
            PeriodField::Period | PeriodField::Recorder | PeriodField::LineNumber
        ),
        Periodicity::Absent | Periodicity::Auto => false,
    }
}

fn valid_recorder_types(types: &[LType], reference_tags: &[u32], map: &StorageMap) -> bool {
    match types {
        [r] if r.code == TypeCode::R && r.ref_kind == 3 => {
            reference_tags.len() == 1
                && map
                    .table_by_name(&r.ref_table)
                    .and_then(|id| map.ref_number(id))
                    == reference_tags.first().copied()
        }
        [r] if r.code == TypeCode::R && r.ref_kind == 4 && r.ref_table.is_empty() => {
            reference_tags.len() != 1
        }
        [e, r]
            if e.code == TypeCode::E
                && e.length == 0
                && e.precision == 0
                && r.code == TypeCode::R
                && r.ref_kind == 4
                && r.ref_table.is_empty() =>
        {
            reference_tags.len() != 1
        }
        _ => false,
    }
}

fn resolve_recorder(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    main_info: &TableInfo,
) -> R<AccountingTurnoversRecorderL> {
    let sem = schema
        .table(table)
        .fields
        .iter()
        .find(|f| f.period == Some(PeriodField::Recorder))
        .ok_or_else(|| fatal("missing semantic Recorder field"))?;
    let mut reference_tags = Vec::with_capacity(sem.ty.refs.len());
    for atom in &sem.ty.refs {
        let tag = storage
            .ref_number(*atom)
            .ok_or_else(|| fatal("unbound semantic Recorder reference"))?;
        reference_tags.push(tag);
    }
    reference_tags.sort_unstable();
    reference_tags.dedup();
    let mut phys = main_info
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.std == Some(StdField::Recorder));
    let (index, phys_info) = phys
        .next()
        .ok_or_else(|| fatal("missing Recorder physical field"))?;
    if phys.next().is_some() || phys_info.nullable || phys_info.side.is_some() {
        return Err(fatal("ambiguous or invalid Recorder physical field"));
    }
    let field = FieldId(u16::try_from(index).map_err(|_| fatal("Recorder field overflow"))?);
    if !valid_recorder_types(&phys_info.types, &reference_tags, map) {
        return Err(fatal("Recorder physical type contradicts metadata"));
    }
    Ok(AccountingTurnoversRecorderL {
        field,
        reference_tags,
    })
}

fn resolve_line(main_info: &TableInfo) -> R<FieldId> {
    let field = standard(main_info, StdField::LineNo, TypeCode::N)?;
    let t = ty(main_info, field)?;
    if t.precision != 0
        || main_info
            .field(field)
            .is_none_or(|f| f.nullable || f.side.is_some())
    {
        return Err(fatal("invalid LineNo physical field"));
    }
    Ok(field)
}

#[allow(clippy::too_many_arguments)]
fn resolve_ed_storage(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
    section_info: &TableInfo,
    kind_ref_table: u32,
    corr: bool,
) -> R<(AccountingBalanceAndTurnoversAccRgEdL, FieldId)> {
    let ed_table = accounting_bt_ext_ed_table(schema, map, table)?
        .ok_or_else(|| fatal("missing AccRgED table"))?;
    let ed_info = map
        .try_table(ed_table)
        .ok_or_else(|| fatal("missing AccRgED descriptor"))?;
    let main_info = map
        .try_table(inputs.main)
        .ok_or_else(|| fatal("missing main descriptor"))?;
    let main_period = standard(main_info, StdField::Period, TypeCode::T)?;
    let recorder = resolve_recorder(schema, storage, map, table, main_info)?;
    let main_line = resolve_line(main_info)?;
    let ed_period = standard(ed_info, StdField::Period, TypeCode::T)?;
    let ed_recorder = {
        let mut phys = ed_info
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.std == Some(StdField::Recorder));
        let (idx, info) = phys
            .next()
            .ok_or_else(|| fatal("missing AccRgED Recorder"))?;
        if phys.next().is_some() || info.nullable {
            return Err(fatal("ambiguous AccRgED Recorder"));
        }
        FieldId(u16::try_from(idx).map_err(|_| fatal("field index overflow"))?)
    };
    let ed_line = resolve_line(ed_info)?;
    let ed_kind = standard(ed_info, StdField::Kind, TypeCode::R)?;
    let ed_value = ed_info
        .fields
        .iter()
        .enumerate()
        .find(|(_, f)| f.std == Some(StdField::Value))
        .map(|(i, _)| u16::try_from(i).map(FieldId))
        .transpose()
        .map_err(|_| fatal("field index overflow"))?
        .ok_or_else(|| fatal("missing AccRgED Value"))?;
    let ed_correspond = if corr {
        Some(standard(ed_info, StdField::Correspond, TypeCode::N)?)
    } else {
        None
    };
    let section_dim_kind = standard(section_info, StdField::DimKind, TypeCode::R)?;
    let main_by_period = find_index_with_seps(main_info, &[main_period]);
    let ed_by_period = find_index_with_seps(ed_info, &[ed_period]);
    let ed_by_recorder = match ed_correspond {
        Some(c) => find_index_with_seps(ed_info, &[ed_recorder, ed_line, c])
            .map(|ix| (ix, true))
            .or_else(|| {
                find_index_with_seps(ed_info, &[ed_recorder, ed_line]).map(|ix| (ix, false))
            }),
        None => find_index_with_seps(ed_info, &[ed_recorder, ed_line]).map(|ix| (ix, false)),
    };
    Ok((
        AccountingBalanceAndTurnoversAccRgEdL {
            table: ed_table,
            section_dim_kind,
            kind_ref_table,
            main_period,
            main_recorder: recorder.field,
            main_line,
            recorder_tags: recorder.reference_tags,
            ed_period,
            ed_recorder,
            ed_line,
            ed_correspond,
            ed_kind,
            ed_value,
            main_by_period,
            ed_by_period,
            ed_by_recorder,
        },
        section_dim_kind,
    ))
}

fn bt_conjuncts(e: &Expr) -> Vec<&Expr> {
    let mut out = Vec::new();
    let mut stack = vec![e];
    while let Some(x) = stack.pop() {
        match &x.kind {
            ExprKind::And(v) => stack.extend(v.iter().rev()),
            _ => out.push(x),
        }
    }
    out
}

fn combine_bt_conjuncts(conjuncts: &[&Expr], ty: &QType) -> Expr {
    match conjuncts {
        [single] => (*single).clone(),
        many => Expr {
            kind: ExprKind::And(many.iter().map(|c| (*c).clone()).collect()),
            ty: ty.clone(),
        },
    }
}

fn collect_rel_sources(rel: &Rel, out: &mut Vec<SrcId>) {
    match rel {
        Rel::Scan { src, .. } | Rel::TempScan { src, .. } | Rel::ParamScan { src, .. } => {
            out.push(*src);
        }
        Rel::RegisterVt(v) | Rel::Slice(v) | Rel::RunningBalance(v) => {
            out.push(v.src);
            out.push(v.inner);
        }
        Rel::Subquery { src, input } => {
            out.push(*src);
            collect_rel_sources(input, out);
        }
        Rel::Join { l, r, .. } => {
            collect_rel_sources(l, out);
            collect_rel_sources(r, out);
        }
        Rel::Filter { input, .. }
        | Rel::Aggregate { input, .. }
        | Rel::Project { input, .. }
        | Rel::Distinct(input)
        | Rel::Sort { input, .. }
        | Rel::Top { input, .. }
        | Rel::Hierarchy { input, .. } => collect_rel_sources(input, out),
        Rel::Union { parts, .. } => {
            for p in parts {
                collect_rel_sources(p, out);
            }
        }
        Rel::Empty => {}
    }
}

fn is_pure_account_conjunct(expr: &Expr, inner: SrcId, acct_qf: QFieldId) -> bool {
    let mut seen_acct = false;
    let mut ok = true;
    let mut stack: Vec<(&Expr, Vec<SrcId>)> = vec![(expr, Vec::new())];
    while let Some((e, local)) = stack.pop() {
        match &e.kind {
            ExprKind::Col { src, col } => {
                if *src == inner {
                    if *col == ColSel::Field(acct_qf) {
                        seen_acct = true;
                    } else {
                        ok = false;
                        break;
                    }
                } else if !local.contains(src) {
                    ok = false;
                    break;
                }
            }
            ExprKind::OuterKey { .. } => {
                ok = false;
                break;
            }
            ExprKind::Deref { base, .. }
            | ExprKind::Neg(base)
            | ExprKind::Not(base)
            | ExprKind::IsNull { l: base, .. }
            | ExprKind::Refs { l: base, .. }
            | ExprKind::Cast { x: base, .. }
            | ExprKind::LocalKey { orig: base, .. } => stack.push((base, local)),
            ExprKind::Arith { first, steps } => {
                stack.push((first, local.clone()));
                for (_, rhs, _) in steps {
                    stack.push((rhs, local.clone()));
                }
            }
            ExprKind::And(v) | ExprKind::Or(v) | ExprKind::Func { args: v, .. } => {
                for x in v {
                    stack.push((x, local.clone()));
                }
            }
            ExprKind::Compare(_, a, b) | ExprKind::Like { l: a, pat: b, .. } => {
                stack.push((a, local.clone()));
                stack.push((b, local));
            }
            ExprKind::InList { l, rhs, .. } => {
                for x in l.iter().chain(rhs) {
                    stack.push((x, local.clone()));
                }
            }
            ExprKind::Between { l, lo, hi, .. } => {
                stack.push((l, local.clone()));
                stack.push((lo, local.clone()));
                stack.push((hi, local));
            }
            ExprKind::Case {
                operand,
                whens,
                else_,
            } => {
                if let Some(o) = operand {
                    stack.push((o, local.clone()));
                }
                for (w, t) in whens {
                    stack.push((w, local.clone()));
                    stack.push((t, local.clone()));
                }
                if let Some(el) = else_ {
                    stack.push((el, local));
                }
            }
            ExprKind::InQuery { l, q, .. } => {
                for x in l {
                    stack.push((x, local.clone()));
                }
                let mut sub_local = local;
                collect_rel_sources(q, &mut sub_local);
                let mut sub_ok = true;
                crate::ir::visit_rel(q, &mut |sub_e| {
                    if let ExprKind::Col { src, .. } = &sub_e.kind
                        && !sub_local.contains(src)
                    {
                        sub_ok = false;
                    }
                    if matches!(sub_e.kind, ExprKind::OuterKey { .. }) {
                        sub_ok = false;
                    }
                });
                if !sub_ok {
                    ok = false;
                    break;
                }
            }
            ExprKind::SectionAgg { .. } => {
                ok = false;
                break;
            }
            ExprKind::Lit(_)
            | ExprKind::Param(_)
            | ExprKind::Value(_)
            | ExprKind::KeyRef(_)
            | ExprKind::AggRef(_)
            | ExprKind::Out(_)
            | ExprKind::Unit(_) => {}
        }
    }
    ok && seen_acct
}

fn zero() -> PExpr {
    PExpr::Lit(QValue::Number(bsl_prim::Number::zero()))
}

fn zero_if_null(x: PExpr) -> PExpr {
    PExpr::Case {
        operand: None,
        whens: vec![(
            PExpr::IsNull {
                l: Box::new(x.clone()),
                not: false,
            },
            zero(),
        )],
        else_: Some(Box::new(x)),
    }
}

fn begin_of_period(kw: Kw, x: PExpr) -> PExpr {
    PExpr::Func {
        f: Func::BeginOfPeriod,
        args: vec![x, PExpr::Unit(kw)],
        ty: QType::date(DateQ::DateTime),
        arg_dec: Dec::UNKNOWN,
    }
}

fn max_expr(a: PExpr, b: PExpr) -> PExpr {
    PExpr::Case {
        operand: None,
        whens: vec![(
            PExpr::Compare(CmpOp::Lt, Box::new(a.clone()), Box::new(b.clone())),
            b,
        )],
        else_: Some(Box::new(a)),
    }
}

fn opening_or(e: PExpr) -> PExpr {
    PExpr::Case {
        operand: None,
        whens: vec![(
            PExpr::IsNull {
                l: Box::new(PExpr::Col(0)),
                not: false,
            },
            PExpr::Lit(QValue::Null),
        )],
        else_: Some(Box::new(e)),
    }
}

fn balance_expr(comp: BtComponent, val: PExpr, kind: Option<&PExpr>) -> PExpr {
    let compare = |op, a, b| PExpr::Compare(op, Box::new(a), Box::new(b));
    let case = |pred, yes| PExpr::Case {
        operand: None,
        whens: vec![(pred, yes)],
        else_: Some(Box::new(zero())),
    };
    let positive = compare(CmpOp::Gt, val.clone(), zero());
    let negative = compare(CmpOp::Lt, val.clone(), zero());
    let is_kind = |k: &PExpr, n: i64| {
        compare(
            CmpOp::Eq,
            k.clone(),
            PExpr::Lit(QValue::Number(bsl_prim::Number::from(n))),
        )
    };
    match comp {
        BtComponent::OpeningBalance
        | BtComponent::ClosingBalance
        | BtComponent::Turnover
        | BtComponent::TurnoverDr
        | BtComponent::TurnoverCr => val,
        BtComponent::OpeningBalanceDr | BtComponent::ClosingBalanceDr => match kind {
            Some(k) => case(
                PExpr::Or(vec![
                    is_kind(k, 0),
                    PExpr::And(vec![is_kind(k, 2), positive]),
                ]),
                val,
            ),
            None => case(positive, val),
        },
        BtComponent::OpeningBalanceCr | BtComponent::ClosingBalanceCr => match kind {
            Some(k) => case(
                PExpr::Or(vec![
                    is_kind(k, 1),
                    PExpr::And(vec![is_kind(k, 2), negative]),
                ]),
                PExpr::Neg(Box::new(val)),
            ),
            None => case(negative, PExpr::Neg(Box::new(val))),
        },
        BtComponent::OpeningExpandedDr | BtComponent::ClosingExpandedDr => case(positive, val),
        BtComponent::OpeningExpandedCr | BtComponent::ClosingExpandedCr => {
            case(negative, PExpr::Neg(Box::new(val)))
        }
    }
}

impl Lower<'_> {
    fn bt_ext_kind_ref_table(&self, table: QTableId) -> R<u32> {
        let section_logical = accounting_bt_ext_section_identity(self.schema, table)
            .ok_or_else(|| later("Q6c-AccountingBalanceAndTurnoversArguments"))?;
        let mut fields = self
            .schema
            .table(section_logical)
            .fields
            .iter()
            .filter(|f| f.role == FieldRole::Std(StdRole::ExtDimensionType(0)));
        let field = fields
            .next()
            .ok_or_else(|| fatal("missing semantic ExtDimensionType field"))?;
        if fields.next().is_some() {
            return Err(fatal("duplicate semantic ExtDimensionType field"));
        }
        let [atom] = field.ty.refs.as_slice() else {
            return Err(fatal("invalid semantic ExtDimensionType target"));
        };
        self.storage
            .ref_number(*atom)
            .ok_or_else(|| fatal("unbound ExtDimensionTypes chart"))
    }

    fn resolve_bt_ext_dims_arg(
        &self,
        v: &RegisterVtS,
        max_ext_dims: u8,
    ) -> R<Option<(u32, Vec<[u8; 16]>)>> {
        let Some(expr) = &v.ext_dims_arg else {
            return Ok(None);
        };
        if max_ext_dims == 0 {
            return Err(later("Q6c-AccountingBalanceAndTurnoversArguments"));
        }
        let kind_ref_table = self.bt_ext_kind_ref_table(v.table)?;
        let single;
        let slice: &[QValue] = match &expr.kind {
            ExprKind::Value(idx) => {
                let val = self
                    .b
                    .values
                    .get(usize::from(*idx))
                    .ok_or_else(|| fatal("missing bound ExtDimensions value"))?;
                single = [val.clone()];
                &single
            }
            ExprKind::Param(slot) => match self.b.params.get(usize::from(*slot)) {
                Some(crate::bind::PBound::Value(val)) => {
                    single = [val.clone()];
                    &single
                }
                Some(crate::bind::PBound::Set(set)) => &set.values,
                _ => return Err(later("Q6c-AccountingBalanceAndTurnoversArguments")),
            },
            _ => return Err(later("Q6c-AccountingBalanceAndTurnoversArguments")),
        };
        if slice.is_empty() || slice.len() > usize::from(max_ext_dims) {
            return Err(later("Q6c-AccountingBalanceAndTurnoversArguments"));
        }
        let mut ids = Vec::with_capacity(slice.len());
        for value in slice {
            let QValue::Ref { table, id } = value else {
                return Err(later("Q6c-AccountingBalanceAndTurnoversArguments"));
            };
            if *table != kind_ref_table || *id == [0; 16] || ids.contains(id) {
                return Err(later("Q6c-AccountingBalanceAndTurnoversArguments"));
            }
            ids.push(*id);
        }
        Ok(Some((kind_ref_table, ids)))
    }

    pub(super) fn accounting_balance_and_turnovers(&mut self, v: &RegisterVtS) -> R<Row> {
        self.accounting_consumer(v, true)?;
        let outer_used = self
            .vt_used
            .get(v.src.0 as usize)
            .cloned()
            .unwrap_or_default();
        let mut cond_used = self
            .vt_used
            .get(v.inner.0 as usize)
            .cloned()
            .unwrap_or_default();
        if let Some(cond) = &v.cond {
            crate::ir::visit_expr_in_place(cond, true, &mut |e| {
                if let ExprKind::Col {
                    src,
                    col: ColSel::Field(f),
                } = &e.kind
                    && *src == v.inner
                    && !cond_used.contains(f)
                {
                    cond_used.push(*f);
                }
            });
        }
        let mut all_used = outer_used.clone();
        for id in &cond_used {
            if !all_used.contains(id) {
                all_used.push(*id);
            }
        }
        let table = self.schema.table(v.table);
        let meta = self
            .schema
            .meta(v.register)
            .ok_or_else(|| fatal("missing register metadata"))?;
        if meta.adjustment {
            return Err(later("Q6c-AccountingBalanceAndTurnoversAdjustment"));
        }
        let corr = meta.corr;
        let (inputs, record_kind) =
            accounting_bt_inputs(self.schema, self.storage, self.map, v.table)?;
        let main_info = self
            .map
            .try_table(inputs.main)
            .ok_or_else(|| fatal("missing main table"))?;
        let period = standard(main_info, StdField::Period, TypeCode::T)?;

        let mut bounds: [Option<PExpr>; 2] = [None, None];
        for (slot, b) in bounds.iter_mut().zip(&v.bounds) {
            let Some(b) = b else { continue };
            let mut empty = Row::empty();
            let p = self.expr(b, &mut empty)?;
            if empty.width != 0 {
                return Err(later("Q6c-AccountingBalanceAndTurnoversBoundTable"));
            }
            *slot = Some(p);
        }

        let used_period: Vec<(QFieldId, PeriodField)> = table
            .fields
            .iter()
            .enumerate()
            .filter_map(|(idx, field)| {
                let pf = field.period?;
                let qf = QFieldId(u16::try_from(idx).ok()?);
                (outer_used.contains(&qf) || forced_period(v.periodicity, pf)).then_some((qf, pf))
            })
            .collect();
        let flat_auto = v.periodicity == Periodicity::Auto && used_period.is_empty();
        let periodic = v.periodicity != Periodicity::Absent && !flat_auto;

        let need_recorder = used_period.iter().any(|(_, p)| *p == PeriodField::Recorder)
            || bounds
                .iter()
                .flatten()
                .any(|e| !matches!(e, PExpr::Lit(QValue::Date(_) | QValue::Undefined)));
        let recorder = if need_recorder {
            Some(resolve_recorder(
                self.schema,
                self.storage,
                self.map,
                v.table,
                main_info,
            )?)
        } else {
            None
        };
        let line = if used_period
            .iter()
            .any(|(_, p)| *p == PeriodField::LineNumber)
        {
            Some(resolve_line(main_info)?)
        } else {
            None
        };

        let mut outer_ids = Vec::new();
        let mut outer_roles = Vec::new();
        for id in &outer_used {
            let field = self
                .schema
                .field(v.table, *id)
                .ok_or_else(|| fatal("unknown output"))?;
            if let FieldRole::Std(role) = field.role {
                outer_roles.push(role);
            }
            if let Some(md) = field.md
                && !outer_ids.contains(&md)
            {
                outer_ids.push(md);
            }
        }
        let mut all_ids = outer_ids.clone();
        let mut all_roles = outer_roles.clone();
        for id in &cond_used {
            let field = self
                .schema
                .field(v.table, *id)
                .ok_or_else(|| fatal("unknown condition field"))?;
            if let FieldRole::Std(role) = field.role
                && !all_roles.contains(&role)
            {
                all_roles.push(role);
            }
            if let Some(md) = field.md
                && !all_ids.contains(&md)
            {
                all_ids.push(md);
            }
        }

        let outer_has_account = outer_roles.contains(&StdRole::Account);
        let requested_ext = self.resolve_bt_ext_dims_arg(v, meta.ext_dims)?;
        let ext_ordinals: Vec<u8> = (1..=meta.ext_dims)
            .filter(|n| all_roles.contains(&StdRole::ExtDimension(*n)))
            .collect();

        let dims_meta = bt_dims(
            self.schema,
            self.storage,
            self.map,
            v.table,
            &inputs,
            corr,
            &all_ids,
        )?;
        let mut res_mds: Vec<Guid> = outer_ids
            .iter()
            .copied()
            .filter(|md| meta.resources.iter().any(|r| r.id == *md))
            .collect();
        let any_resource_selected = !res_mds.is_empty();
        if res_mds.is_empty() {
            res_mds.extend(meta.resources.iter().map(|r| r.id));
        }
        let mut resources = bt_resources(self.schema, self.map, v.table, &inputs, corr, &res_mds)?;

        let mut need_opening = !any_resource_selected;
        let mut need_turnovers = !any_resource_selected;
        for id in &outer_used {
            let field = self
                .schema
                .field(v.table, *id)
                .ok_or_else(|| fatal("missing field"))?;
            let Some(md) = field.md else { continue };
            let Some(item) = meta.resources.iter().find(|r| r.id == md) else {
                continue;
            };
            if let Some(comp) = BtComponent::parse(&field.name_ru, &item.name) {
                need_opening |= comp.needs_balance();
                need_turnovers |= comp.needs_turnover();
            }
        }

        let ext_dims = if !ext_ordinals.is_empty() {
            if ext_ordinals.len() > 64 {
                return Err(later("Q6c-AccountingBalanceAndTurnoversExtDimensionLayout"));
            }
            let section_logical = accounting_bt_ext_section_identity(self.schema, v.table)
                .ok_or_else(|| fatal("missing ExtDimensionTypes section"))?;
            let section = self
                .storage
                .table(section_logical)
                .ok_or_else(|| fatal("unbound ExtDimensionTypes section"))?;
            let section_info = self
                .map
                .try_table(section)
                .ok_or_else(|| fatal("missing ExtDimensionTypes storage descriptor"))?;
            let sec_owner = standard(section_info, StdField::Id, TypeCode::R)?;
            let sec_ordinal = standard(section_info, StdField::LineNo, TypeCode::N)?;
            let sec_turnover_only = standard(section_info, StdField::TurnoverOnly, TypeCode::L)?;

            let mut ext_flag_fields: Vec<FieldId> = Vec::new();
            for resource in &mut resources {
                let item = meta
                    .resources
                    .iter()
                    .find(|item| item.id == resource.metadata)
                    .ok_or_else(|| fatal("missing selected resource metadata"))?;
                match item.accounting_flags.ext_dimension {
                    AccountingFlagBinding::Absent => {}
                    AccountingFlagBinding::Present { chart, child }
                        if item.accounting_flags.parent_valid =>
                    {
                        let field = resolve_ext_flag_field(
                            self.schema,
                            self.map,
                            v.table,
                            section,
                            chart,
                            child,
                        )?;
                        let idx =
                            if let Some(pos) = ext_flag_fields.iter().position(|f| *f == field) {
                                pos
                            } else {
                                if ext_flag_fields.len() >= 64 {
                                    return Err(later(
                                        "Q6c-AccountingBalanceAndTurnoversExtDimensionLayout",
                                    ));
                                }
                                ext_flag_fields.push(field);
                                ext_flag_fields.len() - 1
                            };
                        resource.ext_flag =
                            Some(u16::try_from(idx).map_err(|_| fatal("ext flag index overflow"))?);
                    }
                    _ => {
                        return Err(later("Q6c-AccountingBalanceAndTurnoversExtDimensionFlag"));
                    }
                }
            }

            let inline = has_inline_ext_values(main_info);

            let (slots, physical_values, dim_kind, kind_ref_table, value_check, acc_rg_ed) =
                if inline {
                    let mut slots = Vec::with_capacity(ext_ordinals.len());
                    let mut check: Option<TurnoversValueCheckL> = None;
                    for &ord in &ext_ordinals {
                        let (idx, sem_field) = table
                            .fields
                            .iter()
                            .enumerate()
                            .find(|(_, f)| f.role == FieldRole::Std(StdRole::ExtDimension(ord)))
                            .ok_or_else(|| fatal("missing ExtDimension output"))?;
                        let output = u16::try_from(idx)
                            .map(QFieldId)
                            .map_err(|_| fatal("ExtDimension field index overflow"))?;
                        let values = find_inline_ext_pair(main_info, ord, corr)?;
                        let dr_info = main_info
                            .field(values[0])
                            .ok_or_else(|| fatal("missing ExtDimension ValueDt descriptor"))?;
                        let dr_check = value_check_with_later(
                            &sem_field.ty,
                            dr_info,
                            self.storage,
                            self.map,
                            "Q6c-AccountingBalanceAndTurnoversExtDimensionLayout",
                        )?;
                        if corr {
                            let cr_info = main_info
                                .field(values[1])
                                .ok_or_else(|| fatal("missing ExtDimension ValueCt descriptor"))?;
                            let cr_check = value_check_with_later(
                                &sem_field.ty,
                                cr_info,
                                self.storage,
                                self.map,
                                "Q6c-AccountingBalanceAndTurnoversExtDimensionLayout",
                            )?;
                            if dr_check != cr_check {
                                return Err(fatal("incompatible ExtDimension side checks"));
                            }
                        }
                        if check.is_none() {
                            check = Some(dr_check);
                        }
                        slots.push(AccountingBalanceAndTurnoversExtSlotL {
                            ordinal: ord,
                            output,
                            values: Some(values),
                        });
                    }
                    let value_check =
                        check.ok_or_else(|| fatal("missing ExtDimension value check"))?;
                    if let Some((ref_table, _)) = &requested_ext {
                        let dk = standard(section_info, StdField::DimKind, TypeCode::R)?;
                        let mut physical_values = Vec::with_capacity(usize::from(meta.ext_dims));
                        for phys_ord in 1..=meta.ext_dims {
                            physical_values.push(find_inline_ext_pair(main_info, phys_ord, corr)?);
                        }
                        (
                            slots,
                            physical_values,
                            Some(dk),
                            Some(*ref_table),
                            value_check,
                            None,
                        )
                    } else {
                        (slots, Vec::new(), None, None, value_check, None)
                    }
                } else {
                    let kind_ref = match &requested_ext {
                        Some((rt, _)) => *rt,
                        None => self.bt_ext_kind_ref_table(v.table)?,
                    };
                    let (ed_desc, dk) = resolve_ed_storage(
                        self.schema,
                        self.storage,
                        self.map,
                        v.table,
                        &inputs,
                        section_info,
                        kind_ref,
                        corr,
                    )?;
                    let ed_info = self
                        .map
                        .try_table(ed_desc.table)
                        .ok_or_else(|| fatal("missing AccRgED descriptor"))?;
                    let ed_val_info = ed_info
                        .field(ed_desc.ed_value)
                        .ok_or_else(|| fatal("missing AccRgED Value descriptor"))?;
                    let mut slots = Vec::with_capacity(ext_ordinals.len());
                    let mut check: Option<TurnoversValueCheckL> = None;
                    for &ord in &ext_ordinals {
                        let (idx, sem_field) = table
                            .fields
                            .iter()
                            .enumerate()
                            .find(|(_, f)| f.role == FieldRole::Std(StdRole::ExtDimension(ord)))
                            .ok_or_else(|| fatal("missing ExtDimension output"))?;
                        let output = u16::try_from(idx)
                            .map(QFieldId)
                            .map_err(|_| fatal("ExtDimension field index overflow"))?;
                        let slot_check = value_check_with_later(
                            &sem_field.ty,
                            ed_val_info,
                            self.storage,
                            self.map,
                            "Q6c-AccountingBalanceAndTurnoversExtDimensionLayout",
                        )?;
                        if check.is_none() {
                            check = Some(slot_check);
                        }
                        slots.push(AccountingBalanceAndTurnoversExtSlotL {
                            ordinal: ord,
                            output,
                            values: None,
                        });
                    }
                    let value_check =
                        check.ok_or_else(|| fatal("missing ExtDimension value check"))?;
                    (
                        slots,
                        Vec::new(),
                        Some(dk),
                        Some(kind_ref),
                        value_check,
                        Some(Box::new(ed_desc)),
                    )
                };

            Some(AccountingBalanceAndTurnoversExtL {
                section,
                owner: sec_owner,
                ordinal: sec_ordinal,
                turnover_only: sec_turnover_only,
                dim_kind,
                kind_ref_table,
                requested_kinds: requested_ext.map(|(_, ids)| ids),
                physical_values,
                ext_flag_fields,
                slots,
                value_check,
                acc_rg_ed,
            })
        } else {
            None
        };

        let secured = self.b.security.accounting_sources.contains_key(&v.src);
        let areas = self
            .b
            .security
            .balance_areas
            .get(&v.src)
            .cloned()
            .unwrap_or_default();
        let mut area_tables = vec![inputs.main, inputs.options, inputs.chart];
        if let Some(ext) = &ext_dims {
            area_tables.push(ext.section);
            if let Some(ed) = &ext.acc_rg_ed {
                area_tables.push(ed.table);
            }
        }
        if super::balance_validate_areas_static(self.schema, self.map, &area_tables)?
            && matches!(areas, BalanceAreasL::None)
        {
            return Err(areas_later());
        }

        let (mut chart_input, mut chart_layout) = self.accounting_chart(v, &inputs)?;
        let dim_count = dims_meta.len();
        let ext_count = ext_dims.as_ref().map_or(0, |e| e.slots.len());
        let raw_dims = 2 + dim_count + ext_count;
        let base = raw::width(raw_dims, resources.len());
        p16(base as i64)?;
        let policy_fields = self.accounting_policy_fields(
            v,
            base + usize::from(chart_layout.is_some_and(|l| l.signal.is_some())),
        )?;
        let chart_signal = chart_layout
            .and_then(|l| l.signal)
            .map(|_| p16(base as i64))
            .transpose()?;

        let account_qf = table
            .fields
            .iter()
            .enumerate()
            .find(|(_, f)| f.role == FieldRole::Std(StdRole::Account))
            .and_then(|(idx, _)| u16::try_from(idx).ok().map(QFieldId));
        let post_cond: Option<Expr> = if let (Some(cond), Some(acct_qf)) = (&v.cond, account_qf)
            && !secured
            && chart_input.is_none()
        {
            let mut acct_conjuncts: Vec<&Expr> = Vec::new();
            let mut rest_conjuncts: Vec<&Expr> = Vec::new();
            for c in bt_conjuncts(cond) {
                if is_pure_account_conjunct(c, v.inner, acct_qf) {
                    acct_conjuncts.push(c);
                } else {
                    rest_conjuncts.push(c);
                }
            }
            if !acct_conjuncts.is_empty() {
                let acct_pred = combine_bt_conjuncts(&acct_conjuncts, &cond.ty);
                let base_chart_rel = match &areas {
                    BalanceAreasL::Exact(t) if !t.chart.is_empty() => {
                        let mut scan_fields = vec![inputs.chart_id, inputs.chart_kind];
                        let mut sep_preds = Vec::with_capacity(t.chart.len());
                        for entry in &t.chart {
                            let col = if let Some(pos) =
                                scan_fields.iter().position(|f| *f == entry.field)
                            {
                                pos
                            } else {
                                scan_fields.push(entry.field);
                                scan_fields.len() - 1
                            };
                            sep_preds.push(PExpr::Compare(
                                CmpOp::Eq,
                                Box::new(PExpr::Col(p16(col as i64)?)),
                                Box::new(PExpr::Lit(entry.value.clone())),
                            ));
                        }
                        let pred = if sep_preds.len() == 1 {
                            sep_preds.pop().unwrap_or(PExpr::Lit(QValue::Bool(true)))
                        } else {
                            PExpr::And(sep_preds)
                        };
                        RelL::Project {
                            input: Box::new(RelL::Filter {
                                input: Box::new(RelL::Scan {
                                    table: inputs.chart,
                                    fields: scan_fields,
                                }),
                                pred,
                            }),
                            exprs: vec![PExpr::Col(0), PExpr::Col(1)],
                        }
                    }
                    _ => RelL::Scan {
                        table: inputs.chart,
                        fields: vec![inputs.chart_id, inputs.chart_kind],
                    },
                };
                let chart_row = Row {
                    signal: None,
                    width: 2,
                    rel: base_chart_rel,
                    rep: Some(v.inner),
                    keys: None,
                };
                let outer_log = self.chain_log.replace(Vec::new());
                self.vt_raw_cols.insert(v.inner, vec![Some(acct_qf), None]);
                self.set_level(v.inner);
                self.inner += 1;
                let filtered_chart = self.filter_row(chart_row, &acct_pred);
                self.inner -= 1;
                self.vt_raw_cols.remove(&v.inner);
                let log = std::mem::replace(&mut self.chain_log, outer_log).unwrap_or_default();
                for (node, old) in log.into_iter().rev() {
                    match old {
                        Some(c) => {
                            self.chains.insert(node, c);
                        }
                        None => {
                            self.chains.remove(&node);
                        }
                    }
                }
                let filtered_chart = filtered_chart?;
                let chart_rel = if filtered_chart.width > 2 {
                    RelL::Project {
                        input: Box::new(filtered_chart.rel),
                        exprs: vec![PExpr::Col(0), PExpr::Col(1)],
                    }
                } else {
                    filtered_chart.rel
                };
                chart_input = Some(Box::new(chart_rel));
                chart_layout = Some(AccountingChartLayoutL {
                    id: 0,
                    kind: 1,
                    signal: None,
                });
                if rest_conjuncts.is_empty() {
                    None
                } else {
                    Some(combine_bt_conjuncts(&rest_conjuncts, &cond.ty))
                }
            } else {
                v.cond.clone()
            }
        } else {
            v.cond.clone()
        };

        let raw_cols = if post_cond.is_some() {
            let mut cols = vec![None; raw::DIMS + raw_dims];
            *cols
                .get_mut(raw::DIMS)
                .ok_or_else(|| fatal("raw account column offset"))? = account_qf;
            for (pos, dim) in dims_meta.iter().enumerate() {
                let qf = table
                    .fields
                    .iter()
                    .enumerate()
                    .find(|(_, f)| f.role == FieldRole::Dimension && f.md == Some(dim.metadata))
                    .and_then(|(idx, _)| u16::try_from(idx).ok().map(QFieldId));
                *cols
                    .get_mut(raw::DIMS + 2 + pos)
                    .ok_or_else(|| fatal("raw dimension column offset"))? = qf;
            }
            for (pos, &ord) in ext_ordinals.iter().enumerate() {
                let qf = table
                    .fields
                    .iter()
                    .enumerate()
                    .find(|(_, f)| f.role == FieldRole::Std(StdRole::ExtDimension(ord)))
                    .and_then(|(idx, _)| u16::try_from(idx).ok().map(QFieldId));
                *cols
                    .get_mut(raw::DIMS + 2 + dim_count + pos)
                    .ok_or_else(|| fatal("raw ext dimension column offset"))? = qf;
            }
            Some(cols)
        } else {
            None
        };

        let register = self
            .schema
            .table(v.register)
            .object
            .ok_or_else(|| fatal("missing register UUID"))?;
        let res_decs: Vec<Dec> = resources.iter().map(|r| r.dec).collect();
        let res_metas: Vec<Guid> = resources.iter().map(|r| r.metadata).collect();
        let dim_metas: Vec<Guid> = dims_meta.iter().map(|d| d.metadata).collect();

        let rows_rel = RelL::RegisterRows(Box::new(RegisterRowsL {
            register,
            kind: v.kind,
            view: v.span,
            rows: RegisterRowsKindL::AccountingBalanceAndTurnovers(
                AccountingBalanceAndTurnoversL {
                    inputs,
                    areas,
                    chart_input,
                    chart_layout,
                    policy_fields,
                    secured,
                    dims: dims_meta,
                    ext_dims,
                    resources,
                    bounds: bounds.clone(),
                    period,
                    recorder,
                    line,
                    periodic,
                    need_opening,
                    need_turnovers,
                    correspondence: corr,
                    record_kind,
                    account_filter: None,
                },
            ),
        }));
        let mut rows = self.accounting_secure(v, rows_rel, base, chart_signal)?;
        if let (Some(cond), Some(raw_cols)) = (&post_cond, raw_cols) {
            self.vt_raw_cols.insert(v.inner, raw_cols);
            self.set_level(v.inner);
            rows.rep = Some(v.inner);
            self.inner += 1;
            let filtered = self.filter_row(rows, cond);
            self.inner -= 1;
            rows = filtered?;
        }

        if periodic {
            self.lower_periodic_bt(
                v,
                rows.checked(),
                &outer_used,
                &used_period,
                outer_has_account,
                &dim_metas,
                &ext_ordinals,
                &res_metas,
                &res_decs,
                raw_dims,
                bounds,
                any_resource_selected,
            )
        } else {
            self.lower_nonperiodic_bt(
                v,
                rows,
                &outer_used,
                outer_has_account,
                &dim_metas,
                &ext_ordinals,
                &res_metas,
                &res_decs,
                raw_dims,
                any_resource_selected,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_nonperiodic_bt(
        &mut self,
        v: &RegisterVtS,
        rows: Row,
        outer_used: &[QFieldId],
        outer_has_account: bool,
        dim_metas: &[Guid],
        ext_ordinals: &[u8],
        res_metas: &[Guid],
        res_decs: &[Dec],
        raw_dims: usize,
        any_resource_selected: bool,
    ) -> R<Row> {
        let table = self.schema.table(v.table);
        let meta = self
            .schema
            .meta(v.register)
            .ok_or_else(|| fatal("missing register metadata"))?;
        let outer_ids: HashSet<Guid> = outer_used
            .iter()
            .filter_map(|id| self.schema.field(v.table, *id).and_then(|f| f.md))
            .collect();
        let outer_ext: HashSet<u8> = outer_used
            .iter()
            .filter_map(|id| match self.schema.field(v.table, *id)?.role {
                FieldRole::Std(StdRole::ExtDimension(n)) => Some(n),
                _ => None,
            })
            .collect();

        let mut group_keys = Vec::new();
        if outer_has_account {
            group_keys.push(PExpr::Col(p16(raw::DIMS as i64)?));
        }
        let mut dim_key_pos = vec![None; dim_metas.len()];
        for (pos, md) in dim_metas.iter().enumerate() {
            if outer_ids.contains(md) {
                let idx = p16(group_keys.len() as i64)?;
                if let Some(slot) = dim_key_pos.get_mut(pos) {
                    *slot = Some(idx);
                }
                group_keys.push(PExpr::Col(p16((raw::DIMS + 2 + pos) as i64)?));
            }
        }
        let mut ext_key_pos = vec![None; ext_ordinals.len()];
        for (pos, &ord) in ext_ordinals.iter().enumerate() {
            if outer_ext.contains(&ord) {
                let idx = p16(group_keys.len() as i64)?;
                if let Some(slot) = ext_key_pos.get_mut(pos) {
                    *slot = Some(idx);
                }
                group_keys.push(PExpr::Col(p16(
                    (raw::DIMS + 2 + dim_metas.len() + pos) as i64
                )?));
            }
        }

        let nk = group_keys.len();
        let res_base = nk + usize::from(outer_has_account);
        let nr = res_metas.len();
        let mut aggs =
            Vec::with_capacity(usize::from(outer_has_account) + nr * raw::PER_RESOURCE + 1);
        if outer_has_account {
            aggs.push(AggL {
                f: Func::Max,
                distinct: false,
                arg: Some(PExpr::Col(p16((raw::DIMS + 1) as i64)?)),
                ty: QType::num(NumQ {
                    digits: 1,
                    scale: 0,
                    nonneg: true,
                }),
                arg_dec: Dec { p: 1, s: 0 },
            });
        }
        for (r, dec) in res_decs.iter().enumerate() {
            let ty = QType::num(NumQ {
                digits: 38,
                scale: dec.s,
                nonneg: false,
            })
            .with_null();
            for c in 0..raw::PER_RESOURCE {
                aggs.push(AggL {
                    f: Func::Sum,
                    distinct: false,
                    arg: Some(PExpr::Col(p16(raw::component(raw_dims, r, c) as i64)?)),
                    ty: ty.clone(),
                    arg_dec: *dec,
                });
            }
        }
        let agg_col = |r: usize, c: usize| -> R<PExpr> {
            Ok(PExpr::Col(p16(
                (res_base + r * raw::PER_RESOURCE + c) as i64
            )?))
        };
        let kind_expr = outer_has_account
            .then(|| p16(nk as i64).map(PExpr::Col))
            .transpose()?;

        let mut projection = vec![PExpr::Lit(QValue::Null); table.fields.len()];
        let mut wanted_ors: Vec<PExpr> = Vec::new();
        let mut seen_wanted: HashSet<(usize, usize)> = HashSet::new();
        let mut push_wanted = |r: usize, c: usize| -> R<()> {
            if seen_wanted.insert((r, c)) {
                wanted_ors.push(PExpr::Compare(
                    CmpOp::Ne,
                    Box::new(agg_col(r, c)?),
                    Box::new(zero()),
                ));
            }
            Ok(())
        };

        for id in outer_used {
            let field = self
                .schema
                .field(v.table, *id)
                .ok_or_else(|| fatal("missing field"))?;
            let expr = if field.role == FieldRole::Std(StdRole::Account) {
                PExpr::Col(0)
            } else if field.role == FieldRole::Dimension {
                let md = field.md.ok_or_else(|| fatal("missing dimension md"))?;
                let pos = dim_metas
                    .iter()
                    .position(|d| *d == md)
                    .ok_or_else(|| fatal("unresolved dimension output"))?;
                let key_col = dim_key_pos
                    .get(pos)
                    .copied()
                    .flatten()
                    .ok_or_else(|| fatal("unresolved dimension key offset"))?;
                PExpr::Col(key_col)
            } else if let FieldRole::Std(StdRole::ExtDimension(n)) = field.role {
                let pos = ext_ordinals
                    .iter()
                    .position(|&ord| ord == n)
                    .ok_or_else(|| fatal("unresolved ExtDimension output"))?;
                let key_col = ext_key_pos
                    .get(pos)
                    .copied()
                    .flatten()
                    .ok_or_else(|| fatal("unresolved ExtDimension key offset"))?;
                PExpr::Col(key_col)
            } else if let Some(md) = field.md {
                let r = res_metas
                    .iter()
                    .position(|m| *m == md)
                    .ok_or_else(|| fatal("unresolved resource output"))?;
                let item = meta
                    .resources
                    .iter()
                    .find(|x| x.id == md)
                    .ok_or_else(|| fatal("unknown resource"))?;
                let comp = BtComponent::parse(&field.name_ru, &item.name)
                    .ok_or_else(|| later("Q6c-AccountingBalanceAndTurnoversColumns"))?;
                match comp.raw_component() {
                    raw::BALANCE => push_wanted(r, raw::BALANCE)?,
                    raw::RECEIPT => push_wanted(r, raw::RECEIPT)?,
                    raw::EXPENSE => push_wanted(r, raw::EXPENSE)?,
                    raw::TURNOVER => push_wanted(r, raw::TURNOVER)?,
                    raw::CLOSING => {
                        push_wanted(r, raw::BALANCE)?;
                        push_wanted(r, raw::TURNOVER)?;
                        push_wanted(r, raw::CLOSING)?;
                    }
                    _ => {}
                }
                let raw_val = zero_if_null(agg_col(r, comp.raw_component())?);
                balance_expr(comp, raw_val, kind_expr.as_ref())
            } else {
                PExpr::Lit(QValue::Null)
            };
            if let Some(slot) = projection.get_mut(usize::from(id.0)) {
                *slot = expr;
            }
        }
        if !any_resource_selected {
            for r in 0..nr {
                for c in [raw::BALANCE, raw::RECEIPT, raw::EXPENSE, raw::CLOSING] {
                    push_wanted(r, c)?;
                }
            }
        }

        if let Some(signal) = rows.signal {
            projection.push(PExpr::Col(p16((nk + aggs.len()) as i64)?));
            aggs.push(super::security::min(signal));
        }
        let signal = rows
            .signal
            .map(|_| p16((projection.len() - 1) as i64))
            .transpose()?;
        let grouped = RelL::Aggregate {
            input: Box::new(rows.rel),
            keys: group_keys,
            sets: None,
            aggs,
        };
        let filtered = if nk == 0 || wanted_ors.is_empty() {
            grouped
        } else {
            RelL::Filter {
                input: Box::new(grouped),
                pred: PExpr::Or(wanted_ors),
            }
        };
        self.added = self.added.saturating_add(12);
        self.set_level(v.src);
        Ok(Row {
            signal,
            width: projection.len(),
            rel: RelL::Project {
                input: Box::new(filtered),
                exprs: projection,
            },
            rep: Some(v.src),
            keys: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_periodic_bt(
        &mut self,
        v: &RegisterVtS,
        mut input: Row,
        outer_used: &[QFieldId],
        used_period: &[(QFieldId, PeriodField)],
        outer_has_account: bool,
        dim_metas: &[Guid],
        ext_ordinals: &[u8],
        res_metas: &[Guid],
        res_decs: &[Dec],
        raw_dims: usize,
        bounds: [Option<PExpr>; 2],
        any_resource_selected: bool,
    ) -> R<Row> {
        let table = self.schema.table(v.table);
        let meta = self
            .schema
            .meta(v.register)
            .ok_or_else(|| fatal("missing register metadata"))?;
        let col = |i: usize| -> R<PExpr> { p16(i as i64).map(PExpr::Col) };
        let outer_ids: HashSet<Guid> = outer_used
            .iter()
            .filter_map(|id| self.schema.field(v.table, *id).and_then(|f| f.md))
            .collect();
        let outer_ext: HashSet<u8> = outer_used
            .iter()
            .filter_map(|id| match self.schema.field(v.table, *id)?.role {
                FieldRole::Std(StdRole::ExtDimension(n)) => Some(n),
                _ => None,
            })
            .collect();

        let mut key_sources: Vec<usize> = Vec::new();
        let mut dim_key_pos = vec![None; dim_metas.len()];
        let mut ext_key_pos = vec![None; ext_ordinals.len()];
        if outer_has_account {
            key_sources.push(raw::DIMS);
            key_sources.push(raw::DIMS + 1);
        }
        for (pos, md) in dim_metas.iter().enumerate() {
            if outer_ids.contains(md) {
                if let Some(slot) = dim_key_pos.get_mut(pos) {
                    *slot = Some(key_sources.len());
                }
                key_sources.push(raw::DIMS + 2 + pos);
            }
        }
        for (pos, &ord) in ext_ordinals.iter().enumerate() {
            if outer_ext.contains(&ord) {
                if let Some(slot) = ext_key_pos.get_mut(pos) {
                    *slot = Some(key_sources.len());
                }
                key_sources.push(raw::DIMS + 2 + dim_metas.len() + pos);
            }
        }
        let nd = key_sources.len();
        let nr = res_metas.len();
        if nr == 0 {
            return internal("periodic accounting register without resources");
        }

        let (has_recorder, has_line, period_expr) = match v.periodicity {
            Periodicity::Unit(kw) => (false, false, begin_of_period(kw, col(0)?)),
            Periodicity::Recorder => (true, false, col(0)?),
            Periodicity::Record => (true, true, col(0)?),
            Periodicity::Auto => {
                let line = used_period
                    .iter()
                    .any(|(_, p)| *p == PeriodField::LineNumber);
                let rec = line || used_period.iter().any(|(_, p)| *p == PeriodField::Recorder);
                let units: Vec<Kw> = used_period
                    .iter()
                    .filter_map(|(_, p)| match p {
                        PeriodField::Unit(kw) => Some(*kw),
                        _ => None,
                    })
                    .collect();
                let period = if rec {
                    col(0)?
                } else {
                    let Some(unit) = [
                        Kw::Second,
                        Kw::Minute,
                        Kw::Hour,
                        Kw::Day,
                        Kw::Week,
                        Kw::TenDays,
                        Kw::Month,
                        Kw::Quarter,
                        Kw::HalfYear,
                        Kw::Year,
                    ]
                    .into_iter()
                    .find(|kw| units.contains(kw)) else {
                        return internal("Auto running balance without period fields");
                    };
                    begin_of_period(unit, PExpr::Col(0))
                };
                (rec, line, period)
            }
            Periodicity::Absent => return internal("nonperiodic running balance"),
        };

        let start = match &bounds[0] {
            Some(PExpr::Lit(val)) => {
                super::vt::bound_date(val).map(|d| PExpr::Lit(QValue::Date(d)))
            }
            Some(e) => Some(e.clone()),
            None => None,
        };
        let cut = |e: PExpr| match &start {
            Some(s) => max_expr(e, s.clone()),
            None => e,
        };
        let mut period_fields = Vec::with_capacity(used_period.len());
        for (_, p) in used_period {
            period_fields.push(match p {
                PeriodField::Period => col(0)?,
                PeriodField::Unit(kw) => cut(begin_of_period(*kw, col(0)?)),
                PeriodField::Recorder => col(1)?,
                PeriodField::LineNumber => col(2)?,
            });
        }

        let mut keys = vec![
            opening_or(cut(period_expr.clone())),
            if has_recorder {
                opening_or(col(1)?)
            } else {
                PExpr::Lit(QValue::Undefined)
            },
            if has_line {
                opening_or(col(2)?)
            } else {
                zero()
            },
        ];
        for &src_col in &key_sources {
            keys.push(col(src_col)?);
        }
        for (_, p) in used_period {
            keys.push(opening_or(match p {
                PeriodField::Period => cut(period_expr.clone()),
                PeriodField::Unit(kw) => cut(begin_of_period(*kw, period_expr.clone())),
                PeriodField::Recorder => {
                    if has_recorder {
                        col(1)?
                    } else {
                        PExpr::Lit(QValue::Undefined)
                    }
                }
                PeriodField::LineNumber => {
                    if has_line {
                        col(2)?
                    } else {
                        zero()
                    }
                }
            }));
        }
        let nk = keys.len();
        let mut aggs = Vec::with_capacity(nr * raw::PER_RESOURCE);
        for (r, dec) in res_decs.iter().enumerate() {
            let ty = QType::num(NumQ {
                digits: 38,
                scale: dec.s,
                nonneg: false,
            })
            .with_null();
            for c in 0..raw::PER_RESOURCE {
                aggs.push(AggL {
                    f: Func::Sum,
                    distinct: false,
                    arg: Some(col(raw::component(raw_dims, r, c))?),
                    ty: ty.clone(),
                    arg_dec: *dec,
                });
            }
        }
        input.rel = RelL::Aggregate {
            input: input.take(),
            keys,
            sets: None,
            aggs,
        };
        let mut reorder: Vec<PExpr> = (0..raw::DIMS + nd).map(col).collect::<R<_>>()?;
        reorder.extend(
            (0..nr * raw::PER_RESOURCE)
                .map(|i| col(nk + i))
                .collect::<R<Vec<_>>>()?,
        );
        reorder.extend((raw::DIMS + nd..nk).map(col).collect::<R<Vec<_>>>()?);
        input.rel = RelL::Project {
            input: input.take(),
            exprs: reorder,
        };
        let mut sort: Vec<(PExpr, bool)> = (0..nd)
            .map(|i| col(raw::DIMS + i).map(|e| (e, false)))
            .collect::<R<_>>()?;
        sort.extend(
            (0..3)
                .map(|i| col(i).map(|e| (e, false)))
                .collect::<R<Vec<_>>>()?,
        );
        sort.extend(
            (0..used_period.len())
                .map(|i| col(raw::width(nd, nr) + i).map(|e| (e, false)))
                .collect::<R<Vec<_>>>()?,
        );
        input.rel = RelL::Sort {
            input: input.take(),
            keys: sort,
        };

        let mut wanted: Vec<(usize, usize)> = Vec::new();
        for id in outer_used {
            let Some(field) = self.schema.field(v.table, *id) else {
                continue;
            };
            let Some(md) = field.md else { continue };
            let Some(r) = res_metas.iter().position(|m| *m == md) else {
                continue;
            };
            let Some(item) = meta.resources.iter().find(|x| x.id == md) else {
                continue;
            };
            if let Some(comp) = BtComponent::parse(&field.name_ru, &item.name) {
                let rc = comp.raw_component();
                if !wanted.contains(&(r, rc)) {
                    wanted.push((r, rc));
                }
            }
        }
        if !any_resource_selected || wanted.is_empty() {
            for r in 0..nr {
                for c in [raw::BALANCE, raw::RECEIPT, raw::EXPENSE] {
                    if !wanted.contains(&(r, c)) {
                        wanted.push((r, c));
                    }
                }
            }
        }

        let step = RunningBalanceL {
            dims: nd,
            resources: nr,
            period: period_expr,
            period_fields,
            bounds,
            boundaries: v.complement != Some(Complement::Movements),
            wanted,
            accounting: true,
        };
        input.width = step.width();
        input.rel = RelL::RunningBalance {
            input: input.take(),
            step: Box::new(step),
        };

        let kind_expr = outer_has_account.then(|| col(raw::DIMS + 1)).transpose()?;
        let mut output = vec![PExpr::Lit(QValue::Null); table.fields.len()];
        for id in outer_used {
            let field = self
                .schema
                .field(v.table, *id)
                .ok_or_else(|| fatal("missing output field"))?;
            let expr = if field.role == FieldRole::Std(StdRole::Account) {
                col(raw::DIMS)?
            } else if let Some(j) = used_period.iter().position(|(f, _)| f == id) {
                col(raw::width(nd, nr) + j)?
            } else if field.role == FieldRole::Dimension {
                let md = field.md.ok_or_else(|| fatal("missing dimension md"))?;
                let pos = dim_metas
                    .iter()
                    .position(|d| *d == md)
                    .ok_or_else(|| fatal("unresolved dimension output"))?;
                let k = dim_key_pos
                    .get(pos)
                    .copied()
                    .flatten()
                    .ok_or_else(|| fatal("unresolved dimension key offset"))?;
                col(raw::DIMS + k)?
            } else if let FieldRole::Std(StdRole::ExtDimension(n)) = field.role {
                let pos = ext_ordinals
                    .iter()
                    .position(|&ord| ord == n)
                    .ok_or_else(|| fatal("unresolved ExtDimension output"))?;
                let k = ext_key_pos
                    .get(pos)
                    .copied()
                    .flatten()
                    .ok_or_else(|| fatal("unresolved ExtDimension key offset"))?;
                col(raw::DIMS + k)?
            } else if let Some(md) = field.md {
                let r = res_metas
                    .iter()
                    .position(|m| *m == md)
                    .ok_or_else(|| fatal("unresolved resource output"))?;
                let item = meta
                    .resources
                    .iter()
                    .find(|x| x.id == md)
                    .ok_or_else(|| fatal("unknown resource"))?;
                let comp = BtComponent::parse(&field.name_ru, &item.name)
                    .ok_or_else(|| later("Q6c-AccountingBalanceAndTurnoversColumns"))?;
                let val = col(raw::component(nd, r, comp.raw_component()))?;
                balance_expr(comp, val, kind_expr.as_ref())
            } else {
                PExpr::Lit(QValue::Null)
            };
            if let Some(slot) = output.get_mut(usize::from(id.0)) {
                *slot = expr;
            }
        }
        self.added = self.added.saturating_add(16);
        self.set_level(v.src);
        Ok(Row {
            signal: None,
            rel: RelL::Project {
                input: input.take(),
                exprs: output,
            },
            width: table.fields.len(),
            rep: Some(v.src),
            keys: None,
        })
    }
}
