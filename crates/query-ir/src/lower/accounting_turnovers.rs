use super::layout::{R, Row, p16};
use super::rel::Lower;
use super::{AggL, Dec, PExpr, RegisterRowsKindL, RegisterRowsL, RelL};
use crate::ir::{RegisterVtS, raw_layout as raw};
use crate::schema::{
    AccountingFlagBinding, FieldRole, Item, QFieldId, QTableId, QuerySchema, StdRole,
    StorageBinding, TableKind, VtKind,
};
use crate::{ExecError, QValue};
use brace::Guid;
use ibschema::logical::{LType, TypeCode};
use ibschema::names::{Side, StdField, role};
use ibschema::storage::{FieldId, StorageMap, TableId, TableInfo};
use mdschema::MdKind;
use query_syntax::{Func, ast::CmpOp};

const COMPONENTS: [usize; 3] = [raw::TURNOVER, raw::RECEIPT, raw::EXPENSE];

#[derive(Clone, Debug, PartialEq)]
pub struct AccountingTurnoversL {
    pub policy_fields: Vec<FieldId>,
    pub secured: bool,
    pub inputs: AccountingTurnoversInputsL,
    pub account: bool,
    pub resources: Vec<AccountingTurnoversResourceL>,
    pub dates: [Option<bsl_prim::BslDate>; 2],
    pub period: Option<FieldId>,
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

    pub(crate) fn has_physical_separators(&self) -> bool {
        let Ok((owner, id)) = self.owner() else {
            return false;
        };
        let Some(object) = self.map.object(id) else {
            return false;
        };
        std::iter::once(object.main)
            .chain(self.storage.table(owner))
            .chain(
                object
                    .service
                    .iter()
                    .filter(|(role_name, _)| {
                        role_name == role::MAIN
                            || role_name == role::ACCOUNT_REGISTER_TOTALS_OPTIONS
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
        for id in used {
            let Some(field) = self.schema.field(self.table, *id) else {
                continue;
            };
            if field.role == FieldRole::Std(StdRole::Account) {
                continue;
            }
            if field.period().is_some()
                || matches!(
                    field.role,
                    FieldRole::Std(StdRole::Period | StdRole::Recorder | StdRole::LineNumber)
                )
            {
                rank = 0;
            } else if let Some(item) = meta.resources.iter().find(|item| Some(item.id) == field.md)
            {
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
            _ => None,
        }
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
            dec: Dec {
                p: numeric.digits,
                s: numeric.scale,
            },
            outputs: [o, d, c],
        })
    }
}

impl Lower<'_> {
    pub(super) fn accounting_turnovers(&mut self, view: &RegisterVtS) -> R<Row> {
        self.accounting_consumer(view, false)?;
        let used = self
            .vt_used
            .get(view.src.0 as usize)
            .cloned()
            .unwrap_or_default();
        let resolver = turnovers_inputs(self.schema, self.storage, self.map, view.table);
        if let Some(what) = resolver.form_refusal(&used, false) {
            return Err(later(what));
        }
        let inputs = resolver.resolve()?;
        let dates = self.accounting_date_bounds(&view.bounds)?;
        let period = if dates.iter().any(Option::is_some) {
            Some(standard(
                self.map.table(inputs.main),
                StdField::Period,
                TypeCode::T,
            )?)
        } else {
            None
        };
        let account = used.contains(&inputs.account_output);
        let nk = usize::from(account);
        let mut resources: Vec<AccountingTurnoversResourceL> = Vec::with_capacity(used.len());
        for output in &used {
            let Some(metadata) = self
                .schema
                .field(view.table, *output)
                .and_then(|field| field.md)
            else {
                continue;
            };
            if !resources
                .iter()
                .any(|resource| resource.metadata == metadata)
            {
                resources.push(resolver.resource(&inputs, metadata)?);
            }
        }
        p16(raw::width(nk, resources.len()) as i64)?;
        let mut projection =
            vec![PExpr::Lit(QValue::Null); self.schema.table(view.table).fields.len()];
        if account {
            *projection
                .get_mut(usize::from(inputs.account_output.0))
                .ok_or_else(|| fatal("account projection offset"))? = PExpr::Col(0);
        }
        let mut aggs = Vec::with_capacity(resources.len().saturating_mul(3));
        let mut wanted = Vec::with_capacity(used.len());
        for (r, resource) in resources.iter().enumerate() {
            for (index, component) in COMPONENTS.iter().enumerate() {
                let output = resource
                    .outputs
                    .get(index)
                    .copied()
                    .ok_or_else(|| fatal("aggregate component offset"))?;
                let column = p16((nk + r * 3 + index) as i64)?;
                if used.contains(&output) {
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
                    arg: Some(PExpr::Col(p16(raw::component(nk, r, *component) as i64)?)),
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
        let base = raw::width(nk, resources.len());
        let policy_fields = self.accounting_policy_fields(view, base)?;
        let secured = self.b.security.accounting_sources.contains_key(&view.src);
        let rows = RelL::RegisterRows(Box::new(RegisterRowsL {
            register,
            kind: view.kind,
            view: view.span,
            rows: RegisterRowsKindL::AccountingTurnovers(AccountingTurnoversL {
                inputs,
                account,
                resources,
                dates,
                period,
                policy_fields,
                secured,
            }),
        }));
        let rows = self.accounting_secure(view, rows, base, None)?;
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
            keys: if account {
                vec![PExpr::Col(p16(raw::DIMS as i64)?)]
            } else {
                Vec::new()
            },
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
}
