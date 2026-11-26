use super::layout::{R, Row, p16};
use super::rel::Lower;
use super::{AggL, Dec, PExpr, RegisterRowsKindL, RegisterRowsL, RelL};
use crate::ir::{ColSel, Expr, ExprKind, RegisterVtS, Rel, SrcId, raw_layout as raw};
use crate::schema::{
    AccountingFlagBinding, FieldRole, Item, QFieldId, QTableId, QuerySchema, StdRole,
    StorageBinding, TableKind, VtKind,
};
use crate::{ExecError, NumQ, QType, QValue};
use ibschema::logical::{LType, TypeCode};
use ibschema::names::{Side, StdField, role};
use ibschema::storage::{FieldId, IndexId, StorageMap, TableId, TableInfo};
use mdschema::MdKind;
use query_syntax::{Func, ast::CmpOp};

pub(crate) fn balance_later(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q6a" }
}

pub(crate) fn balance_areas_later() -> ExecError {
    ExecError::Later {
        what: "Q6a-AccountingBalanceAreas",
        stage: "Q7c",
    }
}

fn fatal(what: impl std::fmt::Display) -> ExecError {
    ExecError::Fatal(format!("accounting Balance: {what}"))
}

pub(crate) fn is_accounting_balance(schema: &QuerySchema, table: QTableId) -> bool {
    let t = schema.table(table);
    t.kind == TableKind::Virtual(VtKind::Balance)
        && t.owner
            .is_some_and(|o| schema.table(o).kind == TableKind::Object(MdKind::AccountingRegister))
}

pub(crate) fn balance_chart(schema: &QuerySchema, table: QTableId) -> R<QTableId> {
    let vt = schema.table(table);
    if !is_accounting_balance(schema, table) {
        return Err(fatal("not an accounting Balance source"));
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

pub(crate) fn balance_ext1_section_identity(
    schema: &QuerySchema,
    table: QTableId,
) -> Option<QTableId> {
    if !is_accounting_balance(schema, table) {
        return None;
    }
    let mut accounts = schema
        .table(table)
        .fields
        .iter()
        .filter(|field| field.role == FieldRole::Std(StdRole::Account));
    let account = accounts.next()?;
    if accounts.next().is_some() || account.ty.atom_count() != 1 || account.ty.refs.len() != 1 {
        return None;
    }
    let account_reference = schema.ref_info(*account.ty.refs.first()?);
    if account_reference.kind != MdKind::ChartOfAccounts {
        return None;
    }
    let chart = account_reference.table;
    let owner = schema.table(chart);
    if owner.kind != TableKind::Object(MdKind::ChartOfAccounts) {
        return None;
    }
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
    if nested.owner != Some(chart)
        || nested.kind != TableKind::Tabular
        || nested.object.is_some()
        || nested
            .name_en
            .strip_prefix(owner.name_en.as_str())
            .and_then(|suffix| suffix.strip_prefix('.'))
            != Some("ExtDimensionTypes")
        || owner
            .fields
            .iter()
            .filter(|field| field.nested == Some(section))
            .count()
            != 1
    {
        return None;
    }
    Some(section)
}

pub(crate) fn is_balance_ext_dimension(role: StdRole) -> bool {
    matches!(role, StdRole::ExtDimension(n) if n >= 1)
}

fn balance_ext_declares_chart(schema: &QuerySchema, table: QTableId, standard: &[StdRole]) -> bool {
    schema.table(table).fields.iter().any(|field| {
        matches!(field.role, FieldRole::Std(role) if is_balance_ext_dimension(role) && standard.contains(&role))
            && field
                .ty
                .refs
                .iter()
                .any(|atom| schema.ref_info(*atom).kind == MdKind::ChartOfAccounts)
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct BalanceInputsL {
    pub main: TableId,
    pub options: TableId,
    pub chart: TableId,
    pub chart_id: FieldId,
    pub chart_kind: FieldId,
    pub active: FieldId,
    pub accounts: [FieldId; 2],
    pub ref_table: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceDimL {
    pub metadata: brace::Guid,
    pub main: [FieldId; 2],
    pub nullable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BalanceResourceL {
    pub metadata: brace::Guid,
    pub main: [FieldId; 2],
    pub null_zero: [bool; 2],
    pub ext_flag: Option<u16>,
    pub dec: Dec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountingChartLayoutL {
    pub id: u16,
    pub kind: u16,
    pub signal: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BalanceAreaFieldL {
    pub common_attribute: brace::Guid,
    pub field: FieldId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BalanceAreaExactL {
    pub common_attribute: brace::Guid,
    pub field: FieldId,
    pub value: QValue,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BalanceTableAreasL<T> {
    pub main: Vec<T>,
    pub options: Vec<T>,
    pub chart: Vec<T>,
    pub section: Vec<T>,
    pub ext_dims: Vec<T>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum BalanceAreasL {
    #[default]
    None,
    Exact(BalanceTableAreasL<BalanceAreaExactL>),
    Unseparated(BalanceTableAreasL<BalanceAreaFieldL>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct BalanceRowsL {
    pub ext1: Option<crate::QShared<super::BalanceExt1L>>,
    pub chart_input: Option<Box<RelL>>,
    pub chart_layout: Option<AccountingChartLayoutL>,
    pub policy_fields: Vec<FieldId>,
    pub secured: bool,
    pub areas: BalanceAreasL,
    pub inputs: BalanceInputsL,
    pub dims: Vec<BalanceDimL>,
    pub resources: Vec<BalanceResourceL>,
    pub date: Option<bsl_prim::BslDate>,
    pub period: Option<FieldId>,
}

impl BalanceRowsL {
    pub fn ext_count(&self) -> usize {
        self.ext1.as_ref().map_or(0, |ext| ext.slots.len())
    }
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

pub(crate) fn balance_inputs(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
) -> R<BalanceInputsL> {
    let chart_table = balance_chart(schema, table)?;
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing owner"))?;
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
    let accounts = [
        standard(m, StdField::AccountDt, TypeCode::R)?,
        standard(m, StdField::AccountCt, TypeCode::R)?,
    ];
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
    Ok(BalanceInputsL {
        main,
        options: options_id,
        chart,
        chart_id,
        chart_kind,
        active,
        accounts,
        ref_table,
    })
}

fn separator_int_fits(number: &bsl_prim::Number, digits: u32) -> bool {
    if digits == 0 || digits > 38 {
        return false;
    }
    struct IntDigits {
        limit: u32,
        count: u32,
        leading_zero: bool,
    }
    impl std::fmt::Write for IntDigits {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            for byte in text.bytes() {
                match byte {
                    b'-' => {}
                    b'0' if self.count == 0 && !self.leading_zero => {
                        self.leading_zero = true;
                    }
                    b'0'..=b'9' => {
                        if self.count == self.limit {
                            return Err(std::fmt::Error);
                        }
                        self.count += 1;
                    }
                    _ => return Err(std::fmt::Error),
                }
            }
            Ok(())
        }
    }
    let mut counter = IntDigits {
        limit: digits,
        count: 0,
        leading_zero: false,
    };
    number.write_xml(&mut counter).is_ok()
}

fn validate_table_separators(
    schema: &QuerySchema,
    info: &TableInfo,
    map: &StorageMap,
) -> R<Vec<BalanceAreaFieldL>> {
    let mut fields = Vec::with_capacity(info.separators.len());
    for sep in &info.separators {
        if sep.common_attribute == brace::Guid::NIL
            || !schema.is_data_separator(sep.common_attribute)
            || sep.use_field.is_some()
        {
            return Err(balance_areas_later());
        }
        if fields
            .iter()
            .any(|existing: &BalanceAreaFieldL| existing.common_attribute == sep.common_attribute)
        {
            return Err(balance_areas_later());
        }
        let field = info.field(sep.field).ok_or_else(balance_areas_later)?;
        if field.nullable || field.std.is_some() {
            return Err(balance_areas_later());
        }
        let [ty] = field.types.as_slice() else {
            return Err(balance_areas_later());
        };
        let valid_type = match ty.code {
            TypeCode::N => ty.precision == 0 && ty.length > 0 && ty.length <= 38,
            TypeCode::S => {
                let len = ty.length & !ibschema::logical::VAR;
                ty.precision == 0 && len < ibschema::logical::VAR
            }
            TypeCode::L | TypeCode::T => ty.length == 0 && ty.precision == 0,
            TypeCode::R => {
                ty.ref_kind == 3
                    && map
                        .table_by_name(&ty.ref_table)
                        .and_then(|id| map.ref_number(id))
                        .is_some()
            }
            TypeCode::E | TypeCode::B | TypeCode::V => false,
        };
        if !valid_type {
            return Err(balance_areas_later());
        }
        fields.push(BalanceAreaFieldL {
            common_attribute: sep.common_attribute,
            field: sep.field,
        });
    }
    Ok(fields)
}

fn separator_value_matches(
    value: &QValue,
    info: &TableInfo,
    field: FieldId,
    map: &StorageMap,
) -> bool {
    let Some([ty]) = info.field(field).map(|f| f.types.as_slice()) else {
        return false;
    };
    match (ty.code, value) {
        (TypeCode::N, QValue::Number(n)) => separator_int_fits(n, ty.length),
        (TypeCode::S, QValue::String(s)) => {
            let max_len = ty.length & !ibschema::logical::VAR;
            max_len == 0 || s.len_utf16() <= max_len as usize
        }
        (TypeCode::L, QValue::Bool(_)) => true,
        (TypeCode::T, QValue::Date(d)) => d.frac_ticks() == 0,
        (TypeCode::R, QValue::Ref { table, .. }) => {
            ty.ref_kind == 3
                && map
                    .table_by_name(&ty.ref_table)
                    .and_then(|id| map.ref_number(id))
                    == Some(*table)
        }
        _ => false,
    }
}

pub(crate) fn balance_validate_areas_static(
    schema: &QuerySchema,
    map: &StorageMap,
    tables: &[TableId],
) -> R<bool> {
    let mut has_any = false;
    for &id in tables {
        let info = map
            .try_table(id)
            .ok_or_else(|| fatal("missing required input"))?;
        if !info.separators.is_empty() {
            validate_table_separators(schema, info, map)?;
            has_any = true;
        }
    }
    Ok(has_any)
}

pub(crate) fn balance_bind_areas(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    with_section: bool,
    filters: &[crate::host::SeparatorFilter],
) -> R<BalanceAreasL> {
    let inputs = balance_inputs(schema, storage, map, table)?;
    let section_id = if with_section {
        Some(balance_ext_section_storage(schema, storage, map, table, &inputs)?.section)
    } else {
        None
    };
    let ed_id = if with_section {
        balance_ext_ed_table(schema, map, table)?
    } else {
        None
    };
    let table_fields = |id: TableId| -> R<Vec<BalanceAreaFieldL>> {
        let info = map
            .try_table(id)
            .ok_or_else(|| fatal("missing required input"))?;
        validate_table_separators(schema, info, map)
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
            let filter = matches.next().ok_or_else(balance_areas_later)?;
            if matches.next().is_some()
                || !separator_value_matches(&filter.value, info, entry.field, map)
            {
                return Err(balance_areas_later());
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

fn nonbalanced_refusal(item: &Item) -> Option<&'static str> {
    if item.balance {
        return None;
    }
    let facts = item.accounting_flags;
    if !facts.parent_valid
        || facts.accounting == AccountingFlagBinding::Unresolved
        || facts.ext_dimension == AccountingFlagBinding::Unresolved
    {
        return Some("Q6a-AccountingBalanceNonBalancedFlagBinding");
    }
    if matches!(facts.ext_dimension, AccountingFlagBinding::Present { .. }) {
        return Some("Q6a-AccountingBalanceNonBalancedExtDimensionFlag");
    }
    None
}

pub(crate) fn balance_dims(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
    used: &[brace::Guid],
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
            if meta.resources.iter().any(|r| r.id == *metadata) {
                continue;
            }
            return Err(balance_later("Q6a-AccountingBalanceColumns"));
        };
        if let Some(what) = nonbalanced_refusal(item) {
            return Err(balance_later(what));
        }
        let (fields, nullable) = if item.balance {
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
            if field.nullable {
                let facts = item.accounting_flags;
                if !facts.parent_valid
                    || facts.accounting == AccountingFlagBinding::Unresolved
                    || facts.ext_dimension == AccountingFlagBinding::Unresolved
                {
                    return Err(balance_later("Q6a-AccountingBalanceNonBalancedFlagBinding"));
                }
                if matches!(facts.ext_dimension, AccountingFlagBinding::Present { .. }) {
                    return Err(balance_later(
                        "Q6a-AccountingBalanceNonBalancedExtDimensionFlag",
                    ));
                }
                if !matches!(facts.accounting, AccountingFlagBinding::Present { .. }) {
                    return Err(fatal("dimension nullability contradicts flag provenance"));
                }
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
            let nullable = matches!(
                item.accounting_flags.accounting,
                AccountingFlagBinding::Present { .. }
            );
            for field in [dr, cr] {
                if main
                    .field(field)
                    .is_none_or(|field| field.nullable != nullable)
                {
                    return Err(fatal("dimension nullability contradicts flag provenance"));
                }
            }
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

pub(crate) fn balance_resources(
    schema: &QuerySchema,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
    used: &[brace::Guid],
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
            if meta.dims.iter().any(|d| d.id == *metadata) {
                continue;
            }
            return Err(balance_later("Q6a-AccountingBalanceColumns"));
        };
        let qualifiers = item
            .ty
            .number
            .ok_or_else(|| balance_later("Q6a-AccountingBalanceResourceShape"))?;
        if !item.ty.only_number()
            || qualifiers.digits == 0
            || qualifiers.digits > 38
            || qualifiers.scale > qualifiers.digits
        {
            return Err(balance_later("Q6a-AccountingBalanceResourceShape"));
        }
        if let Some(what) = nonbalanced_refusal(item) {
            return Err(balance_later(what));
        }
        let (fields, null_zero) = if item.balance {
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
            if field.nullable {
                return Err(balance_later("Q6a-AccountingBalanceNullableResource"));
            }
            let common = FieldId(u16::try_from(index).map_err(|_| fatal("field index overflow"))?);
            ([common, common], [false; 2])
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
            if !matches!(stored.types.as_slice(), [t] if t.code == TypeCode::N && t.length == u32::from(qualifiers.digits) && t.precision == u32::from(qualifiers.scale))
            {
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

pub(crate) fn balance_form(
    schema: &QuerySchema,
    table: QTableId,
    all: bool,
    standard: &[StdRole],
    resources: &[brace::Guid],
    phase: crate::bind::PhaseContext<'_>,
) -> R<()> {
    if all {
        return Err(balance_later("Q6a-AccountingBalanceStar"));
    }
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing owner"))?;
    let meta = schema
        .meta(owner)
        .ok_or_else(|| fatal("missing metadata"))?;
    if !meta.corr || meta.adjustment {
        return Err(balance_later("Q6a-AccountingBalanceRegisterShape"));
    }
    if standard.iter().any(|s| {
        !matches!(
            s,
            StdRole::Account
                | StdRole::AccountDr
                | StdRole::AccountCr
                | StdRole::Active
                | StdRole::Period
        ) && !(phase.enabled()
            && matches!(s, StdRole::ExtDimension(n) if (1..=meta.ext_dims).contains(n)))
    }) {
        return Err(balance_later("Q6a-AccountingBalanceColumns"));
    }
    let mut has_dim = false;
    let mut has_resource = false;
    for md in resources {
        if meta.dims.iter().any(|d| d.id == *md) {
            has_dim = true;
        } else if meta.resources.iter().any(|r| r.id == *md) {
            has_resource = true;
        } else {
            return Err(balance_later("Q6a-AccountingBalanceColumns"));
        }
    }
    let has_account = standard.contains(&StdRole::Account);
    let has_ext = phase.enabled() && standard.iter().any(|&s| is_balance_ext_dimension(s));
    if !has_account && !has_dim && !has_ext {
        return Err(balance_later("Q6a-AccountingBalanceAccountRequired"));
    }
    if !has_resource {
        return Err(balance_later("Q6a-AccountingBalanceNoResource"));
    }
    if has_ext && balance_ext_declares_chart(schema, table, standard) {
        return Err(balance_later("Q6a-AccountingBalanceExtDimensionChart"));
    }
    Ok(())
}

fn component(name: &str, item: &str) -> Option<usize> {
    [
        "Остаток",
        "ОстатокДт",
        "ОстатокКт",
        "РазвернутыйОстатокДт",
        "РазвернутыйОстатокКт",
    ]
    .iter()
    .position(|suffix| name == format!("{item}{suffix}"))
}

impl Lower<'_> {
    fn balance_ext_kind_ref_table(&self, table: QTableId) -> R<u32> {
        let section_logical = balance_ext1_section_identity(self.schema, table)
            .ok_or_else(|| balance_later("Q6a-AccountingBalanceArguments"))?;
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

    fn resolve_balance_ext_dims_arg(
        &self,
        v: &RegisterVtS,
        max_ext_dims: u8,
    ) -> R<Option<(u32, Vec<[u8; 16]>)>> {
        let Some(expr) = &v.ext_dims_arg else {
            return Ok(None);
        };
        if max_ext_dims == 0 {
            return Err(balance_later("Q6a-AccountingBalanceArguments"));
        }
        let kind_ref_table = self.balance_ext_kind_ref_table(v.table)?;
        let single;
        let slice: &[QValue] = match &expr.kind {
            crate::ir::ExprKind::Value(idx) => {
                let val = self
                    .b
                    .values
                    .get(usize::from(*idx))
                    .ok_or_else(|| fatal("missing bound ExtDimensions value"))?;
                single = [val.clone()];
                &single
            }
            crate::ir::ExprKind::Param(slot) => match self.b.params.get(usize::from(*slot)) {
                Some(crate::bind::PBound::Value(val)) => {
                    single = [val.clone()];
                    &single
                }
                Some(crate::bind::PBound::Set(set)) => &set.values,
                _ => return Err(balance_later("Q6a-AccountingBalanceArguments")),
            },
            _ => return Err(balance_later("Q6a-AccountingBalanceArguments")),
        };
        if slice.is_empty() || slice.len() > usize::from(max_ext_dims) {
            return Err(balance_later("Q6a-AccountingBalanceArguments"));
        }
        let mut ids = Vec::with_capacity(slice.len());
        for value in slice {
            let QValue::Ref { table, id } = value else {
                return Err(balance_later("Q6a-AccountingBalanceArguments"));
            };
            if *table != kind_ref_table || *id == [0; 16] || ids.contains(id) {
                return Err(balance_later("Q6a-AccountingBalanceArguments"));
            }
            ids.push(*id);
        }
        Ok(Some((kind_ref_table, ids)))
    }

    pub(super) fn accounting_balance(&mut self, v: &RegisterVtS) -> R<Row> {
        self.accounting_consumer(v, true)?;
        let outer_used = self
            .vt_used
            .get(v.src.0 as usize)
            .cloned()
            .unwrap_or_default();
        let cond_used = self
            .vt_used
            .get(v.inner.0 as usize)
            .cloned()
            .unwrap_or_default();
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
        balance_form(
            self.schema,
            v.table,
            self.stmt
                .sources
                .get(v.src.0 as usize)
                .is_some_and(|s| s.all_fields),
            &all_roles,
            &all_ids,
            self.phase,
        )?;
        let outer_has_account = outer_roles.contains(&StdRole::Account);
        let outer_has_dim = outer_ids
            .iter()
            .any(|md| meta.dims.iter().any(|d| d.id == *md));
        let outer_has_ext =
            self.phase.enabled() && outer_roles.iter().any(|&s| is_balance_ext_dimension(s));
        if !outer_has_account && !outer_has_dim && !outer_has_ext {
            return Err(balance_later("Q6a-AccountingBalanceAccountRequired"));
        }
        let requested_ext = self.resolve_balance_ext_dims_arg(v, meta.ext_dims)?;
        let ext_ordinals: Vec<u8> = (1..=meta.ext_dims)
            .filter(|n| all_roles.contains(&StdRole::ExtDimension(*n)))
            .collect();
        let inputs = balance_inputs(self.schema, self.storage, self.map, v.table)?;
        let dates = self.accounting_date_bounds(&v.bounds)?;
        let date = dates[0];
        let period = if date.is_some() {
            Some(standard(
                self.map.table(inputs.main),
                StdField::Period,
                TypeCode::T,
            )?)
        } else {
            None
        };
        let dims_meta = balance_dims(
            self.schema,
            self.storage,
            self.map,
            v.table,
            &inputs,
            &all_ids,
        )?;
        let mut resources = balance_resources(self.schema, self.map, v.table, &inputs, &outer_ids)?;
        if !ext_ordinals.is_empty() {
            balance_ext1_static_inputs(
                self.schema,
                self.storage,
                self.map,
                v.table,
                &inputs,
                &all_roles,
                &all_ids,
            )?;
        }
        let dim_count = dims_meta.len();
        let ext_count = ext_ordinals.len();
        let dims = 2 + dim_count + ext_count;
        let mut group_keys = Vec::new();
        if outer_has_account {
            group_keys.push(PExpr::Col(p16(raw::DIMS as i64)?));
        }
        let mut dim_key_pos = vec![None; dim_count];
        for (pos, dim) in dims_meta.iter().enumerate() {
            if outer_ids.contains(&dim.metadata) {
                let key_idx = p16(group_keys.len() as i64)?;
                *dim_key_pos
                    .get_mut(pos)
                    .ok_or_else(|| fatal("dimension key offset"))? = Some(key_idx);
                group_keys.push(PExpr::Col(p16((raw::DIMS + 2 + pos) as i64)?));
            }
        }
        let mut ext_key_pos = vec![None; ext_count];
        for (pos, &ord) in ext_ordinals.iter().enumerate() {
            if outer_roles.contains(&StdRole::ExtDimension(ord)) {
                let key_idx = p16(group_keys.len() as i64)?;
                *ext_key_pos
                    .get_mut(pos)
                    .ok_or_else(|| fatal("ext dimension key offset"))? = Some(key_idx);
                group_keys.push(PExpr::Col(p16((raw::DIMS + 2 + dim_count + pos) as i64)?));
            }
        }
        let keys = group_keys.len();
        let res_base = keys + usize::from(outer_has_account);
        p16(raw::width(dims, resources.len()) as i64)?;
        let zero = || PExpr::Lit(QValue::Number(bsl_prim::Number::zero()));
        let compare = |op, a, b| PExpr::Compare(op, Box::new(a), Box::new(b));
        let case = |pred, yes| PExpr::Case {
            operand: None,
            whens: vec![(pred, yes)],
            else_: Some(Box::new(zero())),
        };
        let mut aggs = Vec::with_capacity(usize::from(outer_has_account) + resources.len());
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
        for (i, resource) in resources.iter().enumerate() {
            aggs.push(AggL {
                f: Func::Sum,
                distinct: false,
                arg: Some(PExpr::Col(p16(
                    raw::component(dims, i, raw::BALANCE) as i64
                )?)),
                ty: QType::num(NumQ {
                    digits: 38,
                    scale: resource.dec.s,
                    nonneg: false,
                })
                .with_null(),
                arg_dec: resource.dec,
            });
        }
        let kind = PExpr::Col(p16(keys as i64)?);
        let mut projection = vec![PExpr::Lit(QValue::Null); table.fields.len()];
        for id in &outer_used {
            let field = self
                .schema
                .field(v.table, *id)
                .ok_or_else(|| fatal("missing field"))?;
            let expr = if field.role == FieldRole::Std(StdRole::Account) {
                PExpr::Col(0)
            } else if field.role == FieldRole::Dimension {
                let md = field
                    .md
                    .ok_or_else(|| balance_later("Q6a-AccountingBalanceColumns"))?;
                let pos = dims_meta
                    .iter()
                    .position(|d| d.metadata == md)
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
            } else {
                let md = field
                    .md
                    .ok_or_else(|| balance_later("Q6a-AccountingBalanceColumns"))?;
                let index = resources
                    .iter()
                    .position(|r| r.metadata == md)
                    .ok_or_else(|| fatal("unresolved resource output"))?;
                let item = meta
                    .resources
                    .iter()
                    .find(|r| r.id == md)
                    .ok_or_else(|| fatal("unknown resource"))?;
                let c = component(&field.name_ru, &item.name)
                    .ok_or_else(|| balance_later("Q6a-AccountingBalanceColumns"))?;
                let b = PExpr::Col(p16((index + res_base) as i64)?);
                let positive = compare(CmpOp::Gt, b.clone(), zero());
                let negative = compare(CmpOp::Lt, b.clone(), zero());
                let is_kind = |n| {
                    compare(
                        CmpOp::Eq,
                        kind.clone(),
                        PExpr::Lit(QValue::Number(bsl_prim::Number::from(n))),
                    )
                };
                match c {
                    0 => b,
                    1 if outer_has_account => case(
                        PExpr::Or(vec![is_kind(0), PExpr::And(vec![is_kind(2), positive])]),
                        b,
                    ),
                    1 => case(positive, b),
                    2 if outer_has_account => case(
                        PExpr::Or(vec![is_kind(1), PExpr::And(vec![is_kind(2), negative])]),
                        PExpr::Neg(Box::new(b)),
                    ),
                    2 => case(negative, PExpr::Neg(Box::new(b))),
                    3 => case(positive, b),
                    4 => case(negative, PExpr::Neg(Box::new(b))),
                    _ => return Err(fatal("unknown component")),
                }
            };
            *projection
                .get_mut(usize::from(id.0))
                .ok_or_else(|| fatal("projection field offset"))? = expr;
        }
        let having = PExpr::Or(
            (0..resources.len())
                .map(|i| {
                    Ok(compare(
                        CmpOp::Ne,
                        PExpr::Col(p16((i + res_base) as i64)?),
                        zero(),
                    ))
                })
                .collect::<R<Vec<_>>>()?,
        );
        let register = self
            .schema
            .table(v.register)
            .object
            .ok_or_else(|| fatal("missing register UUID"))?;
        let (mut chart_input, mut chart_layout) = self.accounting_chart(v, &inputs)?;
        let base = raw::width(dims, resources.len());
        let policy_fields = self.accounting_policy_fields(
            v,
            base + usize::from(chart_layout.is_some_and(|l| l.signal.is_some())),
        )?;
        let ext1 = if let (false, Some(memory)) = (ext_ordinals.is_empty(), self.phase.memory()) {
            if ext_ordinals.len() > 64 {
                return Err(balance_later("Q6a-AccountingBalanceExtDimensionLayout"));
            }
            let identity =
                balance_ext_section_storage(self.schema, self.storage, self.map, v.table, &inputs)?;
            let mut ext_flag_fields: Vec<FieldId> = Vec::new();
            for resource in &mut resources {
                let item = meta
                    .resources
                    .iter()
                    .find(|item| item.id == resource.metadata)
                    .ok_or_else(|| fatal("missing selected resource metadata"))?;
                if !item.balance {
                    continue;
                }
                match item.accounting_flags.ext_dimension {
                    AccountingFlagBinding::Absent => {}
                    AccountingFlagBinding::Present { chart, child }
                        if item.accounting_flags.parent_valid =>
                    {
                        let field = balance_ext_section_flag_field(
                            self.schema,
                            self.map,
                            v.table,
                            identity.section,
                            chart,
                            child,
                        )?;
                        let idx =
                            if let Some(pos) = ext_flag_fields.iter().position(|f| *f == field) {
                                pos
                            } else {
                                if ext_flag_fields.len() >= 64 {
                                    return Err(balance_later(
                                        "Q6a-AccountingBalanceExtDimensionLayout",
                                    ));
                                }
                                ext_flag_fields.push(field);
                                ext_flag_fields.len() - 1
                            };
                        resource.ext_flag =
                            Some(u16::try_from(idx).map_err(|_| fatal("ext flag index overflow"))?);
                    }
                    _ => {
                        return Err(balance_later(
                            "Q6a-AccountingBalanceExtDimensionAccountingFlag",
                        ));
                    }
                }
            }
            let main = self
                .map
                .try_table(inputs.main)
                .ok_or_else(|| fatal("missing main table"))?;
            let dim_types = dims_meta.iter().flat_map(|dim| {
                let item = meta.dims.iter().find(|item| item.id == dim.metadata);
                let second = (dim.main[1] != dim.main[0]).then_some(dim.main[1]);
                std::iter::once(dim.main[0])
                    .chain(second)
                    .map(move |field| {
                        item.map(|item| (field, &item.ty))
                            .ok_or_else(|| fatal("missing semantic dimension"))
                    })
            });
            let resource_types = resources.iter().flat_map(|resource| {
                let item = meta
                    .resources
                    .iter()
                    .find(|item| item.id == resource.metadata);
                let second = (resource.main[1] != resource.main[0]).then_some(resource.main[1]);
                std::iter::once(resource.main[0])
                    .chain(second)
                    .map(move |field| {
                        item.map(|item| (field, &item.ty))
                            .ok_or_else(|| fatal("missing semantic resource"))
                    })
            });
            let policy_types = policy_fields.iter().map(|field| {
                super::accounting_balance_payload::policy_type(
                    self.schema,
                    self.storage,
                    v.register,
                    *field,
                )
                .map(|ty| (*field, ty))
            });
            if main_has_inline_ext_values(main) {
                let slot_specs = ext_ordinals.iter().map(|&ordinal| {
                    let values = balance_ext_slot_values(main, ordinal)?;
                    let mut fields = table
                        .fields
                        .iter()
                        .filter(|f| f.role == FieldRole::Std(StdRole::ExtDimension(ordinal)));
                    let field = fields
                        .next()
                        .ok_or_else(|| fatal("missing semantic ExtDimension"))?;
                    if fields.next().is_some() {
                        return Err(fatal("duplicate semantic ExtDimension"));
                    }
                    Ok((ordinal, values, &field.ty))
                });
                let ext_remap = if let Some((kind_ref_table, req_ids)) = requested_ext.as_ref() {
                    let section_info = self
                        .map
                        .try_table(identity.section)
                        .ok_or_else(|| fatal("missing subconto storage table"))?;
                    let dim_kind = valid_section_dim_kind(section_info, *kind_ref_table, self.map)?;
                    let mut physical_slots = Vec::with_capacity(usize::from(meta.ext_dims));
                    for phys_ord in 1..=meta.ext_dims {
                        let values = balance_ext_slot_values(main, phys_ord)?;
                        let mut fields = table
                            .fields
                            .iter()
                            .filter(|f| f.role == FieldRole::Std(StdRole::ExtDimension(phys_ord)));
                        let field = fields
                            .next()
                            .ok_or_else(|| fatal("missing semantic ExtDimension"))?;
                        if fields.next().is_some() {
                            return Err(fatal("duplicate semantic ExtDimension"));
                        }
                        physical_slots.push((values, &field.ty));
                    }
                    Some(super::accounting_balance_payload::ExtKindRemap {
                        dim_kind,
                        kind_ref_table: *kind_ref_table,
                        requested_kinds: req_ids.as_slice(),
                        physical_slots,
                    })
                } else {
                    None
                };
                Some(super::accounting_balance_payload::build(
                    (
                        identity.section,
                        identity.owner,
                        identity.ordinal,
                        identity.turnover_only,
                    ),
                    slot_specs,
                    ext_remap,
                    &ext_flag_fields,
                    inputs.main,
                    dim_types.chain(resource_types).chain(policy_types),
                    self.storage,
                    self.map,
                    memory,
                )?)
            } else {
                let first_ordinal = *ext_ordinals
                    .first()
                    .ok_or_else(|| fatal("missing required subconto ordinal"))?;
                let mut fields = table
                    .fields
                    .iter()
                    .filter(|f| f.role == FieldRole::Std(StdRole::ExtDimension(first_ordinal)));
                let semantic_ext = fields
                    .next()
                    .ok_or_else(|| fatal("missing semantic ExtDimension"))?;
                if fields.next().is_some() {
                    return Err(fatal("duplicate semantic ExtDimension"));
                }
                let (ed_desc, dim_kind, kind_ref_table) = balance_ext_ed_storage(
                    self.schema,
                    self.storage,
                    self.map,
                    v.table,
                    &inputs,
                    identity.section,
                    meta.ext_dims,
                    &semantic_ext.ty,
                )?;
                Some(super::accounting_balance_payload::build_ed(
                    (
                        identity.section,
                        identity.owner,
                        identity.ordinal,
                        identity.turnover_only,
                    ),
                    dim_kind,
                    kind_ref_table,
                    ed_desc,
                    &ext_ordinals,
                    requested_ext.as_ref().map(|(_, ids)| ids.as_slice()),
                    &ext_flag_fields,
                    &semantic_ext.ty,
                    inputs.main,
                    dim_types.chain(resource_types).chain(policy_types),
                    self.storage,
                    self.map,
                    memory,
                )?)
            }
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
        if let Some(ext) = &ext1 {
            area_tables.push(ext.section);
            if let Some(ed) = ext.ed {
                area_tables.push(ed.table);
            }
        }
        if balance_validate_areas_static(self.schema, self.map, &area_tables)?
            && matches!(areas, BalanceAreasL::None)
        {
            return Err(balance_areas_later());
        }
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
            for c in balance_conjuncts(cond) {
                if is_pure_balance_account_conjunct(c, v.inner, acct_qf) {
                    acct_conjuncts.push(c);
                } else {
                    rest_conjuncts.push(c);
                }
            }
            if !acct_conjuncts.is_empty() {
                let acct_pred = combine_balance_conjuncts(&acct_conjuncts, &cond.ty);
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
                    Some(combine_balance_conjuncts(&rest_conjuncts, &cond.ty))
                }
            } else {
                v.cond.clone()
            }
        } else {
            v.cond.clone()
        };
        let raw_cols = if post_cond.is_some() {
            let mut cols = vec![None; raw::DIMS + dims];
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
        let rows = RelL::RegisterRows(Box::new(RegisterRowsL {
            register,
            kind: v.kind,
            view: v.span,
            rows: RegisterRowsKindL::AccountingBalance(BalanceRowsL {
                ext1,
                inputs,
                dims: dims_meta,
                resources,
                date,
                period,
                chart_input,
                chart_layout,
                policy_fields,
                secured,
                areas,
            }),
        }));
        let mut rows = self.accounting_secure(v, rows, base, chart_signal)?;
        if let (Some(cond), Some(raw_cols)) = (&post_cond, raw_cols) {
            self.vt_raw_cols.insert(v.inner, raw_cols);
            self.set_level(v.inner);
            rows.rep = Some(v.inner);
            self.inner += 1;
            let filtered = self.filter_row(rows, cond);
            self.inner -= 1;
            rows = filtered?;
        }
        if let Some(signal) = rows.signal {
            projection.push(PExpr::Col(p16((keys + aggs.len()) as i64)?));
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
        self.added = self.added.saturating_add(12);
        self.set_level(v.src);
        Ok(Row {
            signal,
            width: projection.len(),
            rel: RelL::Project {
                input: Box::new(RelL::Filter {
                    input: Box::new(grouped),
                    pred: having,
                }),
                exprs: projection,
            },
            rep: Some(v.src),
            keys: None,
        })
    }
}

fn balance_conjuncts(e: &Expr) -> Vec<&Expr> {
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

fn combine_balance_conjuncts(conjuncts: &[&Expr], ty: &QType) -> Expr {
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
        Rel::Union { parts, .. } => {
            for p in parts {
                collect_rel_sources(p, out);
            }
        }
        Rel::Project { input, .. }
        | Rel::Filter { input, .. }
        | Rel::Aggregate { input, .. }
        | Rel::Distinct(input)
        | Rel::Sort { input, .. }
        | Rel::Top { input, .. }
        | Rel::Hierarchy { input, .. } => collect_rel_sources(input, out),
        Rel::Empty => {}
    }
}

fn is_pure_balance_account_conjunct(e: &Expr, inner: SrcId, account_qf: QFieldId) -> bool {
    let mut saw_account = false;
    let mut ok = true;
    crate::ir::visit_expr_in_place(e, false, &mut |x| {
        if !ok {
            return;
        }
        match &x.kind {
            ExprKind::Col { src, col } => {
                if *src == inner && *col == ColSel::Field(account_qf) {
                    saw_account = true;
                } else {
                    ok = false;
                }
            }
            ExprKind::InQuery { q, .. } => {
                let mut local = Vec::new();
                collect_rel_sources(q, &mut local);
                crate::ir::visit_rel(q, &mut |sub_expr| {
                    if let ExprKind::Col { src, .. } = &sub_expr.kind
                        && !local.contains(src)
                    {
                        ok = false;
                    }
                });
            }
            ExprKind::SectionAgg { .. } => {
                ok = false;
            }
            _ => {}
        }
    });
    ok && saw_account
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BalanceExtSectionStorage {
    section: TableId,
    owner: FieldId,
    ordinal: FieldId,
    turnover_only: FieldId,
}

fn matches_numbered_name(actual: &str, prefix: &str, ordinal: u8) -> bool {
    actual.strip_prefix(prefix).is_some_and(|rest| {
        !rest.starts_with('0')
            && rest.bytes().all(|b| b.is_ascii_digit())
            && rest.parse::<u8>() == Ok(ordinal)
    })
}

fn balance_ext_slot_values(main: &TableInfo, ordinal: u8) -> R<[FieldId; 2]> {
    fn exact_value(info: &TableInfo, std: StdField, prefix: &str, ordinal: u8) -> R<FieldId> {
        let mut fields = info
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| matches_numbered_name(&field.name, prefix, ordinal));
        let (index, field) = fields
            .next()
            .ok_or_else(|| fatal("missing required subconto value"))?;
        if fields.next().is_some() || field.std != Some(std) || field.side.is_some() {
            return Err(fatal("contradictory subconto value identity"));
        }
        u16::try_from(index)
            .map(FieldId)
            .map_err(|_| fatal("field index overflow"))
    }
    Ok([
        exact_value(main, StdField::ValueDt, "ValueDt", ordinal)?,
        exact_value(main, StdField::ValueCt, "ValueCt", ordinal)?,
    ])
}

fn balance_ext_section_storage(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
) -> R<BalanceExtSectionStorage> {
    let logical = balance_ext1_section_identity(schema, table)
        .ok_or_else(|| fatal("missing required subconto section identity"))?;
    let chart = schema
        .table(logical)
        .owner
        .ok_or_else(|| fatal("missing subconto logical owner"))?;
    let chart_md = schema
        .table(chart)
        .object
        .ok_or_else(|| fatal("missing chart metadata identity"))?;
    let object = map
        .object(chart_md)
        .ok_or_else(|| fatal("missing chart storage object"))?;
    let mut lists = object
        .lists
        .iter()
        .filter(|(kind, _)| *kind == ibschema::storage::StdList::ExtDimensionTypes);
    let section = lists
        .next()
        .map(|(_, id)| *id)
        .ok_or_else(|| fatal("missing required subconto storage list"))?;
    if lists.next().is_some()
        || object.main != inputs.chart
        || storage.table(chart) != Some(inputs.chart)
        || storage.table(logical) != Some(section)
        || [inputs.main, inputs.options, inputs.chart].contains(&section)
        || object
            .tabular
            .iter()
            .filter(|(_, id)| *id == section)
            .count()
            != 1
        || !object
            .tabular
            .iter()
            .any(|(md, id)| *md == chart_md && *id == section)
    {
        return Err(fatal("ambiguous or aliased subconto storage binding"));
    }
    let section_info = map
        .try_table(section)
        .ok_or_else(|| fatal("missing subconto storage table"))?;
    if section_info.owner != Some(inputs.chart)
        || section_info.md != Some(chart_md)
        || section_info.purpose != role::TABULAR_SECTION
        || section_info.query_name != schema.table(logical).name_en
    {
        return Err(fatal("contradictory subconto storage ownership"));
    }
    balance_validate_areas_static(
        schema,
        map,
        &[inputs.main, inputs.options, inputs.chart, section],
    )?;
    let owner = standard(section_info, StdField::Id, TypeCode::R)?;
    let owner_ty = ty(section_info, owner)?;
    if owner_ty.ref_kind != 2 || map.table_by_name(&owner_ty.ref_table) != Some(inputs.chart) {
        return Err(fatal("subconto owner reference mismatch"));
    }
    let ordinal = standard(section_info, StdField::LineNo, TypeCode::N)?;
    let ordinal_ty = ty(section_info, ordinal)?;
    if (ordinal_ty.length, ordinal_ty.precision) != (5, 0) {
        return Err(fatal("subconto ordinal qualifiers mismatch"));
    }
    let turnover_only = standard(section_info, StdField::TurnoverOnly, TypeCode::L)?;
    for field in [owner, ordinal, turnover_only] {
        if section_info
            .field(field)
            .is_none_or(|field| field.nullable || field.side.is_some())
        {
            return Err(fatal("invalid required section field flags"));
        }
    }
    Ok(BalanceExtSectionStorage {
        section,
        owner,
        ordinal,
        turnover_only,
    })
}

fn main_has_inline_ext_values(main: &TableInfo) -> bool {
    main.fields.iter().any(|field| {
        matches!(field.std, Some(StdField::ValueDt | StdField::ValueCt))
            || field
                .name
                .strip_prefix("ValueDt")
                .or_else(|| field.name.strip_prefix("ValueCt"))
                .is_some_and(|rest| {
                    !rest.is_empty()
                        && !rest.starts_with('0')
                        && rest.bytes().all(|b| b.is_ascii_digit())
                })
    })
}

pub(crate) fn balance_ext_ed_table(
    schema: &QuerySchema,
    map: &StorageMap,
    table: QTableId,
) -> R<Option<TableId>> {
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing owner"))?;
    let metadata = schema
        .table(owner)
        .object
        .ok_or_else(|| fatal("missing register UUID"))?;
    let object = map
        .object(metadata)
        .ok_or_else(|| fatal("missing register storage object"))?;
    let main_info = map
        .try_table(object.main)
        .ok_or_else(|| fatal("missing main table"))?;
    if main_has_inline_ext_values(main_info) {
        return Ok(None);
    }
    let mut ed_tables = object
        .service
        .iter()
        .filter(|(r, _)| r == role::EXT_DIMENSIONS_VALUES);
    let ed_id = ed_tables
        .next()
        .map(|(_, id)| *id)
        .ok_or_else(|| fatal("missing AccRgED service table"))?;
    if ed_tables.next().is_some() {
        return Err(fatal("ambiguous AccRgED service table"));
    }
    Ok(Some(ed_id))
}

fn valid_section_dim_kind(
    section_info: &TableInfo,
    kind_ref_table: u32,
    map: &StorageMap,
) -> R<FieldId> {
    let dim_kind = standard(section_info, StdField::DimKind, TypeCode::R)?;
    let dk_field = section_info
        .field(dim_kind)
        .ok_or_else(|| fatal("missing section DimKind field"))?;
    if dk_field.nullable || dk_field.side.is_some() {
        return Err(fatal("invalid section DimKind flags"));
    }
    let dk_ty = ty(section_info, dim_kind)?;
    if dk_ty.ref_kind != 3
        || map
            .table_by_name(&dk_ty.ref_table)
            .and_then(|id| map.ref_number(id))
            != Some(kind_ref_table)
    {
        return Err(fatal("section DimKind target mismatch"));
    }
    Ok(dim_kind)
}

fn valid_nonnull_std(info: &TableInfo, std: StdField, code: TypeCode) -> R<FieldId> {
    let field = standard(info, std, code)?;
    let f = info
        .field(field)
        .ok_or_else(|| fatal("missing standard field"))?;
    if f.nullable || f.side.is_some() {
        return Err(fatal(format_args!(
            "invalid flags on {std:?} in {}",
            info.logical
        )));
    }
    Ok(field)
}

fn valid_line_no_field(info: &TableInfo) -> R<FieldId> {
    let field = valid_nonnull_std(info, StdField::LineNo, TypeCode::N)?;
    let t = ty(info, field)?;
    if t.length == 0 || t.precision != 0 {
        return Err(fatal(format_args!(
            "invalid LineNo qualifiers in {}",
            info.logical
        )));
    }
    Ok(field)
}

fn valid_recorder_field(info: &TableInfo, expected_tags: &[u32], map: &StorageMap) -> R<FieldId> {
    let mut matches = info
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.std == Some(StdField::Recorder));
    let (i, f) = matches
        .next()
        .ok_or_else(|| fatal(format_args!("missing Recorder in {}", info.logical)))?;
    if matches.next().is_some() || f.nullable || f.side.is_some() {
        return Err(fatal(format_args!(
            "ambiguous or invalid Recorder in {}",
            info.logical
        )));
    }
    let valid = match f.types.as_slice() {
        [r] if r.code == TypeCode::R && r.ref_kind == 3 => {
            expected_tags.len() == 1
                && map
                    .table_by_name(&r.ref_table)
                    .and_then(|id| map.ref_number(id))
                    == expected_tags.first().copied()
        }
        [r] if r.code == TypeCode::R && r.ref_kind == 4 && r.ref_table.is_empty() => {
            expected_tags.len() != 1
        }
        [e, r]
            if e.code == TypeCode::E
                && e.length == 0
                && e.precision == 0
                && r.code == TypeCode::R
                && r.ref_kind == 4
                && r.ref_table.is_empty() =>
        {
            expected_tags.len() != 1
        }
        _ => false,
    };
    if !valid {
        return Err(fatal(format_args!(
            "Recorder physical type contradicts metadata in {}",
            info.logical
        )));
    }
    u16::try_from(i)
        .map(FieldId)
        .map_err(|_| fatal("field index overflow"))
}

fn find_by_period_index(
    info: &TableInfo,
    period: FieldId,
    recorder: FieldId,
    line: FieldId,
) -> Option<IndexId> {
    let sep_count = info.separators.len();
    let matches_layout = |ix: &ibschema::storage::IndexInfo| -> bool {
        if ix.absent || ix.fields.len() < sep_count + 3 {
            return false;
        }
        let Some(prefix) = ix.fields.get(..sep_count) else {
            return false;
        };
        if !prefix
            .iter()
            .zip(&info.separators)
            .all(|(field, sep)| *field == sep.field)
        {
            return false;
        }
        ix.fields.get(sep_count..sep_count + 3) == Some(&[period, recorder, line])
    };
    let preferred = info
        .indexes
        .iter()
        .enumerate()
        .find(|(_, ix)| ix.name == "ByPeriod" && matches_layout(ix))
        .or_else(|| {
            info.indexes
                .iter()
                .enumerate()
                .find(|(_, ix)| matches_layout(ix))
        });
    preferred.and_then(|(idx, _)| u16::try_from(idx).ok().map(IndexId::Named))
}

#[allow(
    clippy::too_many_arguments,
    reason = "validates AccRgED table, main join fields, and section DimKind against schema and storage"
)]
fn balance_ext_ed_storage(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
    section: TableId,
    max_ext_dims: u8,
    semantic_ext: &QType,
) -> R<(super::BalanceExtEdL, FieldId, u32)> {
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing owner"))?;
    let metadata = schema
        .table(owner)
        .object
        .ok_or_else(|| fatal("missing register UUID"))?;
    let ed_table = balance_ext_ed_table(schema, map, table)?
        .ok_or_else(|| fatal("unexpected inline mode in AccRgED resolver"))?;
    if [inputs.main, inputs.options, inputs.chart, section].contains(&ed_table) {
        return Err(fatal("aliased AccRgED table"));
    }
    let ed_info = map
        .try_table(ed_table)
        .ok_or_else(|| fatal("missing AccRgED table"))?;
    if ed_info.md != Some(metadata) || ed_info.purpose != role::EXT_DIMENSIONS_VALUES {
        return Err(fatal("inconsistent AccRgED ownership or purpose"));
    }
    balance_validate_areas_static(schema, map, &[ed_table])?;

    let section_logical = balance_ext1_section_identity(schema, table)
        .ok_or_else(|| fatal("missing required subconto section identity"))?;
    let mut dk_sem_fields = schema
        .table(section_logical)
        .fields
        .iter()
        .filter(|f| f.role == FieldRole::Std(StdRole::ExtDimensionType(0)));
    let dk_sem = dk_sem_fields
        .next()
        .ok_or_else(|| fatal("missing semantic ExtDimensionType field"))?;
    if dk_sem_fields.next().is_some() {
        return Err(fatal("duplicate semantic ExtDimensionType field"));
    }
    let [dk_atom] = dk_sem.ty.refs.as_slice() else {
        return Err(fatal("invalid semantic ExtDimensionType target"));
    };
    let kind_ref_table = storage
        .ref_number(*dk_atom)
        .ok_or_else(|| fatal("unbound ExtDimensionTypes chart"))?;
    let section_info = map
        .try_table(section)
        .ok_or_else(|| fatal("missing subconto storage table"))?;
    let section_dim_kind = valid_section_dim_kind(section_info, kind_ref_table, map)?;

    let mut rec_sem_fields = schema
        .table(owner)
        .fields
        .iter()
        .filter(|f| f.role == FieldRole::Std(StdRole::Recorder));
    let rec_sem = rec_sem_fields
        .next()
        .ok_or_else(|| fatal("missing semantic Recorder field"))?;
    if rec_sem_fields.next().is_some() {
        return Err(fatal("duplicate semantic Recorder field"));
    }
    let mut rec_tags = Vec::with_capacity(rec_sem.ty.refs.len());
    for atom in &rec_sem.ty.refs {
        let tag = storage
            .ref_number(*atom)
            .ok_or_else(|| fatal("unbound semantic Recorder reference"))?;
        rec_tags.push(tag);
    }
    rec_tags.sort_unstable();
    rec_tags.dedup();

    let main_info = map
        .try_table(inputs.main)
        .ok_or_else(|| fatal("missing main table"))?;
    let main_period = valid_nonnull_std(main_info, StdField::Period, TypeCode::T)?;
    let main_recorder = valid_recorder_field(main_info, &rec_tags, map)?;
    let main_line = valid_line_no_field(main_info)?;

    let ed_period = valid_nonnull_std(ed_info, StdField::Period, TypeCode::T)?;
    let ed_recorder = valid_recorder_field(ed_info, &rec_tags, map)?;
    let ed_line = valid_line_no_field(ed_info)?;
    let ed_correspond = valid_nonnull_std(ed_info, StdField::Correspond, TypeCode::N)?;
    let corr_ty = ty(ed_info, ed_correspond)?;
    if (corr_ty.length, corr_ty.precision) != (1, 0) {
        return Err(fatal("invalid AccRgED Correspond qualifiers"));
    }
    let ed_kind = valid_nonnull_std(ed_info, StdField::Kind, TypeCode::R)?;
    let kind_ty = ty(ed_info, ed_kind)?;
    if kind_ty.ref_kind != 3
        || map
            .table_by_name(&kind_ty.ref_table)
            .and_then(|id| map.ref_number(id))
            != Some(kind_ref_table)
    {
        return Err(fatal("AccRgED Kind target mismatch"));
    }
    let mut val_matches = ed_info
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.std == Some(StdField::Value));
    let (val_idx, val_field) = val_matches
        .next()
        .ok_or_else(|| fatal("missing Value in AccRgED"))?;
    if val_matches.next().is_some() || val_field.side.is_some() {
        return Err(fatal("ambiguous or invalid Value in AccRgED"));
    }
    super::accounting_balance_payload::validate_shape(semantic_ext, val_field, storage, map)?;
    let ed_value = u16::try_from(val_idx)
        .map(FieldId)
        .map_err(|_| fatal("field index overflow"))?;

    let main_by_period = find_by_period_index(main_info, main_period, main_recorder, main_line);
    let ed_by_period = find_by_period_index(ed_info, ed_period, ed_recorder, ed_line);
    Ok((
        super::BalanceExtEdL {
            table: ed_table,
            main_period,
            main_recorder,
            main_line,
            period: ed_period,
            recorder: ed_recorder,
            line: ed_line,
            correspond: ed_correspond,
            kind: ed_kind,
            value: ed_value,
            max_ext_dims,
            main_by_period,
            ed_by_period,
        },
        section_dim_kind,
        kind_ref_table,
    ))
}

fn balance_ext_section_flag_field(
    schema: &QuerySchema,
    map: &StorageMap,
    table: QTableId,
    section: TableId,
    chart_md: brace::Guid,
    child_md: brace::Guid,
) -> R<FieldId> {
    let chart = balance_chart(schema, table)?;
    if schema.table(chart).object != Some(chart_md) {
        return Err(fatal("ExtDimensionAccountingFlag chart mismatch"));
    }
    let section_info = map
        .try_table(section)
        .ok_or_else(|| fatal("missing subconto storage table"))?;
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

pub(crate) fn balance_ext1_static_inputs(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
    standard: &[StdRole],
    used: &[brace::Guid],
) -> R<()> {
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing owner"))?;
    let meta = schema
        .meta(owner)
        .ok_or_else(|| fatal("missing accounting metadata"))?;
    for id in used {
        let Some(item) = meta.resources.iter().find(|r| r.id == *id) else {
            if meta.dims.iter().any(|d| d.id == *id) {
                continue;
            }
            return Err(fatal("missing selected resource metadata"));
        };
        if !item.balance {
            continue;
        }
        if !matches!(
            item.accounting_flags.accounting,
            crate::schema::AccountingFlagBinding::Absent
        ) {
            return Err(balance_later("Q6a-AccountingBalanceAccountingFlag"));
        }
        match item.accounting_flags.ext_dimension {
            crate::schema::AccountingFlagBinding::Absent => {}
            crate::schema::AccountingFlagBinding::Present { .. }
                if item.accounting_flags.parent_valid => {}
            _ => {
                return Err(balance_later(
                    "Q6a-AccountingBalanceExtDimensionAccountingFlag",
                ));
            }
        }
    }
    let ext_slot_count = (1..=meta.ext_dims)
        .filter(|n| standard.contains(&StdRole::ExtDimension(*n)))
        .count();
    if ext_slot_count > 64 {
        return Err(balance_later("Q6a-AccountingBalanceExtDimensionLayout"));
    }
    let section = balance_ext_section_storage(schema, storage, map, table, inputs)?;
    let mut unique_flags: [Option<FieldId>; 64] = [None; 64];
    let mut unique_flag_count: usize = 0;
    for id in used {
        let Some(item) = meta.resources.iter().find(|r| r.id == *id) else {
            continue;
        };
        if !item.balance {
            continue;
        }
        if let crate::schema::AccountingFlagBinding::Present { chart, child } =
            item.accounting_flags.ext_dimension
        {
            let field =
                balance_ext_section_flag_field(schema, map, table, section.section, chart, child)?;
            let already = unique_flags
                .iter()
                .take(unique_flag_count)
                .any(|f| *f == Some(field));
            if !already {
                if unique_flag_count >= 64 {
                    return Err(balance_later("Q6a-AccountingBalanceExtDimensionLayout"));
                }
                if let Some(slot) = unique_flags.get_mut(unique_flag_count) {
                    *slot = Some(field);
                }
                unique_flag_count += 1;
            }
        }
    }
    let main = map
        .try_table(inputs.main)
        .ok_or_else(|| fatal("missing main"))?;
    if !main_has_inline_ext_values(main) {
        let mut first_ty: Option<&QType> = None;
        for ordinal in 1..=meta.ext_dims {
            if !standard.contains(&StdRole::ExtDimension(ordinal)) {
                continue;
            }
            let mut fields = schema
                .table(table)
                .fields
                .iter()
                .filter(|f| f.role == FieldRole::Std(StdRole::ExtDimension(ordinal)));
            let field = fields
                .next()
                .ok_or_else(|| fatal("missing semantic ExtDimension"))?;
            if fields.next().is_some() {
                return Err(fatal("duplicate semantic ExtDimension"));
            }
            match first_ty {
                None => first_ty = Some(&field.ty),
                Some(prev) => {
                    if prev != &field.ty {
                        return Err(fatal(
                            "accounting payload: incompatible subconto slot checks",
                        ));
                    }
                }
            }
        }
        let semantic_ext = first_ty.ok_or_else(|| fatal("missing required subconto ordinal"))?;
        balance_ext_ed_storage(
            schema,
            storage,
            map,
            table,
            inputs,
            section.section,
            meta.ext_dims,
            semantic_ext,
        )?;
        return Ok(());
    }
    let mut first_slot: Option<(bool, &QType)> = None;
    for ordinal in 1..=meta.ext_dims {
        if !standard.contains(&StdRole::ExtDimension(ordinal)) {
            continue;
        }
        let values = balance_ext_slot_values(main, ordinal)?;
        let mut fields = schema
            .table(table)
            .fields
            .iter()
            .filter(|f| f.role == FieldRole::Std(StdRole::ExtDimension(ordinal)));
        let field = fields
            .next()
            .ok_or_else(|| fatal("missing semantic ExtDimension"))?;
        if fields.next().is_some() {
            return Err(fatal("duplicate semantic ExtDimension"));
        }
        let dr = main
            .field(values[0])
            .ok_or_else(|| fatal("missing ValueDt"))?;
        let cr = main
            .field(values[1])
            .ok_or_else(|| fatal("missing ValueCt"))?;
        super::accounting_balance_payload::validate_shape(&field.ty, dr, storage, map)?;
        super::accounting_balance_payload::validate_shape(&field.ty, cr, storage, map)?;
        if dr.nullable != cr.nullable {
            return Err(fatal("side nullable mismatch"));
        }
        match first_slot {
            None => first_slot = Some((dr.nullable, &field.ty)),
            Some((prev_nullable, prev_ty)) => {
                if prev_nullable != dr.nullable || prev_ty != &field.ty {
                    return Err(fatal(
                        "accounting payload: incompatible subconto slot checks",
                    ));
                }
            }
        }
    }
    if first_slot.is_none() {
        return Err(fatal("missing required subconto ordinal"));
    }
    Ok(())
}
