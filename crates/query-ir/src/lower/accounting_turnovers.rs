use super::accounting_turnovers_payload::{
    TurnoversAccRgEdL, TurnoversExtDimSlotL, TurnoversExtDimsL, TurnoversValueCheckL, value_check,
    value_check_with_later,
};
use super::layout::{R, Row, p16};
use super::rel::Lower;
use super::{AggL, Dec, PExpr, RegisterRowsKindL, RegisterRowsL, RelL};
use crate::ir::{ColSel, Expr, ExprKind, RegisterVtS, SrcId, SrcKind, StmtS, raw_layout as raw};
use crate::schema::{
    AccountingFlagBinding, FieldRole, Item, PeriodField, Periodicity, QFieldId, QTableId,
    QuerySchema, StdRole, StorageBinding, TableKind, VtKind,
};
use crate::{DateQ, ExecError, QType, QValue};
use brace::Guid;
use ibschema::logical::{LType, TypeCode, VAR};
use ibschema::names::{Side, StdField, role};
use ibschema::storage::{FieldId, IndexId, StdList, StorageMap, TableId, TableInfo};
use mdschema::MdKind;
use query_syntax::{Func, Kw, ast::CmpOp};

const COMPONENTS: [usize; 3] = [raw::TURNOVER, raw::RECEIPT, raw::EXPENSE];

#[derive(Clone, Debug, PartialEq)]
pub enum TurnoversAreasL {
    Exact {
        main: Vec<(FieldId, QValue)>,
        options: Vec<(FieldId, QValue)>,
        ext: Vec<(FieldId, QValue)>,
        ed: Vec<(FieldId, QValue)>,
    },
    Unseparated {
        slots: u16,
        main: Vec<(u16, FieldId)>,
        options: Vec<(u16, FieldId)>,
        ext: Vec<(u16, FieldId)>,
        ed: Vec<(u16, FieldId)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingTurnoversDimensionL {
    pub metadata: Guid,
    pub output: QFieldId,
    pub main: [FieldId; 2],
    pub value_check: TurnoversValueCheckL,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingTurnoversRecorderL {
    pub field: FieldId,
    pub reference_tags: Vec<u32>,
}

pub type AccountHierarchySpec = (TableId, FieldId, FieldId);
pub type AccountFilterSpec = (Option<AccountHierarchySpec>, Vec<[u8; 16]>);

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingTurnoversL {
    pub policy_fields: Vec<FieldId>,
    pub secured: bool,
    pub inputs: AccountingTurnoversInputsL,
    pub areas: Option<TurnoversAreasL>,
    pub account: bool,
    pub account_filter: Option<AccountFilterSpec>,
    pub dims: Vec<AccountingTurnoversDimensionL>,
    pub ext_dims: Option<TurnoversExtDimsL>,
    pub resources: Vec<AccountingTurnoversResourceL>,
    pub dates: [Option<bsl_prim::BslDate>; 2],
    pub period: Option<FieldId>,
    pub recorder: Option<AccountingTurnoversRecorderL>,
    pub line: Option<FieldId>,
}

impl AccountingTurnoversL {
    pub fn is_account_filter_empty(&self) -> bool {
        matches!(&self.account_filter, Some((_, ids)) if ids.is_empty())
    }

    pub fn account_filter_ids(&self) -> Option<&[[u8; 16]]> {
        match &self.account_filter {
            Some((None, ids)) => Some(ids.as_slice()),
            _ => None,
        }
    }

    pub fn account_filter_hierarchy(&self) -> Option<(TableId, FieldId, FieldId, &[[u8; 16]])> {
        match &self.account_filter {
            Some((Some((chart, id, parent)), seeds)) => {
                Some((*chart, *id, *parent, seeds.as_slice()))
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingTurnoversInputsL {
    pub main: TableId,
    pub options: TableId,
    pub active: FieldId,
    pub accounts: [FieldId; 2],
    pub account_output: QFieldId,
    pub ref_table: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingTurnoversResourceL {
    pub metadata: Guid,
    pub main: [FieldId; 2],
    pub null_zero: [bool; 2],
    pub ext_flag: Option<u16>,
    pub dec: Dec,
    pub outputs: [QFieldId; 3],
}

fn fatal(message: impl std::fmt::Display) -> ExecError {
    ExecError::Fatal(format!("accounting Turnovers: {message}"))
}

fn later(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q6b" }
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
        return Some("Q6b-TurnoversNonBalancedFlagBinding");
    }
    if matches!(facts.ext_dimension, AccountingFlagBinding::Present { .. }) {
        return Some("Q6b-TurnoversNonBalancedExtDimensionFlag");
    }
    None
}

pub(crate) fn is_accounting_turnovers(schema: &QuerySchema, table: QTableId) -> bool {
    let table = schema.table(table);
    table.kind == TableKind::Virtual(VtKind::Turnovers)
        && table.owner.is_some_and(|owner| {
            schema.table(owner).kind == TableKind::Object(MdKind::AccountingRegister)
        })
}

pub(crate) fn turnovers_ext_section_identity(
    schema: &QuerySchema,
    table: QTableId,
) -> Option<QTableId> {
    if !is_accounting_turnovers(schema, table) {
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

pub(crate) fn turnovers_uses_ext_dims(stmt: &StmtS, schema: &QuerySchema, table: QTableId) -> bool {
    if !is_accounting_turnovers(schema, table) {
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

pub(crate) struct TurnoversInputs<'a> {
    schema: &'a QuerySchema,
    storage: &'a StorageBinding,
    map: &'a StorageMap,
    table: QTableId,
}

pub(crate) fn turnovers_inputs<'a>(
    schema: &'a QuerySchema,
    storage: &'a StorageBinding,
    map: &'a StorageMap,
    table: QTableId,
) -> TurnoversInputs<'a> {
    TurnoversInputs {
        schema,
        storage,
        map,
        table,
    }
}

fn one_type(info: &TableInfo, field: FieldId) -> R<&LType> {
    match info.field(field).map(|field| field.types.as_slice()) {
        Some([ty]) => Ok(ty),
        _ => Err(fatal("missing or ambiguous stored type")),
    }
}

fn standard(info: &TableInfo, role: StdField, code: TypeCode) -> R<FieldId> {
    let mut fields = info
        .fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.std == Some(role));
    let (index, _) = fields
        .next()
        .ok_or_else(|| fatal(format_args!("missing {role:?}")))?;
    if fields.next().is_some() {
        return Err(fatal(format_args!("ambiguous {role:?}")));
    }
    let field = FieldId(u16::try_from(index).map_err(|_| fatal("stored field index overflow"))?);
    if one_type(info, field)?.code != code {
        return Err(fatal(format_args!("wrong type of {role:?}")));
    }
    Ok(field)
}

fn areas_refused() -> ExecError {
    ExecError::Later {
        what: "Q6b-TurnoversAreas",
        stage: "Q7c",
    }
}

fn separator_number_fits(number: &bsl_prim::Number, length: u32, precision: u32) -> bool {
    if length == 0 || length > 38 || precision > length {
        return false;
    }
    struct Digits {
        integer_limit: u8,
        fraction_limit: u8,
        integer: u8,
        fraction: u8,
        after_dot: bool,
        leading_zero: bool,
    }
    impl std::fmt::Write for Digits {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            for byte in text.bytes() {
                match byte {
                    b'-' => {}
                    b'.' => self.after_dot = true,
                    b'0'..=b'9' if self.after_dot => {
                        if self.fraction == self.fraction_limit {
                            return Err(std::fmt::Error);
                        }
                        self.fraction += 1;
                    }
                    b'0' if self.integer == 0 && !self.leading_zero => {
                        self.leading_zero = true;
                    }
                    b'0'..=b'9' => {
                        if self.integer == self.integer_limit {
                            return Err(std::fmt::Error);
                        }
                        self.integer += 1;
                    }
                    _ => return Err(std::fmt::Error),
                }
            }
            Ok(())
        }
    }
    let mut digits = Digits {
        integer_limit: (length - precision) as u8,
        fraction_limit: precision as u8,
        integer: 0,
        fraction: 0,
        after_dot: false,
        leading_zero: false,
    };
    number.write_xml(&mut digits).is_ok()
}

fn separator_type_supported(ty: &LType, map: &StorageMap) -> bool {
    match ty.code {
        TypeCode::N => ty.length > 0 && ty.length <= 38 && ty.precision <= ty.length,
        TypeCode::S => ty.precision == 0,
        TypeCode::L | TypeCode::T => ty.length == 0 && ty.precision == 0,
        TypeCode::R => ty.ref_kind == 3 && map.table_by_name(&ty.ref_table).is_some(),
        TypeCode::B => ty.length == 16 && ty.precision == 0,
        TypeCode::E | TypeCode::V => false,
    }
}

fn separator_value_matches(value: &QValue, ty: &LType, map: &StorageMap) -> bool {
    match (ty.code, value) {
        (TypeCode::N, QValue::Number(n)) => separator_number_fits(n, ty.length, ty.precision),
        (TypeCode::S, QValue::String(s)) => {
            ty.length == VAR || s.len_utf16() <= (ty.length & !VAR) as usize
        }
        (TypeCode::L, QValue::Bool(_)) => true,
        (TypeCode::T, QValue::Date(d)) => d.frac_ticks() == 0,
        (TypeCode::R, QValue::Ref { table, .. }) => {
            ty.ref_kind == 3 && map.ref_table(*table) == map.table_by_name(&ty.ref_table)
        }
        (TypeCode::B, QValue::Uuid(_)) => ty.length == 16,
        (TypeCode::B, QValue::Binary(b)) => ty.length == 16 && b.len() == 16,
        _ => false,
    }
}

fn has_inline_ext_dims(info: &TableInfo) -> bool {
    info.fields.iter().any(|field| {
        matches!(field.std, Some(StdField::ValueDt | StdField::ValueCt))
            || field.name.starts_with("ValueDt")
            || field.name.starts_with("ValueCt")
    })
}

pub(super) fn find_index_with_seps(
    info: &TableInfo,
    logical_prefix: &[FieldId],
) -> Option<IndexId> {
    let sep_len = info.separators.len();
    for (idx, ix) in info.indexes.iter().enumerate() {
        if ix.absent {
            continue;
        }
        let seps_match = info
            .separators
            .iter()
            .zip(&ix.fields)
            .all(|(sep, field)| sep.field == *field);
        if seps_match
            && ix.fields.get(sep_len..sep_len + logical_prefix.len()) == Some(logical_prefix)
        {
            return u16::try_from(idx).ok().map(IndexId::Named);
        }
    }
    None
}

pub(crate) struct TurnoversAreaMapping {
    attrs: Vec<Guid>,
    main: Vec<(u16, FieldId, LType)>,
    options: Vec<(u16, FieldId, LType)>,
    ext: Vec<(u16, FieldId, LType)>,
    ed: Vec<(u16, FieldId, LType)>,
}

impl TurnoversInputs<'_> {
    fn acc_rg_ed_id(&self, main: TableId, options: TableId) -> R<Option<TableId>> {
        let (_, id) = self.owner()?;
        let object = self
            .map
            .object(id)
            .ok_or_else(|| fatal("missing storage owner"))?;
        let main_info = self
            .map
            .try_table(main)
            .ok_or_else(|| fatal("missing main descriptor"))?;
        if has_inline_ext_dims(main_info) {
            return Ok(None);
        }
        let mut eds = object
            .service
            .iter()
            .filter(|(name, _)| name == role::EXT_DIMENSIONS_VALUES);
        let (_, ed_id) = eds.next().ok_or_else(|| fatal("missing AccRgED role"))?;
        if eds.next().is_some() || *ed_id == main || *ed_id == options {
            return Err(fatal("ambiguous or aliased AccRgED role"));
        }
        let ed_info = self
            .map
            .try_table(*ed_id)
            .ok_or_else(|| fatal("missing AccRgED descriptor"))?;
        if ed_info.md != Some(id) || ed_info.purpose != role::EXT_DIMENSIONS_VALUES {
            return Err(fatal("AccRgED ownership or purpose mismatch"));
        }
        Ok(Some(*ed_id))
    }

    pub(crate) fn acc_rg_ed_table(
        &self,
        inputs: &AccountingTurnoversInputsL,
    ) -> R<Option<TableId>> {
        self.acc_rg_ed_id(inputs.main, inputs.options)
    }

    fn area_tables(
        &self,
        with_ext_dims: bool,
    ) -> R<(TableId, TableId, Option<TableId>, Option<TableId>)> {
        let (owner, id) = self.owner()?;
        let object = self
            .map
            .object(id)
            .ok_or_else(|| fatal("missing storage owner"))?;
        let main = object.main;
        if self.storage.table(owner) != Some(main)
            || object.service.iter().any(|(name, _)| name == role::MAIN)
        {
            return Err(fatal("ambiguous or contradictory main binding"));
        }
        let mut options = object
            .service
            .iter()
            .filter(|(name, _)| name == role::ACCOUNT_REGISTER_TOTALS_OPTIONS);
        let (_, options_id) = options
            .next()
            .ok_or_else(|| fatal("missing options role"))?;
        if options.next().is_some() || *options_id == main {
            return Err(fatal("ambiguous or aliased options role"));
        }
        let ext = with_ext_dims
            .then(|| turnovers_ext_section_identity(self.schema, self.table))
            .flatten()
            .and_then(|logical| self.storage.table(logical));
        let ed = if with_ext_dims {
            let ed = self.acc_rg_ed_id(main, *options_id)?;
            if ed.is_some() && ed == ext {
                return Err(fatal("ambiguous or aliased AccRgED role"));
            }
            ed
        } else {
            None
        };
        Ok((main, *options_id, ext, ed))
    }

    fn validate_separator<'a>(
        &self,
        info: &'a TableInfo,
        sep: &ibschema::storage::SeparatorInfo,
    ) -> R<&'a LType> {
        if sep.common_attribute == Guid::NIL
            || sep.use_field.is_some()
            || !self
                .schema
                .data_separators()
                .iter()
                .any(|ds| ds.id == sep.common_attribute)
        {
            return Err(areas_refused());
        }
        let field = info.field(sep.field).ok_or_else(areas_refused)?;
        let [ty] = field.types.as_slice() else {
            return Err(areas_refused());
        };
        if field.std.is_some() || !separator_type_supported(ty, self.map) {
            return Err(areas_refused());
        }
        Ok(ty)
    }

    pub(crate) fn area_mapping(&self, with_ext_dims: bool) -> R<Option<TurnoversAreaMapping>> {
        let (main_id, options_id, ext_id, ed_id) = self.area_tables(with_ext_dims)?;
        let main_info = self
            .map
            .try_table(main_id)
            .ok_or_else(|| fatal("missing main descriptor"))?;
        let options_info = self
            .map
            .try_table(options_id)
            .ok_or_else(|| fatal("missing options descriptor"))?;
        let ext_info = match ext_id {
            Some(id) => Some(
                self.map
                    .try_table(id)
                    .ok_or_else(|| fatal("missing ExtDimensionTypes storage table"))?,
            ),
            None => None,
        };
        let ed_info = match ed_id {
            Some(id) => Some(
                self.map
                    .try_table(id)
                    .ok_or_else(|| fatal("missing AccRgED descriptor"))?,
            ),
            None => None,
        };
        if main_info.separators.is_empty()
            && options_info.separators.is_empty()
            && ext_info.is_none_or(|info| info.separators.is_empty())
            && ed_info.is_none_or(|info| info.separators.is_empty())
        {
            return Ok(None);
        }
        let mut attrs: Vec<Guid> = Vec::new();
        let mut attr_types: Vec<LType> = Vec::new();
        let mut collect = |info: &TableInfo| -> R<Vec<(u16, FieldId, LType)>> {
            let mut out = Vec::with_capacity(info.separators.len());
            for sep in &info.separators {
                let ty = self.validate_separator(info, sep)?;
                let slot = if let Some(pos) =
                    attrs.iter().position(|attr| *attr == sep.common_attribute)
                {
                    if attr_types.get(pos) != Some(ty) {
                        return Err(areas_refused());
                    }
                    u16::try_from(pos).map_err(|_| areas_refused())?
                } else {
                    let pos = u16::try_from(attrs.len()).map_err(|_| areas_refused())?;
                    attrs.push(sep.common_attribute);
                    attr_types.push(ty.clone());
                    pos
                };
                if out.iter().any(|(existing, _, _)| *existing == slot) {
                    return Err(areas_refused());
                }
                out.push((slot, sep.field, ty.clone()));
            }
            Ok(out)
        };
        let main = collect(main_info)?;
        let options = collect(options_info)?;
        let ext = match ext_info {
            Some(info) => collect(info)?,
            None => Vec::new(),
        };
        let ed = match ed_info {
            Some(info) => collect(info)?,
            None => Vec::new(),
        };
        Ok(Some(TurnoversAreaMapping {
            attrs,
            main,
            options,
            ext,
            ed,
        }))
    }

    pub(crate) fn bind_areas(
        &self,
        with_ext_dims: bool,
        filters: &[crate::host::SeparatorFilter],
    ) -> R<Option<TurnoversAreasL>> {
        let Some(mapping) = self.area_mapping(with_ext_dims)? else {
            return Ok(None);
        };
        if filters.is_empty() {
            let slots = u16::try_from(mapping.attrs.len()).map_err(|_| areas_refused())?;
            return Ok(Some(TurnoversAreasL::Unseparated {
                slots,
                main: mapping
                    .main
                    .into_iter()
                    .map(|(slot, field, _)| (slot, field))
                    .collect(),
                options: mapping
                    .options
                    .into_iter()
                    .map(|(slot, field, _)| (slot, field))
                    .collect(),
                ext: mapping
                    .ext
                    .into_iter()
                    .map(|(slot, field, _)| (slot, field))
                    .collect(),
                ed: mapping
                    .ed
                    .into_iter()
                    .map(|(slot, field, _)| (slot, field))
                    .collect(),
            }));
        }
        let mut values: Vec<QValue> = Vec::with_capacity(mapping.attrs.len());
        for attr in &mapping.attrs {
            let mut matches = filters.iter().filter(|f| f.common_attribute == *attr);
            let filter = matches.next().ok_or_else(areas_refused)?;
            if matches.next().is_some() {
                return Err(areas_refused());
            }
            values.push(filter.value.clone());
        }
        let bind_table = |entries: Vec<(u16, FieldId, LType)>| -> R<Vec<(FieldId, QValue)>> {
            let mut out = Vec::with_capacity(entries.len());
            for (slot, field, ty) in entries {
                let value = values.get(usize::from(slot)).ok_or_else(areas_refused)?;
                if !separator_value_matches(value, &ty, self.map) {
                    return Err(areas_refused());
                }
                out.push((field, value.clone()));
            }
            Ok(out)
        };
        Ok(Some(TurnoversAreasL::Exact {
            main: bind_table(mapping.main)?,
            options: bind_table(mapping.options)?,
            ext: bind_table(mapping.ext)?,
            ed: bind_table(mapping.ed)?,
        }))
    }
}

impl TurnoversInputs<'_> {
    fn owner(&self) -> R<(QTableId, Guid)> {
        if !is_accounting_turnovers(self.schema, self.table) {
            return Err(fatal("not an accounting Turnovers source"));
        }
        let owner = self
            .schema
            .table(self.table)
            .owner
            .ok_or_else(|| fatal("missing owner"))?;
        let id = self
            .schema
            .table(owner)
            .object
            .ok_or_else(|| fatal("missing register UUID"))?;
        Ok((owner, id))
    }

    pub(crate) fn has_physical_separators(&self, with_ext_dims: bool) -> bool {
        let Ok((owner, id)) = self.owner() else {
            return false;
        };
        let Some(object) = self.map.object(id) else {
            return false;
        };
        let section = with_ext_dims
            .then(|| turnovers_ext_section_identity(self.schema, self.table))
            .flatten()
            .and_then(|logical| self.storage.table(logical));
        let uses_ed = with_ext_dims
            && self
                .map
                .try_table(object.main)
                .is_some_and(|main| !has_inline_ext_dims(main));
        std::iter::once(object.main)
            .chain(self.storage.table(owner))
            .chain(section)
            .chain(
                object
                    .service
                    .iter()
                    .filter(|(role_name, _)| {
                        role_name == role::MAIN
                            || role_name == role::ACCOUNT_REGISTER_TOTALS_OPTIONS
                            || (uses_ed && role_name == role::EXT_DIMENSIONS_VALUES)
                    })
                    .map(|(_, table)| *table),
            )
            .any(|table| {
                self.map
                    .try_table(table)
                    .is_some_and(|info| !info.separators.is_empty())
            })
    }

    pub(crate) fn resolve(&self) -> R<AccountingTurnoversInputsL> {
        let (owner, id) = self.owner()?;
        if !self.schema.meta(owner).is_some_and(|meta| meta.corr) {
            return Err(later("Q6b-TurnoversCorrespondence"));
        }
        let object = self
            .map
            .object(id)
            .ok_or_else(|| fatal("missing storage owner"))?;
        let main = object.main;
        if self.storage.table(owner) != Some(main)
            || object.service.iter().any(|(name, _)| name == role::MAIN)
        {
            return Err(fatal("ambiguous or contradictory main binding"));
        }
        let mut options = object
            .service
            .iter()
            .filter(|(name, _)| name == role::ACCOUNT_REGISTER_TOTALS_OPTIONS);
        let (_, options_id) = options
            .next()
            .ok_or_else(|| fatal("missing options role"))?;
        if options.next().is_some() || *options_id == main {
            return Err(fatal("ambiguous or aliased options role"));
        }
        let options = *options_id;
        let m = self
            .map
            .try_table(main)
            .ok_or_else(|| fatal("missing main descriptor"))?;
        let o = self
            .map
            .try_table(options)
            .ok_or_else(|| fatal("missing options descriptor"))?;
        if m.md != Some(id)
            || m.purpose != role::MAIN
            || o.purpose != role::ACCOUNT_REGISTER_TOTALS_OPTIONS
        {
            return Err(fatal("main/options ownership or purpose mismatch"));
        }
        let active = standard(m, StdField::Active, TypeCode::L)?;
        for (role, code) in [
            (StdField::RegId, TypeCode::B),
            (StdField::Period, TypeCode::T),
            (StdField::ActualPeriod, TypeCode::L),
            (StdField::UseTotals, TypeCode::N),
            (StdField::MinPeriod, TypeCode::T),
            (StdField::UseSplitter, TypeCode::L),
        ] {
            let field = standard(o, role, code)?;
            let ty = one_type(o, field)?;
            if (role == StdField::RegId && ty.length != 16)
                || (role == StdField::UseTotals && (ty.length, ty.precision) != (1, 0))
            {
                return Err(fatal("invalid options field shape"));
            }
        }
        if o.fields
            .iter()
            .any(|field| field.std == Some(StdField::MinCalculatedPeriod))
        {
            standard(o, StdField::MinCalculatedPeriod, TypeCode::T)?;
        }
        let mut accounts = self
            .schema
            .table(self.table)
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| field.role == FieldRole::Std(StdRole::Account));
        let (output, account) = accounts
            .next()
            .ok_or_else(|| fatal("missing Account catalog field"))?;
        if accounts.next().is_some() || account.ty.atom_count() != 1 || account.ty.refs.len() != 1 {
            return Err(fatal("ambiguous account reference target"));
        }
        let atom = *account
            .ty
            .refs
            .first()
            .ok_or_else(|| fatal("missing account reference atom"))?;
        let ref_table = self
            .storage
            .ref_number(atom)
            .ok_or_else(|| fatal("unbound chart reference"))?;
        let chart = self
            .map
            .ref_table(ref_table)
            .ok_or_else(|| fatal("unknown chart reference"))?;
        let accounts = [
            standard(m, StdField::AccountDt, TypeCode::R)?,
            standard(m, StdField::AccountCt, TypeCode::R)?,
        ];
        if accounts[0] == accounts[1] {
            return Err(fatal("aliased account fields"));
        }
        for field in accounts {
            let ty = one_type(m, field)?;
            if ty.ref_kind != 3 || self.map.table_by_name(&ty.ref_table) != Some(chart) {
                return Err(fatal("stored account has a different chart target"));
            }
        }
        Ok(AccountingTurnoversInputsL {
            main,
            options,
            active,
            accounts,
            ref_table,
            account_output: QFieldId(
                u16::try_from(output).map_err(|_| fatal("account output overflow"))?,
            ),
        })
    }

    fn component(&self, output: QFieldId) -> Option<usize> {
        let field = self.schema.field(self.table, output)?;
        let owner = self.schema.table(self.table).owner?;
        let item = self
            .schema
            .meta(owner)?
            .resources
            .iter()
            .find(|item| Some(item.id) == field.md)?;
        if field.role != FieldRole::Virtual || field.side.is_some() {
            return None;
        }
        match field.name_ru.strip_prefix(&item.name)? {
            "Оборот" => Some(0),
            "ОборотДт" => Some(1),
            "ОборотКт" => Some(2),
            _ => None,
        }
    }

    pub(crate) fn form_refusal(&self, used: &[QFieldId], all: bool) -> Option<&'static str> {
        if all {
            return Some("Q6b-TurnoversStar");
        }
        let owner = self.schema.table(self.table).owner?;
        let meta = self.schema.meta(owner)?;
        let mut rank = usize::MAX;
        let mut resource_refusal = None;
        let mut resources = false;
        let mut ext_chart = false;
        for id in used {
            let Some(field) = self.schema.field(self.table, *id) else {
                continue;
            };
            if field.role == FieldRole::Std(StdRole::Account) {
                continue;
            }
            if let FieldRole::Std(StdRole::ExtDimension(n)) = field.role
                && n >= 1
            {
                if field
                    .ty
                    .refs
                    .iter()
                    .any(|atom| self.schema.ref_info(*atom).kind == MdKind::ChartOfAccounts)
                {
                    ext_chart = true;
                }
                continue;
            }
            if field.role == FieldRole::Dimension
                && meta.dims.iter().any(|item| Some(item.id) == field.md)
            {
                continue;
            }
            if field.period().is_some()
                || matches!(
                    field.role,
                    FieldRole::Std(StdRole::Period | StdRole::Recorder | StdRole::LineNumber)
                )
            {
                continue;
            }
            if let Some(item) = meta.resources.iter().find(|item| Some(item.id) == field.md) {
                if self.component(*id).is_some() {
                    if let Some(what) = nonbalanced_refusal(item) {
                        if rank > 2 {
                            rank = 2;
                            resource_refusal = Some(what);
                        }
                    } else {
                        resources = true;
                    }
                } else {
                    rank = rank.min(3);
                }
            } else if field.role == FieldRole::Virtual {
                rank = rank.min(3);
            } else if field.md.is_some()
                || field.nested.is_some()
                || matches!(
                    field.role,
                    FieldRole::Std(StdRole::ExtDimension(_) | StdRole::ExtDimensionType(_))
                )
            {
                rank = rank.min(1);
            } else {
                rank = rank.min(3)
            }
        }
        match rank {
            0 => Some("Q6b-TurnoversPeriodFields"),
            1 => Some("Q6b-TurnoversDimensions"),
            2 => resource_refusal,
            3 => Some("Q6b-TurnoversColumns"),
            _ if !resources => Some("Q6b-TurnoversNoResource"),
            _ if ext_chart => Some("Q6b-TurnoversExtDimensionChart"),
            _ => None,
        }
    }

    pub(crate) fn resolve_ext_dims(
        &self,
        inputs: &AccountingTurnoversInputsL,
        used: &[QFieldId],
    ) -> R<Option<TurnoversExtDimsL>> {
        let mut selected: Vec<(u8, QFieldId)> = Vec::new();
        for output in used {
            let Some(field) = self.schema.field(self.table, *output) else {
                continue;
            };
            if let FieldRole::Std(StdRole::ExtDimension(ordinal)) = field.role {
                if ordinal == 0 {
                    return Err(fatal("invalid zero ExtDimension ordinal"));
                }
                if selected.iter().any(|(ord, _)| *ord == ordinal) {
                    return Err(fatal("duplicate semantic ExtDimension ordinal"));
                }
                selected.push((ordinal, *output));
            }
        }
        if selected.is_empty() {
            return Ok(None);
        }
        selected.sort_by_key(|(ord, _)| *ord);
        let logical = turnovers_ext_section_identity(self.schema, self.table)
            .ok_or_else(|| fatal("missing required ExtDimensionTypes section identity"))?;
        let chart = self
            .schema
            .table(logical)
            .owner
            .ok_or_else(|| fatal("missing ExtDimensionTypes logical owner"))?;
        let chart_md = self
            .schema
            .table(chart)
            .object
            .ok_or_else(|| fatal("missing chart metadata identity"))?;
        let chart_storage = self
            .map
            .ref_table(inputs.ref_table)
            .ok_or_else(|| fatal("unknown chart reference"))?;
        let object = self
            .map
            .object(chart_md)
            .ok_or_else(|| fatal("missing chart storage object"))?;
        let mut lists = object
            .lists
            .iter()
            .filter(|(kind, _)| *kind == StdList::ExtDimensionTypes);
        let section = lists
            .next()
            .map(|(_, id)| *id)
            .ok_or_else(|| fatal("missing required ExtDimensionTypes storage list"))?;
        if lists.next().is_some()
            || object.main != chart_storage
            || self.storage.table(chart) != Some(chart_storage)
            || self.storage.table(logical) != Some(section)
            || [inputs.main, inputs.options, chart_storage].contains(&section)
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
            return Err(fatal(
                "ambiguous or aliased ExtDimensionTypes storage binding",
            ));
        }
        let section_info = self
            .map
            .try_table(section)
            .ok_or_else(|| fatal("missing ExtDimensionTypes storage table"))?;
        if section_info.owner != Some(chart_storage)
            || section_info.md != Some(chart_md)
            || section_info.purpose != role::TABULAR_SECTION
            || section_info.query_name != self.schema.table(logical).name_en
        {
            return Err(fatal("contradictory ExtDimensionTypes storage ownership"));
        }
        for sep in &section_info.separators {
            self.validate_separator(section_info, sep)?;
        }
        let owner = standard(section_info, StdField::Id, TypeCode::R)?;
        let owner_ty = one_type(section_info, owner)?;
        if owner_ty.ref_kind != 2
            || self.map.table_by_name(&owner_ty.ref_table) != Some(chart_storage)
        {
            return Err(fatal("ExtDimensionTypes owner reference mismatch"));
        }
        let ordinal = standard(section_info, StdField::LineNo, TypeCode::N)?;
        let ordinal_ty = one_type(section_info, ordinal)?;
        if (ordinal_ty.length, ordinal_ty.precision) != (5, 0) {
            return Err(fatal("ExtDimensionTypes ordinal qualifiers mismatch"));
        }
        for field in [owner, ordinal] {
            if section_info
                .field(field)
                .is_none_or(|f| f.nullable || f.side.is_some())
            {
                return Err(fatal("invalid required ExtDimensionTypes field flags"));
            }
        }
        let main_info = self
            .map
            .try_table(inputs.main)
            .ok_or_else(|| fatal("missing main descriptor"))?;
        if has_inline_ext_dims(main_info) {
            fn exact_value(info: &TableInfo, std: StdField, name: &str) -> R<FieldId> {
                let mut fields = info
                    .fields
                    .iter()
                    .enumerate()
                    .filter(|(_, field)| field.name == name);
                let (index, field) = fields.next().ok_or_else(|| {
                    fatal(format_args!("missing required ExtDimension value {name}"))
                })?;
                if fields.next().is_some() || field.std != Some(std) || field.side.is_some() {
                    return Err(fatal(format_args!(
                        "contradictory ExtDimension value identity {name}"
                    )));
                }
                u16::try_from(index)
                    .map(FieldId)
                    .map_err(|_| fatal("ExtDimension field index overflow"))
            }
            let mut slots = Vec::with_capacity(selected.len());
            for (ord, output) in selected {
                let field = self
                    .schema
                    .field(self.table, output)
                    .ok_or_else(|| fatal("missing semantic ExtDimension field"))?;
                let dt = exact_value(main_info, StdField::ValueDt, &format!("ValueDt{ord}"))?;
                let ct = exact_value(main_info, StdField::ValueCt, &format!("ValueCt{ord}"))?;
                if dt == ct {
                    return Err(fatal("aliased ExtDimension value sides"));
                }
                let dr_info = main_info
                    .field(dt)
                    .ok_or_else(|| fatal("missing ExtDimension ValueDt descriptor"))?;
                let cr_info = main_info
                    .field(ct)
                    .ok_or_else(|| fatal("missing ExtDimension ValueCt descriptor"))?;
                let dr_check = value_check(&field.ty, dr_info, self.storage, self.map)?;
                let cr_check = value_check(&field.ty, cr_info, self.storage, self.map)?;
                if dr_check != cr_check {
                    return Err(fatal("incompatible ExtDimension side checks"));
                }
                slots.push(TurnoversExtDimSlotL {
                    ordinal: ord,
                    output,
                    values: Some([dt, ct]),
                    value_check: dr_check,
                });
            }
            return Ok(Some(TurnoversExtDimsL {
                section,
                owner,
                ordinal,
                acc_rg_ed: None,
                ext_flag_fields: Vec::new(),
                slots,
            }));
        }

        let ed_id = self
            .acc_rg_ed_table(inputs)?
            .ok_or_else(|| fatal("missing AccRgED role"))?;
        if [inputs.main, inputs.options, chart_storage, section].contains(&ed_id) {
            return Err(fatal("aliased AccRgED role"));
        }
        let ed_info = self
            .map
            .try_table(ed_id)
            .ok_or_else(|| fatal("missing AccRgED descriptor"))?;
        for sep in &ed_info.separators {
            self.validate_separator(ed_info, sep)?;
        }
        let main_period = standard(main_info, StdField::Period, TypeCode::T)?;
        if main_info
            .field(main_period)
            .is_none_or(|f| f.nullable || f.side.is_some())
        {
            return Err(fatal("invalid main Period physical field"));
        }
        let recorder_tags = self.recorder_reference_tags()?;
        let main_recorder = self.resolve_recorder_field(main_info, &recorder_tags)?;
        let main_line = Self::resolve_line_field(main_info)?;

        let ed_period = standard(ed_info, StdField::Period, TypeCode::T)?;
        if ed_info
            .field(ed_period)
            .is_none_or(|f| f.nullable || f.side.is_some())
        {
            return Err(fatal("invalid AccRgED Period physical field"));
        }
        let ed_recorder = self.resolve_recorder_field(ed_info, &recorder_tags)?;
        let ed_line = Self::resolve_line_field(ed_info)?;
        let ed_correspond = standard(ed_info, StdField::Correspond, TypeCode::N)?;
        let ed_corr_ty = one_type(ed_info, ed_correspond)?;
        if (ed_corr_ty.length, ed_corr_ty.precision) != (1, 0)
            || ed_info
                .field(ed_correspond)
                .is_none_or(|f| f.nullable || f.side.is_some())
        {
            return Err(fatal("invalid AccRgED Correspond physical field"));
        }
        let ed_kind = standard(ed_info, StdField::Kind, TypeCode::R)?;
        let ed_kind_ty = one_type(ed_info, ed_kind)?;
        if ed_kind_ty.ref_kind != 3
            || ed_info
                .field(ed_kind)
                .is_none_or(|f| f.nullable || f.side.is_some())
        {
            return Err(fatal("invalid AccRgED Kind physical field"));
        }
        let kind_storage = self
            .map
            .table_by_name(&ed_kind_ty.ref_table)
            .ok_or_else(|| fatal("unknown AccRgED Kind reference table"))?;
        let kind_ref_table = self
            .map
            .ref_number(kind_storage)
            .ok_or_else(|| fatal("unbound AccRgED Kind reference number"))?;

        let mut ed_val_matches = ed_info
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| field.std == Some(StdField::Value));
        let (ed_val_idx, ed_val_info) = ed_val_matches
            .next()
            .ok_or_else(|| fatal("missing AccRgED Value physical field"))?;
        if ed_val_matches.next().is_some() || ed_val_info.side.is_some() {
            return Err(fatal("ambiguous or sided AccRgED Value physical field"));
        }
        let ed_value = FieldId(
            u16::try_from(ed_val_idx).map_err(|_| fatal("AccRgED Value field index overflow"))?,
        );
        let ed_all_fields = [
            ed_period,
            ed_recorder,
            ed_line,
            ed_correspond,
            ed_kind,
            ed_value,
        ];
        for (i, a) in ed_all_fields.iter().enumerate() {
            if ed_all_fields.iter().skip(i + 1).any(|b| b == a) {
                return Err(fatal("aliased AccRgED fields"));
            }
        }

        let section_dim_kind = standard(section_info, StdField::DimKind, TypeCode::R)?;
        if section_info
            .field(section_dim_kind)
            .is_none_or(|f| f.nullable || f.side.is_some())
        {
            return Err(fatal("invalid ExtDimensionTypes DimKind field"));
        }
        let dk_ty = one_type(section_info, section_dim_kind)?;
        if dk_ty.ref_kind != 3 || self.map.table_by_name(&dk_ty.ref_table) != Some(kind_storage) {
            return Err(fatal("ExtDimensionTypes DimKind target mismatch"));
        }
        if let Some(sem_dim_kind) = self
            .schema
            .table(logical)
            .fields
            .iter()
            .find(|f| f.role == FieldRole::Std(StdRole::ExtDimensionType(0)))
            && let [atom] = sem_dim_kind.ty.refs.as_slice()
            && self.storage.ref_number(*atom) != Some(kind_ref_table)
        {
            return Err(fatal("ExtDimensionTypes semantic DimKind target mismatch"));
        }

        let mut slots = Vec::with_capacity(selected.len());
        for (ord, output) in selected {
            let field = self
                .schema
                .field(self.table, output)
                .ok_or_else(|| fatal("missing semantic ExtDimension field"))?;
            let check = value_check(&field.ty, ed_val_info, self.storage, self.map)?;
            slots.push(TurnoversExtDimSlotL {
                ordinal: ord,
                output,
                values: None,
                value_check: check,
            });
        }

        let main_by_period =
            find_index_with_seps(main_info, &[main_period, main_recorder, main_line]);
        let ed_by_period = find_index_with_seps(
            ed_info,
            &[ed_period, ed_recorder, ed_line, ed_kind, ed_correspond],
        );
        let (ed_by_recorder, ed_by_recorder_has_correspond) = if let Some(ix) =
            find_index_with_seps(ed_info, &[ed_recorder, ed_line, ed_correspond])
        {
            (Some(ix), true)
        } else if let Some(ix) = find_index_with_seps(ed_info, &[ed_recorder, ed_line]) {
            (Some(ix), false)
        } else {
            (None, false)
        };

        Ok(Some(TurnoversExtDimsL {
            section,
            owner,
            ordinal,
            acc_rg_ed: Some(Box::new(TurnoversAccRgEdL {
                table: ed_id,
                main_period,
                main_recorder,
                main_line,
                recorder_tags,
                section_dim_kind,
                kind_ref_table,
                ed_period,
                ed_recorder,
                ed_line,
                ed_correspond,
                ed_kind,
                ed_value,
                main_by_period,
                ed_by_period,
                ed_by_recorder,
                ed_by_recorder_has_correspond,
            })),
            ext_flag_fields: Vec::new(),
            slots,
        }))
    }

    pub(crate) fn dimension(
        &self,
        inputs: &AccountingTurnoversInputsL,
        output: QFieldId,
    ) -> R<AccountingTurnoversDimensionL> {
        let (owner, _) = self.owner()?;
        let field = self
            .schema
            .field(self.table, output)
            .ok_or_else(|| fatal("missing semantic dimension field"))?;
        if field.role != FieldRole::Dimension {
            return Err(fatal("dimension field role mismatch"));
        }
        let metadata = field
            .md
            .ok_or_else(|| fatal("missing dimension metadata UUID"))?;
        let item = self
            .schema
            .meta(owner)
            .and_then(|meta| meta.dims.iter().find(|item| item.id == metadata))
            .ok_or_else(|| fatal("unknown dimension UUID"))?;
        let info = self
            .map
            .try_table(inputs.main)
            .ok_or_else(|| fatal("missing dimension descriptor"))?;
        let main = if item.balance {
            let mut fields = info
                .fields
                .iter()
                .enumerate()
                .filter(|(_, field)| field.md == Some(metadata) && field.side.is_none());
            let (index, _) = fields
                .next()
                .ok_or_else(|| fatal("missing balanced dimension UUID"))?;
            if fields.next().is_some() {
                return Err(fatal("ambiguous balanced dimension UUID"));
            }
            let common =
                FieldId(u16::try_from(index).map_err(|_| fatal("dimension field overflow"))?);
            [common, common]
        } else {
            let mut slots = [None; 2];
            for (index, field) in info
                .fields
                .iter()
                .enumerate()
                .filter(|(_, field)| field.md == Some(metadata))
            {
                let slot = match field.side {
                    Some(Side::Dr) => &mut slots[0],
                    Some(Side::Cr) => &mut slots[1],
                    None => return Err(fatal("unsided nonbalanced dimension UUID")),
                };
                let fid =
                    FieldId(u16::try_from(index).map_err(|_| fatal("dimension field overflow"))?);
                if slot.replace(fid).is_some() {
                    return Err(fatal("ambiguous nonbalanced dimension side"));
                }
            }
            let [Some(dr), Some(cr)] = slots else {
                return Err(fatal("missing nonbalanced dimension side"));
            };
            if dr == cr {
                return Err(fatal("aliased nonbalanced dimension sides"));
            }
            [dr, cr]
        };
        let dr_info = info
            .field(main[0])
            .ok_or_else(|| fatal("missing dimension debit descriptor"))?;
        let cr_info = info
            .field(main[1])
            .ok_or_else(|| fatal("missing dimension credit descriptor"))?;
        let dr_check = value_check_with_later(
            &field.ty,
            dr_info,
            self.storage,
            self.map,
            "Q6b-TurnoversStorageShape",
        )?;
        let cr_check = value_check_with_later(
            &field.ty,
            cr_info,
            self.storage,
            self.map,
            "Q6b-TurnoversStorageShape",
        )?;
        if dr_check != cr_check {
            return Err(fatal("incompatible dimension side checks"));
        }
        Ok(AccountingTurnoversDimensionL {
            metadata,
            output,
            main,
            value_check: dr_check,
        })
    }

    pub(crate) fn resource(
        &self,
        inputs: &AccountingTurnoversInputsL,
        metadata: Guid,
    ) -> R<AccountingTurnoversResourceL> {
        let (owner, _) = self.owner()?;
        let item = self
            .schema
            .meta(owner)
            .and_then(|meta| meta.resources.iter().find(|item| item.id == metadata))
            .ok_or_else(|| fatal("unknown resource UUID"))?;
        let numeric = item
            .ty
            .number
            .ok_or_else(|| later("Q6b-TurnoversStorageShape"))?;
        if item.ty.atom_count() != 1
            || numeric.digits == 0
            || numeric.digits > 38
            || numeric.scale > numeric.digits
        {
            return Err(later("Q6b-TurnoversStorageShape"));
        }
        if let Some(what) = nonbalanced_refusal(item) {
            return Err(later(what));
        }
        let info = self
            .map
            .try_table(inputs.main)
            .ok_or_else(|| fatal("missing resource descriptor"))?;
        let (main, null_zero) = if item.balance {
            let mut fields = info
                .fields
                .iter()
                .enumerate()
                .filter(|(_, field)| field.md == Some(metadata) && field.side.is_none());
            let (index, _) = fields
                .next()
                .ok_or_else(|| fatal("missing balanced resource UUID"))?;
            if fields.next().is_some() {
                return Err(fatal("ambiguous balanced resource UUID"));
            }
            let common =
                FieldId(u16::try_from(index).map_err(|_| fatal("resource field overflow"))?);
            ([common, common], [false; 2])
        } else {
            let mut slots = [None; 2];
            for (index, field) in info
                .fields
                .iter()
                .enumerate()
                .filter(|(_, field)| field.md == Some(metadata))
            {
                let slot = match field.side {
                    Some(Side::Dr) => &mut slots[0],
                    Some(Side::Cr) => &mut slots[1],
                    None => return Err(fatal("unsided nonbalanced resource UUID")),
                };
                let field =
                    FieldId(u16::try_from(index).map_err(|_| fatal("resource field overflow"))?);
                if slot.replace(field).is_some() {
                    return Err(fatal("ambiguous nonbalanced resource side"));
                }
            }
            let [Some(dr), Some(cr)] = slots else {
                return Err(fatal("missing nonbalanced resource side"));
            };
            if dr == cr {
                return Err(fatal("aliased nonbalanced resource sides"));
            }
            let nullable = matches!(
                item.accounting_flags.accounting,
                AccountingFlagBinding::Present { .. }
            );
            for field in [dr, cr] {
                if info
                    .field(field)
                    .is_none_or(|field| field.nullable != nullable)
                {
                    return Err(fatal("resource nullability contradicts flag provenance"));
                }
            }
            ([dr, cr], [nullable; 2])
        };
        for field in main {
            let ty = one_type(info, field)?;
            if ty.code != TypeCode::N
                || ty.length != u32::from(numeric.digits)
                || ty.precision != u32::from(numeric.scale)
            {
                return Err(fatal("stored resource type contradicts metadata"));
            }
        }
        let mut outputs = [None; 3];
        for (index, field) in self.schema.table(self.table).fields.iter().enumerate() {
            if field.md != Some(metadata) {
                continue;
            }
            let output =
                QFieldId(u16::try_from(index).map_err(|_| fatal("resource output overflow"))?);
            let Some(component) = self.component(output) else {
                continue;
            };
            let slot = outputs
                .get_mut(component)
                .ok_or_else(|| fatal("resource component offset"))?;
            if slot.replace(output).is_some()
                || field.ty.atom_count() != 1
                || field
                    .ty
                    .number
                    .is_none_or(|number| number.digits != 38 || number.scale != numeric.scale)
            {
                return Err(fatal("resource catalog type or component mismatch"));
            }
        }
        let [Some(o), Some(d), Some(c)] = outputs else {
            return Err(fatal("missing resource components"));
        };
        Ok(AccountingTurnoversResourceL {
            metadata,
            main,
            null_zero,
            ext_flag: None,
            dec: Dec {
                p: numeric.digits,
                s: numeric.scale,
            },
            outputs: [o, d, c],
        })
    }

    fn resolve_ext_flag_field(
        &self,
        section: TableId,
        chart_md: Guid,
        child_md: Guid,
    ) -> R<FieldId> {
        let mut accounts = self
            .schema
            .table(self.table)
            .fields
            .iter()
            .filter(|f| f.role == FieldRole::Std(StdRole::Account));
        let account = accounts
            .next()
            .ok_or_else(|| fatal("missing Account field"))?;
        if accounts.next().is_some() || account.ty.atom_count() != 1 || account.ty.refs.len() != 1 {
            return Err(fatal("ambiguous Account reference target"));
        }
        let atom = *account
            .ty
            .refs
            .first()
            .ok_or_else(|| fatal("missing Account reference atom"))?;
        let chart = self.schema.ref_info(atom).table;
        if self.schema.table(chart).object != Some(chart_md) {
            return Err(fatal("ExtDimensionAccountingFlag chart mismatch"));
        }
        let section_info = self
            .map
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

impl TurnoversInputs<'_> {
    fn recorder_reference_tags(&self) -> R<Vec<u32>> {
        let sem = self
            .schema
            .table(self.table)
            .fields
            .iter()
            .find(|f| f.period == Some(PeriodField::Recorder))
            .ok_or_else(|| fatal("missing semantic Recorder field"))?;
        let mut reference_tags = Vec::with_capacity(sem.ty.refs.len());
        for atom in &sem.ty.refs {
            let tag = self
                .storage
                .ref_number(*atom)
                .ok_or_else(|| fatal("unbound semantic Recorder reference"))?;
            reference_tags.push(tag);
        }
        reference_tags.sort_unstable();
        reference_tags.dedup();
        Ok(reference_tags)
    }

    fn resolve_recorder_field(&self, info: &TableInfo, reference_tags: &[u32]) -> R<FieldId> {
        let mut phys = info
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
        if !valid_recorder_types(&phys_info.types, reference_tags, self.map) {
            return Err(fatal("Recorder physical type contradicts metadata"));
        }
        Ok(field)
    }

    fn resolve_line_field(info: &TableInfo) -> R<FieldId> {
        let field = standard(info, StdField::LineNo, TypeCode::N)?;
        let ty = one_type(info, field)?;
        if ty.precision != 0
            || info
                .field(field)
                .is_none_or(|f| f.nullable || f.side.is_some())
        {
            return Err(fatal("invalid LineNo physical field"));
        }
        Ok(field)
    }

    pub(crate) fn used_period(
        &self,
        periodicity: Periodicity,
        used: &[QFieldId],
    ) -> Vec<(QFieldId, PeriodField)> {
        self.schema
            .table(self.table)
            .fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| {
                let period = field.period?;
                let qf = QFieldId(u16::try_from(index).ok()?);
                (used.contains(&qf) || forced_period(periodicity, period)).then_some((qf, period))
            })
            .collect()
    }

    pub(crate) fn resolve_period_fields(
        &self,
        inputs: &AccountingTurnoversInputsL,
        periodicity: Periodicity,
        used: &[QFieldId],
    ) -> R<(
        Option<FieldId>,
        Option<AccountingTurnoversRecorderL>,
        Option<FieldId>,
    )> {
        let used_period = self.used_period(periodicity, used);
        let need_period = used_period
            .iter()
            .any(|(_, p)| matches!(p, PeriodField::Period | PeriodField::Unit(_)));
        let need_recorder = used_period.iter().any(|(_, p)| *p == PeriodField::Recorder);
        let need_line = used_period
            .iter()
            .any(|(_, p)| *p == PeriodField::LineNumber);
        let main_info = self
            .map
            .try_table(inputs.main)
            .ok_or_else(|| fatal("missing main descriptor"))?;
        let period = if need_period {
            Some(standard(main_info, StdField::Period, TypeCode::T)?)
        } else {
            None
        };
        let recorder = if need_recorder {
            let reference_tags = self.recorder_reference_tags()?;
            let field = self.resolve_recorder_field(main_info, &reference_tags)?;
            Some(AccountingTurnoversRecorderL {
                field,
                reference_tags,
            })
        } else {
            None
        };
        let line = if need_line {
            Some(Self::resolve_line_field(main_info)?)
        } else {
            None
        };
        Ok((period, recorder, line))
    }
}

impl Lower<'_> {
    pub(super) fn accounting_turnovers(&mut self, view: &RegisterVtS) -> R<Row> {
        self.accounting_consumer(view, false)?;
        let outer_used = self
            .vt_used
            .get(view.src.0 as usize)
            .cloned()
            .unwrap_or_default();
        let mut cond_used: Vec<QFieldId> = Vec::new();
        if let Some(cond) = &view.cond {
            crate::bind::visit_expr(cond, false, &mut |expr, _| {
                if let ExprKind::Col {
                    src,
                    col: ColSel::Field(field),
                } = expr.kind
                    && src == view.inner
                    && !cond_used.contains(&field)
                {
                    cond_used.push(field);
                }
            });
        }
        let mut all_used = outer_used.clone();
        for field in &cond_used {
            if !all_used.contains(field) {
                all_used.push(*field);
            }
        }
        let resolver = turnovers_inputs(self.schema, self.storage, self.map, view.table);
        if let Some(what) = resolver.form_refusal(&all_used, false) {
            return Err(later(what));
        }
        let inputs = resolver.resolve()?;
        let dates = self.accounting_date_bounds(&view.bounds)?;
        let (period_from_fields, recorder, line) =
            resolver.resolve_period_fields(&inputs, view.periodicity, &outer_used)?;
        let period = match period_from_fields {
            Some(field) => Some(field),
            None if dates.iter().any(Option::is_some) => Some(standard(
                self.map.table(inputs.main),
                StdField::Period,
                TypeCode::T,
            )?),
            None => None,
        };
        let used_period = resolver.used_period(view.periodicity, &outer_used);
        let account_raw = all_used.contains(&inputs.account_output);
        let account_outer = outer_used.contains(&inputs.account_output);
        let mut dims: Vec<AccountingTurnoversDimensionL> = Vec::new();
        for output in &all_used {
            let Some(field) = self.schema.field(view.table, *output) else {
                continue;
            };
            if field.role != FieldRole::Dimension {
                continue;
            }
            if !dims.iter().any(|dim| dim.output == *output) {
                dims.push(resolver.dimension(&inputs, *output)?);
            }
        }
        let mut ext_dims = resolver.resolve_ext_dims(&inputs, &all_used)?;
        let mut areas = self.b.security.turnovers_areas.get(&view.table).cloned();
        if resolver.has_physical_separators(ext_dims.is_some()) && areas.is_none() {
            return Err(areas_refused());
        }
        if ext_dims.is_none()
            && let Some(ref mut area) = areas
        {
            match area {
                TurnoversAreasL::Exact { ext, ed, .. } => {
                    ext.clear();
                    ed.clear();
                }
                TurnoversAreasL::Unseparated { ext, ed, .. } => {
                    ext.clear();
                    ed.clear();
                }
            }
        } else if ext_dims.as_ref().is_some_and(|e| e.acc_rg_ed.is_none())
            && let Some(ref mut area) = areas
        {
            match area {
                TurnoversAreasL::Exact { ed, .. } => ed.clear(),
                TurnoversAreasL::Unseparated { ed, .. } => ed.clear(),
            }
        }
        let ext_count = ext_dims.as_ref().map_or(0, |ext| ext.slots.len());
        let raw_nk = usize::from(account_raw) + dims.len() + ext_count;
        let mut resources: Vec<AccountingTurnoversResourceL> = Vec::with_capacity(outer_used.len());
        for output in &outer_used {
            let Some(field) = self.schema.field(view.table, *output) else {
                continue;
            };
            if field.role == FieldRole::Dimension || field.period().is_some() {
                continue;
            }
            let Some(metadata) = field.md else {
                continue;
            };
            if !resources
                .iter()
                .any(|resource| resource.metadata == metadata)
            {
                resources.push(resolver.resource(&inputs, metadata)?);
            }
        }
        if let Some(ref mut ext) = ext_dims {
            if ext.slots.len() > 64 {
                return Err(later("Q6b-TurnoversExtDimensionLayout"));
            }
            let owner = self
                .schema
                .table(view.table)
                .owner
                .ok_or_else(|| fatal("missing register owner"))?;
            let meta = self
                .schema
                .meta(owner)
                .ok_or_else(|| fatal("missing register metadata"))?;
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
                        let field = resolver.resolve_ext_flag_field(ext.section, chart, child)?;
                        let idx = if let Some(pos) =
                            ext.ext_flag_fields.iter().position(|f| *f == field)
                        {
                            pos
                        } else {
                            if ext.ext_flag_fields.len() >= 64 {
                                return Err(later("Q6b-TurnoversExtDimensionLayout"));
                            }
                            ext.ext_flag_fields.push(field);
                            ext.ext_flag_fields.len() - 1
                        };
                        resource.ext_flag =
                            Some(u16::try_from(idx).map_err(|_| fatal("ext flag index overflow"))?);
                    }
                    _ => return Err(later("Q6b-TurnoversExtDimensionAccountingFlag")),
                }
            }
        }
        p16(raw::width(raw_nk, resources.len()) as i64)?;
        let mut projection =
            vec![PExpr::Lit(QValue::Null); self.schema.table(view.table).fields.len()];
        let start_lit = dates[0].map(|d| PExpr::Lit(QValue::Date(d)));
        let cut = |x: PExpr| -> PExpr {
            match &start_lit {
                Some(s) => PExpr::Case {
                    operand: None,
                    whens: vec![(
                        PExpr::Compare(CmpOp::Lt, Box::new(x.clone()), Box::new(s.clone())),
                        s.clone(),
                    )],
                    else_: Some(Box::new(x)),
                },
                None => x,
            }
        };
        let begin = |kw: Kw, x: PExpr| -> PExpr {
            PExpr::Func {
                f: Func::BeginOfPeriod,
                args: vec![x, PExpr::Unit(kw)],
                ty: QType::date(DateQ::DateTime),
                arg_dec: Dec::UNKNOWN,
            }
        };
        let mut keys = Vec::with_capacity(used_period.len() + raw_nk);
        for (output, period_field) in &used_period {
            let expr = match (period_field, view.periodicity) {
                (PeriodField::Period, Periodicity::Unit(kw)) => {
                    cut(begin(kw, PExpr::Col(raw::PERIOD as u16)))
                }
                (PeriodField::Period, _) => PExpr::Col(raw::PERIOD as u16),
                (PeriodField::Unit(kw), _) => cut(begin(*kw, PExpr::Col(raw::PERIOD as u16))),
                (PeriodField::Recorder, _) => PExpr::Col(raw::RECORDER as u16),
                (PeriodField::LineNumber, _) => PExpr::Col(raw::LINE as u16),
            };
            let col = p16(keys.len() as i64)?;
            keys.push(expr);
            if outer_used.contains(output) {
                *projection
                    .get_mut(usize::from(output.0))
                    .ok_or_else(|| fatal("period projection offset"))? = PExpr::Col(col);
            }
        }
        if account_outer {
            let col = p16(keys.len() as i64)?;
            keys.push(PExpr::Col(p16(raw::DIMS as i64)?));
            *projection
                .get_mut(usize::from(inputs.account_output.0))
                .ok_or_else(|| fatal("account projection offset"))? = PExpr::Col(col);
        }
        for (index, dim) in dims.iter().enumerate() {
            if !outer_used.contains(&dim.output) {
                continue;
            }
            let col = p16(keys.len() as i64)?;
            keys.push(PExpr::Col(p16(
                (raw::DIMS + usize::from(account_raw) + index) as i64,
            )?));
            *projection
                .get_mut(usize::from(dim.output.0))
                .ok_or_else(|| fatal("dimension projection offset"))? = PExpr::Col(col);
        }
        if let Some(ext) = &ext_dims {
            for (index, slot) in ext.slots.iter().enumerate() {
                if !outer_used.contains(&slot.output) {
                    continue;
                }
                let col = p16(keys.len() as i64)?;
                keys.push(PExpr::Col(p16(
                    (raw::DIMS + usize::from(account_raw) + dims.len() + index) as i64,
                )?));
                *projection
                    .get_mut(usize::from(slot.output.0))
                    .ok_or_else(|| fatal("ExtDimension projection offset"))? = PExpr::Col(col);
            }
        }
        let agg_keys = keys.len();
        let mut aggs = Vec::with_capacity(resources.len().saturating_mul(3));
        let mut wanted = Vec::with_capacity(outer_used.len());
        for (r, resource) in resources.iter().enumerate() {
            for (index, component) in COMPONENTS.iter().enumerate() {
                let output = resource
                    .outputs
                    .get(index)
                    .copied()
                    .ok_or_else(|| fatal("aggregate component offset"))?;
                let column = p16((agg_keys + r * 3 + index) as i64)?;
                if outer_used.contains(&output) {
                    *projection
                        .get_mut(usize::from(output.0))
                        .ok_or_else(|| fatal("resource projection offset"))? = PExpr::Col(column);
                    wanted.push(PExpr::Compare(
                        CmpOp::Ne,
                        Box::new(PExpr::Col(column)),
                        Box::new(PExpr::Lit(QValue::Number(bsl_prim::Number::zero()))),
                    ));
                }
                aggs.push(AggL {
                    f: Func::Sum,
                    distinct: false,
                    arg: Some(PExpr::Col(p16(
                        raw::component(raw_nk, r, *component) as i64
                    )?)),
                    ty: self
                        .schema
                        .field(view.table, output)
                        .ok_or_else(|| fatal("missing aggregate type"))?
                        .ty
                        .clone()
                        .with_null(),
                    arg_dec: resource.dec,
                });
            }
        }
        let register = self
            .schema
            .table(view.register)
            .object
            .ok_or_else(|| fatal("missing register UUID"))?;
        let base = raw::width(raw_nk, resources.len());
        let policy_fields = self.accounting_policy_fields(view, base)?;
        let policy_len = policy_fields.len();
        let secured = self.b.security.accounting_sources.contains_key(&view.src);
        let mut raw_cols = Vec::with_capacity(raw::DIMS + raw_nk);
        if view.cond.is_some() {
            raw_cols.extend([QFieldId(u16::MAX); raw::DIMS]);
            if account_raw {
                raw_cols.push(inputs.account_output);
            }
            for dim in &dims {
                raw_cols.push(dim.output);
            }
            if let Some(ext) = &ext_dims {
                for slot in &ext.slots {
                    raw_cols.push(slot.output);
                }
            }
        }
        let account_filter = if !secured && let Some(cond) = &view.cond {
            self.extract_turnovers_account_filter(
                cond,
                view.inner,
                inputs.account_output,
                inputs.ref_table,
            )
        } else {
            None
        };
        let rows_rel = RelL::RegisterRows(Box::new(RegisterRowsL {
            register,
            kind: view.kind,
            view: view.span,
            rows: RegisterRowsKindL::AccountingTurnovers(AccountingTurnoversL {
                inputs,
                areas,
                account: account_raw,
                account_filter,
                dims,
                ext_dims,
                resources,
                dates,
                period,
                recorder,
                line,
                policy_fields,
                secured,
            }),
        }));
        let (rows_rel, base_for_secure, cond_signal) = if let Some(cond) = &view.cond {
            self.slice_cols.insert(view.inner, raw_cols);
            self.set_level(view.inner);
            self.inner += 1;
            let raw_row = Row {
                signal: None,
                width: base + policy_len,
                rel: rows_rel,
                rep: Some(view.inner),
                keys: None,
            };
            let filtered = self.filter_row(raw_row, cond);
            self.inner -= 1;
            self.slice_cols.remove(&view.inner);
            let filtered = filtered?;
            let sec_base = filtered
                .width
                .checked_sub(usize::from(filtered.signal.is_some()) + policy_len)
                .ok_or_else(|| fatal("filtered row width underflow"))?;
            (filtered.rel, sec_base, filtered.signal)
        } else {
            (rows_rel, base, None)
        };
        let rows = self.accounting_secure(view, rows_rel, base_for_secure, cond_signal)?;
        if let Some(signal) = rows.signal {
            projection.push(PExpr::Col(p16((agg_keys + aggs.len()) as i64)?));
            aggs.push(super::security::min(signal));
        }
        let signal = rows
            .signal
            .map(|_| p16((projection.len() - 1) as i64))
            .transpose()?;
        let grouped = RelL::Aggregate {
            input: Box::new(rows.rel),
            keys,
            sets: None,
            aggs,
        };
        self.added = self.added.saturating_add(3);
        self.set_level(view.src);
        Ok(Row {
            signal,
            width: projection.len(),
            rel: RelL::Project {
                input: Box::new(RelL::Filter {
                    input: Box::new(grouped),
                    pred: PExpr::Or(wanted),
                }),
                exprs: projection,
            },
            rep: Some(view.src),
            keys: None,
        })
    }

    fn const_values_of<'e>(&'e self, expr: &'e Expr) -> Option<Vec<&'e QValue>> {
        match &expr.kind {
            ExprKind::Lit(v) => Some(vec![v]),
            ExprKind::Value(idx) => self.b.values.get(usize::from(*idx)).map(|v| vec![v]),
            ExprKind::Param(slot) => match self.b.params.get(usize::from(*slot)) {
                Some(crate::bind::PBound::Value(v)) => Some(vec![v]),
                Some(crate::bind::PBound::Set(set)) => Some(set.values.iter().collect()),
                _ => None,
            },
            _ => None,
        }
    }

    fn extract_turnovers_account_filter(
        &self,
        cond: &Expr,
        inner: SrcId,
        account_output: QFieldId,
        ref_table: u32,
    ) -> Option<AccountFilterSpec> {
        let is_account_col = |e: &Expr| -> bool {
            matches!(
                e.kind,
                ExprKind::Col {
                    src,
                    col: ColSel::Field(f)
                } if src == inner && f == account_output
            )
        };
        let mut stack = vec![cond];
        let mut conjuncts = Vec::new();
        while let Some(x) = stack.pop() {
            match &x.kind {
                ExprKind::And(v) => stack.extend(v.iter().rev()),
                _ => conjuncts.push(x),
            }
        }
        let mut exact_ids: Option<Vec<[u8; 16]>> = None;
        let mut hier: Option<(TableId, FieldId, FieldId, Vec<[u8; 16]>)> = None;

        for c in conjuncts {
            match &c.kind {
                ExprKind::Compare(CmpOp::Eq, a, b) => {
                    let rhs = if is_account_col(a) {
                        b
                    } else if is_account_col(b) {
                        a
                    } else {
                        continue;
                    };
                    let Some(vals) = self.const_values_of(rhs) else {
                        continue;
                    };
                    let [val] = vals.as_slice() else {
                        continue;
                    };
                    let mut ids = Vec::new();
                    if let QValue::Ref { table, id } = *val
                        && *table == ref_table
                        && *id != [0; 16]
                    {
                        ids.push(*id);
                    }
                    if ids.is_empty() {
                        return Some((None, Vec::new()));
                    }
                    exact_ids = Some(match exact_ids {
                        None => ids,
                        Some(mut prev) => {
                            prev.retain(|x| ids.binary_search(x).is_ok());
                            if prev.is_empty() {
                                return Some((None, Vec::new()));
                            }
                            prev
                        }
                    });
                }
                ExprKind::InList {
                    l,
                    not: false,
                    hierarchy,
                    rhs,
                } if l.len() == 1 && l.first().is_some_and(is_account_col) => {
                    let mut vals = Vec::new();
                    let mut all_const = true;
                    for item in rhs {
                        let Some(vs) = self.const_values_of(item) else {
                            all_const = false;
                            break;
                        };
                        vals.extend(vs);
                    }
                    if !all_const {
                        continue;
                    }
                    let hier_tables = if *hierarchy {
                        self.hier_tables(l)
                    } else {
                        Vec::new()
                    };
                    if let [ht] = hier_tables.as_slice()
                        && ht.number == ref_table
                    {
                        let mut seeds: Vec<[u8; 16]> = vals
                            .into_iter()
                            .filter_map(|v| match v {
                                QValue::Ref { table, id } if *table == ref_table => Some(*id),
                                _ => None,
                            })
                            .collect();
                        seeds.sort_unstable();
                        seeds.dedup();
                        if seeds.is_empty() {
                            return Some((None, Vec::new()));
                        }
                        if hier.is_none() {
                            hier = Some((ht.table, ht.id, ht.parent, seeds));
                        }
                    } else {
                        let mut ids: Vec<[u8; 16]> = vals
                            .into_iter()
                            .filter_map(|v| match v {
                                QValue::Ref { table, id }
                                    if *table == ref_table && *id != [0; 16] =>
                                {
                                    Some(*id)
                                }
                                _ => None,
                            })
                            .collect();
                        ids.sort_unstable();
                        ids.dedup();
                        if ids.is_empty() {
                            return Some((None, Vec::new()));
                        }
                        exact_ids = Some(match exact_ids {
                            None => ids,
                            Some(mut prev) => {
                                prev.retain(|x| ids.binary_search(x).is_ok());
                                if prev.is_empty() {
                                    return Some((None, Vec::new()));
                                }
                                prev
                            }
                        });
                    }
                }
                _ => {}
            }
        }
        if let Some(ids) = exact_ids {
            Some((None, ids))
        } else if let Some((chart, id, parent, seeds)) = hier {
            Some((Some((chart, id, parent)), seeds))
        } else {
            None
        }
    }
}
