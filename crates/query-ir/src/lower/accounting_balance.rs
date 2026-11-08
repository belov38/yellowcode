use super::layout::{R, Row, p16};
use super::rel::Lower;
use super::{AggL, Dec, PExpr, RegisterRowsKindL, RegisterRowsL, RelL};
use crate::ir::{RegisterVtS, raw_layout as raw};
use crate::schema::{FieldRole, QTableId, QuerySchema, StdRole, StorageBinding, TableKind, VtKind};
use crate::{ExecError, NumQ, QType, QValue};
use ibschema::logical::{LType, TypeCode};
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, StorageMap, TableId, TableInfo};
use mdschema::MdKind;
use query_syntax::{Func, ast::CmpOp};

pub(crate) fn balance_later(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q6a" }
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

fn balance_ext1_declares_chart(schema: &QuerySchema, table: QTableId) -> bool {
    schema.table(table).fields.iter().any(|field| {
        field.role == FieldRole::Std(StdRole::ExtDimension(1))
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

#[derive(Clone, Debug, PartialEq)]
pub struct BalanceResourceL {
    pub metadata: brace::Guid,
    pub main: FieldId,
    pub dec: Dec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountingChartLayoutL {
    pub id: u16,
    pub kind: u16,
    pub signal: Option<u16>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BalanceRowsL {
    pub ext1: Option<crate::QShared<super::BalanceExt1L>>,
    pub chart_input: Option<Box<RelL>>,
    pub chart_layout: Option<AccountingChartLayoutL>,
    pub policy_fields: Vec<FieldId>,
    pub secured: bool,
    pub inputs: BalanceInputsL,
    pub resources: Vec<BalanceResourceL>,
    pub date: Option<bsl_prim::BslDate>,
    pub period: Option<FieldId>,
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
        let item = meta
            .resources
            .iter()
            .find(|r| r.id == *metadata)
            .ok_or_else(|| balance_later("Q6a-AccountingBalanceColumns"))?;
        if !item.balance {
            return Err(balance_later("Q6a-AccountingBalanceNonBalancedResource"));
        }
        let qualifiers = item
            .ty
            .number
            .ok_or_else(|| balance_later("Q6a-AccountingBalanceResourceShape"))?;
        let mut fields = main
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.md == Some(*metadata));
        let (index, field) = fields
            .next()
            .ok_or_else(|| fatal("missing balanced main resource"))?;
        if fields.next().is_some() || field.side.is_some() {
            return Err(fatal("ambiguous balanced resource UUID"));
        }
        if field.nullable {
            return Err(balance_later("Q6a-AccountingBalanceNullableResource"));
        }
        if !matches!(field.types.as_slice(), [t] if t.code == TypeCode::N && t.length == u32::from(qualifiers.digits) && t.precision == u32::from(qualifiers.scale))
        {
            return Err(fatal("resource numeric type contradicts metadata"));
        }
        if qualifiers.digits == 0 || qualifiers.digits > 38 || qualifiers.scale > qualifiers.digits
        {
            return Err(balance_later("Q6a-AccountingBalanceResourceShape"));
        }
        result.push(BalanceResourceL {
            metadata: *metadata,
            main: FieldId(u16::try_from(index).map_err(|_| fatal("field index overflow"))?),
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
    if !meta.corr || meta.adjustment || meta.dims.iter().any(|d| !d.balance) {
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
        ) && !(phase.enabled() && *s == StdRole::ExtDimension(1))
    }) {
        return Err(balance_later("Q6a-AccountingBalanceColumns"));
    }
    if !standard.contains(&StdRole::Account) {
        return Err(balance_later("Q6a-AccountingBalanceAccountRequired"));
    }
    if resources.is_empty() {
        return Err(balance_later("Q6a-AccountingBalanceNoResource"));
    }
    if phase.enabled()
        && standard.contains(&StdRole::ExtDimension(1))
        && balance_ext1_declares_chart(schema, table)
    {
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
    pub(super) fn accounting_balance(&mut self, v: &RegisterVtS) -> R<Row> {
        self.accounting_consumer(v, true)?;
        let used = self
            .vt_used
            .get(v.src.0 as usize)
            .cloned()
            .unwrap_or_default();
        let table = self.schema.table(v.table);
        let meta = self
            .schema
            .meta(v.register)
            .ok_or_else(|| fatal("missing register metadata"))?;
        let mut ids = Vec::new();
        let mut roles = Vec::new();
        for id in &used {
            let field = self
                .schema
                .field(v.table, *id)
                .ok_or_else(|| fatal("unknown output"))?;
            if let FieldRole::Std(role) = field.role {
                roles.push(role);
            }
            if let Some(md) = field.md
                && !ids.contains(&md)
            {
                ids.push(md);
            }
        }
        balance_form(
            self.schema,
            v.table,
            self.stmt
                .sources
                .get(v.src.0 as usize)
                .is_some_and(|s| s.all_fields),
            &roles,
            &ids,
            self.phase,
        )?;
        let first = roles.contains(&StdRole::ExtDimension(1));
        let dims = 2 + usize::from(first);
        let keys = 1 + usize::from(first);
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
        let resources = balance_resources(self.schema, self.map, v.table, &inputs, &ids)?;
        if first {
            balance_ext1_static_inputs(
                self.schema,
                self.storage,
                self.map,
                v.table,
                &inputs,
                &ids,
            )?;
        }
        p16(raw::width(dims, resources.len()) as i64)?;
        let zero = || PExpr::Lit(QValue::Number(bsl_prim::Number::zero()));
        let compare = |op, a, b| PExpr::Compare(op, Box::new(a), Box::new(b));
        let case = |pred, yes| PExpr::Case {
            operand: None,
            whens: vec![(pred, yes)],
            else_: Some(Box::new(zero())),
        };
        let kind = PExpr::Col(p16(keys as i64)?);
        let mut aggs = vec![AggL {
            f: Func::Max,
            distinct: false,
            arg: Some(PExpr::Col(p16((raw::DIMS + 1) as i64)?)),
            ty: QType::num(NumQ {
                digits: 1,
                scale: 0,
                nonneg: true,
            }),
            arg_dec: Dec { p: 1, s: 0 },
        }];
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
        let mut projection = vec![PExpr::Lit(QValue::Null); table.fields.len()];
        for id in &used {
            let field = self
                .schema
                .field(v.table, *id)
                .ok_or_else(|| fatal("missing field"))?;
            let expr = if field.role == FieldRole::Std(StdRole::Account) {
                PExpr::Col(0)
            } else if field.role == FieldRole::Std(StdRole::ExtDimension(1)) {
                PExpr::Col(1)
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
                let b = PExpr::Col(p16((index + keys + 1) as i64)?);
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
                    1 => case(
                        PExpr::Or(vec![is_kind(0), PExpr::And(vec![is_kind(2), positive])]),
                        b,
                    ),
                    2 => case(
                        PExpr::Or(vec![is_kind(1), PExpr::And(vec![is_kind(2), negative])]),
                        PExpr::Neg(Box::new(b)),
                    ),
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
                        PExpr::Col(p16((i + keys + 1) as i64)?),
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
        let (chart_input, chart_layout) = self.accounting_chart(v, &inputs)?;
        let base = raw::width(dims, resources.len());
        let policy_fields = self.accounting_policy_fields(
            v,
            base + usize::from(chart_layout.is_some_and(|l| l.signal.is_some())),
        )?;
        let ext1 = if let (true, Some(memory)) = (first, self.phase.memory()) {
            let identity =
                balance_ext1_storage(self.schema, self.storage, self.map, v.table, &inputs)?;
            let mut fields = table
                .fields
                .iter()
                .filter(|f| f.role == FieldRole::Std(StdRole::ExtDimension(1)));
            let field = fields
                .next()
                .ok_or_else(|| fatal("missing semantic ExtDimension1"))?;
            if fields.next().is_some() {
                return Err(fatal("duplicate semantic ExtDimension1"));
            }
            let resource_types = resources.iter().map(|resource| {
                meta.resources
                    .iter()
                    .find(|item| item.id == resource.metadata)
                    .map(|item| (resource.main, &item.ty))
                    .ok_or_else(|| fatal("missing semantic resource"))
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
            Some(super::accounting_balance_payload::build(
                (
                    identity.section,
                    identity.owner,
                    identity.ordinal,
                    identity.turnover_only,
                    identity.values,
                ),
                &field.ty,
                inputs.main,
                resource_types.chain(policy_types),
                self.storage,
                self.map,
                memory,
            )?)
        } else {
            None
        };
        let secured = self.b.security.accounting_sources.contains_key(&v.src);
        let chart_signal = chart_layout
            .and_then(|l| l.signal)
            .map(|_| p16(base as i64))
            .transpose()?;
        let rows = RelL::RegisterRows(Box::new(RegisterRowsL {
            register,
            kind: v.kind,
            view: v.span,
            rows: RegisterRowsKindL::AccountingBalance(BalanceRowsL {
                ext1,
                inputs,
                resources,
                date,
                period,
                chart_input,
                chart_layout,
                policy_fields,
                secured,
            }),
        }));
        let rows = self.accounting_secure(v, rows, base, chart_signal)?;
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
            keys: if first {
                vec![
                    PExpr::Col(p16(raw::DIMS as i64)?),
                    PExpr::Col(p16((raw::DIMS + 2) as i64)?),
                ]
            } else {
                vec![PExpr::Col(p16(raw::DIMS as i64)?)]
            },
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BalanceExt1Storage {
    section: TableId,
    owner: FieldId,
    ordinal: FieldId,
    turnover_only: FieldId,
    values: [FieldId; 2],
}

fn balance_ext1_storage(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
) -> R<BalanceExt1Storage> {
    let logical = balance_ext1_section_identity(schema, table)
        .ok_or_else(|| fatal("missing required first subconto section identity"))?;
    let chart = schema
        .table(logical)
        .owner
        .ok_or_else(|| fatal("missing first subconto logical owner"))?;
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
        .ok_or_else(|| fatal("missing required first subconto storage list"))?;
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
        return Err(fatal("ambiguous or aliased first subconto storage binding"));
    }
    let section_info = map
        .try_table(section)
        .ok_or_else(|| fatal("missing first subconto storage table"))?;
    if section_info.owner != Some(inputs.chart)
        || section_info.md != Some(chart_md)
        || section_info.purpose != role::TABULAR_SECTION
        || section_info.query_name != schema.table(logical).name_en
    {
        return Err(fatal("contradictory first subconto storage ownership"));
    }
    for id in [inputs.main, inputs.options, inputs.chart, section] {
        let info = map
            .try_table(id)
            .ok_or_else(|| fatal("missing required input"))?;
        if !info.separators.is_empty() {
            return Err(ExecError::Later {
                what: "Q6a-AccountingBalanceAreas",
                stage: "Q7c",
            });
        }
    }
    let owner = standard(section_info, StdField::Id, TypeCode::R)?;
    let owner_ty = ty(section_info, owner)?;
    if owner_ty.ref_kind != 2 || map.table_by_name(&owner_ty.ref_table) != Some(inputs.chart) {
        return Err(fatal("first subconto owner reference mismatch"));
    }
    let ordinal = standard(section_info, StdField::LineNo, TypeCode::N)?;
    let ordinal_ty = ty(section_info, ordinal)?;
    if (ordinal_ty.length, ordinal_ty.precision) != (5, 0) {
        return Err(fatal("first subconto ordinal qualifiers mismatch"));
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
    let main = map
        .try_table(inputs.main)
        .ok_or_else(|| fatal("missing main table"))?;
    fn exact_value(info: &TableInfo, std: StdField, name: &str) -> R<FieldId> {
        let mut fields = info
            .fields
            .iter()
            .enumerate()
            .filter(|(_, field)| field.name == name);
        let (index, field) = fields
            .next()
            .ok_or_else(|| fatal("missing required first subconto value"))?;
        if fields.next().is_some() || field.std != Some(std) || field.side.is_some() {
            return Err(fatal("contradictory first subconto value identity"));
        }
        u16::try_from(index)
            .map(FieldId)
            .map_err(|_| fatal("field index overflow"))
    }
    let values = [
        exact_value(main, StdField::ValueDt, "ValueDt1")?,
        exact_value(main, StdField::ValueCt, "ValueCt1")?,
    ];
    Ok(BalanceExt1Storage {
        section,
        owner,
        ordinal,
        turnover_only,
        values,
    })
}

pub(crate) fn balance_ext1_static_inputs(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
    inputs: &BalanceInputsL,
    used: &[brace::Guid],
) -> R<()> {
    let owner = schema
        .table(table)
        .owner
        .ok_or_else(|| fatal("missing owner"))?;
    let meta = schema
        .meta(owner)
        .ok_or_else(|| fatal("missing accounting metadata"))?;
    for (ext, what) in [
        (false, "Q6a-AccountingBalanceAccountingFlag"),
        (true, "Q6a-AccountingBalanceExtDimensionAccountingFlag"),
    ] {
        for id in used {
            let item = meta
                .resources
                .iter()
                .find(|r| r.id == *id)
                .ok_or_else(|| fatal("missing selected resource metadata"))?;
            let flag = if ext {
                item.accounting_flags.ext_dimension
            } else {
                item.accounting_flags.accounting
            };
            if !matches!(flag, crate::schema::AccountingFlagBinding::Absent) {
                return Err(balance_later(what));
            }
        }
    }
    let identity = balance_ext1_storage(schema, storage, map, table, inputs)?;
    let mut fields = schema
        .table(table)
        .fields
        .iter()
        .filter(|f| f.role == FieldRole::Std(StdRole::ExtDimension(1)));
    let field = fields
        .next()
        .ok_or_else(|| fatal("missing semantic ExtDimension1"))?;
    if fields.next().is_some() {
        return Err(fatal("duplicate semantic ExtDimension1"));
    }
    let main = map
        .try_table(inputs.main)
        .ok_or_else(|| fatal("missing main"))?;
    let dr = main
        .field(identity.values[0])
        .ok_or_else(|| fatal("missing Dt1"))?;
    let cr = main
        .field(identity.values[1])
        .ok_or_else(|| fatal("missing Ct1"))?;
    super::accounting_balance_payload::validate_shape(&field.ty, dr, storage, map)?;
    super::accounting_balance_payload::validate_shape(&field.ty, cr, storage, map)?;
    if dr.nullable != cr.nullable {
        return Err(fatal("side nullable mismatch"));
    }
    Ok(())
}
