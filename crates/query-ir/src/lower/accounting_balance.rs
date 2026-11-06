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
        )
    }) {
        return Err(balance_later("Q6a-AccountingBalanceColumns"));
    }
    if !standard.contains(&StdRole::Account) {
        return Err(balance_later("Q6a-AccountingBalanceAccountRequired"));
    }
    if resources.is_empty() {
        return Err(balance_later("Q6a-AccountingBalanceNoResource"));
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
        )?;
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
        p16(raw::width(2, resources.len()) as i64)?;
        let zero = || PExpr::Lit(QValue::Number(bsl_prim::Number::zero()));
        let compare = |op, a, b| PExpr::Compare(op, Box::new(a), Box::new(b));
        let case = |pred, yes| PExpr::Case {
            operand: None,
            whens: vec![(pred, yes)],
            else_: Some(Box::new(zero())),
        };
        let kind = PExpr::Col(1);
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
                arg: Some(PExpr::Col(p16(raw::component(2, i, raw::BALANCE) as i64)?)),
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
                let b = PExpr::Col(p16((index + 2) as i64)?);
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
                .map(|i| Ok(compare(CmpOp::Ne, PExpr::Col(p16((i + 2) as i64)?), zero())))
                .collect::<R<Vec<_>>>()?,
        );
        let register = self
            .schema
            .table(v.register)
            .object
            .ok_or_else(|| fatal("missing register UUID"))?;
        let (chart_input, chart_layout) = self.accounting_chart(v, &inputs)?;
        let base = raw::width(2, resources.len());
        let policy_fields = self.accounting_policy_fields(
            v,
            base + usize::from(chart_layout.is_some_and(|l| l.signal.is_some())),
        )?;
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
            projection.push(PExpr::Col(p16((1 + aggs.len()) as i64)?));
            aggs.push(super::security::min(signal));
        }
        let signal = rows
            .signal
            .map(|_| p16((projection.len() - 1) as i64))
            .transpose()?;
        let grouped = RelL::Aggregate {
            input: Box::new(rows.rel),
            keys: vec![PExpr::Col(p16(raw::DIMS as i64)?)],
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
