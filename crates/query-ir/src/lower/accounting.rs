use super::layout::{R, Row, p16};
use super::rel::Lower;
use super::vt::DrCrAreaL;
use super::{
    AccountingRowsL, AggL, Dec, DrCrAccountL, DrCrInputsL, DrCrResourceL, DrCrSideL, PExpr,
    RegisterRowsKindL, RegisterRowsL, RelL,
};
use crate::ir::{RegisterVtS, raw_layout as raw};
use crate::schema::{
    FieldRole, QFieldId, QTableId, QuerySchema, Side, StdRole, StorageBinding, TableKind, VtKind,
};
use crate::{ExecError, QValue};
use brace::Guid;
use ibschema::logical::{LType, TypeCode};
use ibschema::names::{Side as StorageSide, StdField, role};
use ibschema::storage::{FieldId, StorageMap, TableId, TableInfo};
use mdschema::MdKind;
use query_syntax::{Func, ast::CmpOp};

fn fatal(message: impl std::fmt::Display) -> ExecError {
    ExecError::Fatal(format!("accounting DrCrTurnovers: {message}"))
}
fn later(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q6b" }
}

pub(crate) struct DrCrInputs<'a> {
    schema: &'a QuerySchema,
    storage: &'a StorageBinding,
    map: &'a StorageMap,
    table: QTableId,
}

pub(crate) fn drcr_inputs<'a>(
    schema: &'a QuerySchema,
    storage: &'a StorageBinding,
    map: &'a StorageMap,
    table: QTableId,
) -> DrCrInputs<'a> {
    DrCrInputs {
        schema,
        storage,
        map,
        table,
    }
}

fn standard(info: &TableInfo, role: StdField) -> R<FieldId> {
    let mut matches = info
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.std == Some(role));
    let (index, _) = matches
        .next()
        .ok_or_else(|| fatal(format_args!("missing {role:?} in {}", info.logical)))?;
    if matches.next().is_some() {
        return Err(fatal(format_args!(
            "ambiguous {role:?} in {}",
            info.logical
        )));
    }
    u16::try_from(index)
        .map(FieldId)
        .map_err(|_| fatal("field index overflow"))
}

fn one_type(info: &TableInfo, field: FieldId) -> R<&LType> {
    match info.field(field).map(|f| f.types.as_slice()) {
        Some([ty]) => Ok(ty),
        _ => Err(fatal(format_args!(
            "ambiguous type of field {field:?} in {}",
            info.logical
        ))),
    }
}

fn typed_standard(info: &TableInfo, role: StdField, code: TypeCode) -> R<FieldId> {
    let field = standard(info, role)?;
    if one_type(info, field)?.code != code {
        return Err(fatal(format_args!(
            "wrong type of {role:?} in {}",
            info.logical
        )));
    }
    Ok(field)
}

impl DrCrInputs<'_> {
    fn owner(&self) -> R<(QTableId, Guid)> {
        let vt = self.schema.table(self.table);
        if vt.kind != TableKind::Virtual(VtKind::DrCrTurnovers) {
            return Err(fatal("not a DrCrTurnovers source"));
        }
        let owner = vt.owner.ok_or_else(|| fatal("missing metadata owner"))?;
        let table = self.schema.table(owner);
        if table.kind != TableKind::Object(MdKind::AccountingRegister)
            || !self.schema.meta(owner).is_some_and(|m| m.corr)
        {
            return Err(fatal("owner is not a correspondence accounting register"));
        }
        Ok((
            owner,
            table.object.ok_or_else(|| fatal("missing register UUID"))?,
        ))
    }

    pub(crate) fn has_physical_separators(&self) -> bool {
        let Ok((owner, md)) = self.owner() else {
            return false;
        };
        let Some(object) = self.map.object(md) else {
            return false;
        };
        std::iter::once(object.main)
            .chain(self.storage.table(owner))
            .chain(
                object
                    .service
                    .iter()
                    .filter(|(r, _)| {
                        r == role::MAIN
                            || r == role::TOTALS_BETWEEN_ACCOUNTS
                            || r == role::ACCOUNT_REGISTER_TOTALS_OPTIONS
                    })
                    .map(|(_, t)| *t),
            )
            .any(|t| {
                self.map
                    .try_table(t)
                    .is_some_and(|t| !t.separators.is_empty())
            })
    }

    pub(crate) fn area_mapping(&self) -> R<Option<(Guid, FieldId, FieldId)>> {
        let inputs = self.resolve()?;
        let main = self.map.table(inputs.main);
        let ct = self.map.table(inputs.ct);
        let options = self.map.table(inputs.options);
        let refused = || ExecError::Later {
            what: "Q6b-DrCrTurnoversAreas",
            stage: "Q7c",
        };
        if !options.separators.is_empty() {
            return Err(refused());
        }
        if main.separators.is_empty() && ct.separators.is_empty() {
            return Ok(None);
        }
        let ([main_area], [ct_area]) = (main.separators.as_slice(), ct.separators.as_slice())
        else {
            return Err(refused());
        };
        if main_area.common_attribute == Guid::NIL
            || main_area.common_attribute != ct_area.common_attribute
            || main_area.use_field.is_some()
            || ct_area.use_field.is_some()
        {
            return Err(refused());
        }
        for (info, area) in [(main, main_area), (ct, ct_area)] {
            let field = info.field(area.field).ok_or_else(refused)?;
            let [ty] = field.types.as_slice() else {
                return Err(refused());
            };
            if field.std.is_some() || ty.code != TypeCode::N || ty.precision != 0 || ty.length == 0
            {
                return Err(refused());
            }
        }
        Ok(Some((
            main_area.common_attribute,
            main_area.field,
            ct_area.field,
        )))
    }

    pub(crate) fn bind_area(
        &self,
        filters: &[crate::host::SeparatorFilter],
    ) -> R<Option<DrCrAreaL>> {
        let Some((common_attribute, main, ct)) = self.area_mapping()? else {
            return Ok(None);
        };
        let refused = || ExecError::Later {
            what: "Q6b-DrCrTurnoversAreas",
            stage: "Q7c",
        };
        let mut matches = filters
            .iter()
            .filter(|f| f.common_attribute == common_attribute);
        let filter = matches.next().ok_or_else(refused)?;
        if matches.next().is_some() {
            return Err(refused());
        }
        let QValue::Number(value) = &filter.value else {
            return Err(refused());
        };
        let value = value.to_i64().ok_or_else(refused)?;
        let inputs = self.resolve()?;
        for (table, field) in [(inputs.main, main), (inputs.ct, ct)] {
            let digits = one_type(self.map.table(table), field)?.length;
            if digits < 19 && value.unsigned_abs() >= 10u64.pow(digits) {
                return Err(refused());
            }
        }
        Ok(Some(DrCrAreaL { main, ct, value }))
    }

    pub(crate) fn resolve(&self) -> R<DrCrInputsL> {
        let (owner, md) = self.owner()?;
        let object = self
            .map
            .object(md)
            .ok_or_else(|| fatal("missing register storage object"))?;
        let main = object.main;
        if object.service.iter().any(|(r, _)| r == role::MAIN) {
            return Err(fatal("ambiguous main role"));
        }
        if self.storage.table(owner) != Some(main) {
            return Err(fatal("main binding differs from metadata owner"));
        }
        let service = |role: &str| -> R<TableId> {
            let mut matches = object.service.iter().filter(|(r, _)| r == role);
            let (_, table) = matches
                .next()
                .ok_or_else(|| fatal(format_args!("missing service {role}")))?;
            if matches.next().is_some() {
                return Err(fatal(format_args!("ambiguous service {role}")));
            }
            Ok(*table)
        };
        let ct = service(role::TOTALS_BETWEEN_ACCOUNTS)?;
        let options = service(role::ACCOUNT_REGISTER_TOTALS_OPTIONS)?;
        if main == ct || main == options || ct == options {
            return Err(fatal("aliased main/CT/options roles"));
        }
        let info = |id| {
            self.map
                .try_table(id)
                .ok_or_else(|| fatal(format_args!("missing table {id:?}")))
        };
        let m = info(main)?;
        let c = info(ct)?;
        let o = info(options)?;
        if m.md != Some(md)
            || m.purpose != role::MAIN
            || c.md != Some(md)
            || c.purpose != role::TOTALS_BETWEEN_ACCOUNTS
            || o.purpose != role::ACCOUNT_REGISTER_TOTALS_OPTIONS
        {
            return Err(fatal("inconsistent main/CT/options ownership or purpose"));
        }
        let main_period = typed_standard(m, StdField::Period, TypeCode::T)?;
        let ct_period = typed_standard(c, StdField::Period, TypeCode::T)?;
        let main_active = typed_standard(m, StdField::Active, TypeCode::L)?;
        for (role, code) in [
            (StdField::RegId, TypeCode::B),
            (StdField::Period, TypeCode::T),
            (StdField::ActualPeriod, TypeCode::L),
            (StdField::UseTotals, TypeCode::N),
            (StdField::MinPeriod, TypeCode::T),
            (StdField::UseSplitter, TypeCode::L),
        ] {
            let field = typed_standard(o, role, code)?;
            let ty = one_type(o, field)?;
            if (role == StdField::RegId && ty.length != 16)
                || (role == StdField::UseTotals && (ty.length, ty.precision) != (1, 0))
            {
                return Err(fatal(format_args!("wrong logical shape of {role:?}")));
            }
        }
        if o.fields
            .iter()
            .any(|f| f.std == Some(StdField::MinCalculatedPeriod))
        {
            typed_standard(o, StdField::MinCalculatedPeriod, TypeCode::T)?;
        }
        let account = |std_role, storage_role| -> R<DrCrAccountL> {
            let mut matches = self
                .schema
                .table(self.table)
                .fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.role == FieldRole::Std(std_role));
            let (index, field) = matches
                .next()
                .ok_or_else(|| fatal(format_args!("missing catalog {std_role:?}")))?;
            if matches.next().is_some() || field.ty.refs.len() != 1 || field.ty.atom_count() != 1 {
                return Err(fatal("ambiguous account metadata target"));
            }
            let atom = *field
                .ty
                .refs
                .first()
                .ok_or_else(|| fatal("missing chart reference atom"))?;
            let target = self
                .storage
                .ref_number(atom)
                .ok_or_else(|| fatal("unbound chart reference"))?;
            let chart = self
                .map
                .ref_table(target)
                .ok_or_else(|| fatal("unknown chart reference table"))?;
            let a = typed_standard(m, storage_role, TypeCode::R)?;
            let b = typed_standard(c, storage_role, TypeCode::R)?;
            for (info, field) in [(m, a), (c, b)] {
                let ty = one_type(info, field)?;
                if ty.ref_kind != 3 || self.map.table_by_name(&ty.ref_table) != Some(chart) {
                    return Err(fatal(format_args!("wrong target of {storage_role:?}")));
                }
            }
            Ok(DrCrAccountL {
                output: QFieldId(
                    u16::try_from(index).map_err(|_| fatal("catalog field index overflow"))?,
                ),
                main: a,
                ct: b,
                ref_table: target,
            })
        };
        let accounts = [
            account(StdRole::AccountDr, StdField::AccountDt)?,
            account(StdRole::AccountCr, StdField::AccountCt)?,
        ];
        if accounts[0].main == accounts[1].main || accounts[0].ct == accounts[1].ct {
            return Err(fatal("aliased account fields"));
        }
        Ok(DrCrInputsL {
            main,
            ct,
            options,
            main_period,
            ct_period,
            main_active,
            accounts,
        })
    }

    pub(crate) fn resource(
        &self,
        inputs: &DrCrInputsL,
        metadata: Guid,
        output: QFieldId,
    ) -> R<DrCrResourceL> {
        let (owner, _) = self.owner()?;
        let mut items = self
            .schema
            .meta(owner)
            .ok_or_else(|| fatal("missing resource metadata"))?
            .resources
            .iter()
            .filter(|r| r.id == metadata);
        let item = items
            .next()
            .ok_or_else(|| fatal(format_args!("unknown resource {metadata}")))?;
        if items.next().is_some() {
            return Err(fatal(format_args!("ambiguous resource {metadata}")));
        }
        if !item.ty.only_number() {
            return Err(later("Q6b-DrCrTurnoversStorageShape"));
        }
        let catalog = self
            .schema
            .field(self.table, output)
            .ok_or_else(|| fatal("missing resource catalog output"))?;
        if catalog.role != FieldRole::Virtual
            || catalog.md != Some(metadata)
            || item.balance != catalog.side.is_none()
        {
            return Err(fatal("resource output UUID/side mismatch"));
        }
        if !catalog.ty.only_number() {
            return Err(fatal("non-numeric resource catalog output"));
        }
        let side = catalog.side.map(|side| match side {
            Side::Dr => StorageSide::Dr,
            Side::Cr => StorageSide::Cr,
        });
        let numeric = item
            .ty
            .number
            .ok_or_else(|| later("Q6b-DrCrTurnoversStorageShape"))?;
        let resource = |table| -> R<(FieldId, u32)> {
            let info = self
                .map
                .try_table(table)
                .ok_or_else(|| fatal("missing resource table"))?;
            let mut sides = [None; 3];
            for (index, field) in info
                .fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.md == Some(metadata))
            {
                let [unsided, dr, cr] = &mut sides;
                let stored = match field.side {
                    None => unsided,
                    Some(StorageSide::Dr) => dr,
                    Some(StorageSide::Cr) => cr,
                };
                if field.std.is_some() || item.balance != field.side.is_none() || stored.is_some() {
                    return Err(fatal(format_args!(
                        "invalid resource {metadata} side {:?} in {}",
                        field.side, info.logical
                    )));
                }
                *stored = Some(FieldId(
                    u16::try_from(index).map_err(|_| fatal("resource field index overflow"))?,
                ));
            }
            if (item.balance && sides[0].is_none())
                || (!item.balance && (sides[1].is_none() || sides[2].is_none()))
            {
                return Err(fatal(format_args!(
                    "incomplete resource {metadata} layout in {}",
                    info.logical
                )));
            }
            let [unsided, dr, cr] = sides;
            let field = match side {
                None => unsided,
                Some(StorageSide::Dr) => dr,
                Some(StorageSide::Cr) => cr,
            };
            let field = field.ok_or_else(|| fatal("missing selected resource side"))?;
            let ty = one_type(info, field)?;
            if ty.code != TypeCode::N {
                return Err(fatal(format_args!("non-numeric resource {metadata}")));
            }
            let p = u32::from(numeric.digits)
                .checked_add(if table == inputs.ct { 6 } else { 0 })
                .ok_or_else(|| fatal("resource precision overflow"))?;
            if ty.length != p || ty.precision != u32::from(numeric.scale) {
                return Err(fatal(format_args!(
                    "numeric type contradicts resource {metadata}"
                )));
            }
            Ok((field, p))
        };
        let (main, main_p) = resource(inputs.main)?;
        let (ct, ct_p) = resource(inputs.ct)?;
        if catalog
            .ty
            .number
            .is_none_or(|q| q.digits != 38 || q.scale != numeric.scale)
        {
            return Err(fatal("resource catalog precision/scale mismatch"));
        }
        if numeric.digits == 0 || numeric.scale > numeric.digits || main_p > 38 || ct_p > 38 {
            return Err(later("Q6b-DrCrTurnoversStorageShape"));
        }
        let main_dec = Dec {
            p: u8::try_from(main_p).map_err(|_| later("Q6b-DrCrTurnoversStorageShape"))?,
            s: numeric.scale,
        };
        let ct_dec = Dec {
            p: u8::try_from(ct_p).map_err(|_| later("Q6b-DrCrTurnoversStorageShape"))?,
            s: numeric.scale,
        };
        Ok(DrCrResourceL {
            metadata,
            output,
            main,
            ct,
            main_dec,
            ct_dec,
        })
    }

    pub(crate) fn form_refusal(&self, used: &[QFieldId], all: bool) -> Option<&'static str> {
        if all {
            return Some("Q6b-DrCrTurnoversStar");
        }
        let vt = self.schema.table(self.table);
        let owner = vt.owner?;
        let meta = self.schema.meta(owner)?;
        let mut rank = usize::MAX;
        let mut resources = false;
        for field in used
            .iter()
            .filter_map(|f| self.schema.field(self.table, *f))
        {
            if matches!(
                field.role,
                FieldRole::Std(StdRole::AccountDr | StdRole::AccountCr)
            ) {
                continue;
            }
            if field.period().is_some()
                || matches!(
                    field.role,
                    FieldRole::Std(StdRole::Period | StdRole::Recorder | StdRole::LineNumber)
                )
            {
                rank = 0;
                continue;
            }
            if field
                .md
                .is_some_and(|md| meta.resources.iter().any(|r| r.id == md))
            {
                resources = true;
            } else if field.nested.is_some()
                || field.md.is_some()
                || matches!(
                    field.role,
                    FieldRole::Std(
                        StdRole::ExtDimensionDr(_)
                            | StdRole::ExtDimensionCr(_)
                            | StdRole::ExtDimensionTypeDr(_)
                            | StdRole::ExtDimensionTypeCr(_)
                    )
                )
            {
                rank = rank.min(1);
            } else {
                rank = rank.min(3);
            }
        }
        match rank {
            0 => Some("Q6b-DrCrTurnoversPeriodFields"),
            1 => Some("Q6b-DrCrTurnoversDimensions"),
            3 => Some("Q6b-DrCrTurnoversColumns"),
            _ if !resources => Some("Q6b-DrCrTurnoversNoResource"),
            _ => None,
        }
    }
}

impl Lower<'_> {
    pub(super) fn accounting_date_bounds(
        &mut self,
        bounds: &[Option<crate::ir::Expr>; 2],
    ) -> R<[Option<bsl_prim::BslDate>; 2]> {
        let mut dates = [None, None];
        for (date, expression) in dates.iter_mut().zip(bounds) {
            let Some(expression) = expression else {
                continue;
            };
            let mut empty = Row::empty();
            match self.expr(expression, &mut empty)? {
                PExpr::Lit(QValue::Date(value)) if empty.width == 0 => {
                    *date = (value != bsl_prim::BslDate::EMPTY).then_some(value);
                }
                _ => return Err(fatal("a plain Date bound did not bind to Date")),
            }
        }
        Ok(dates)
    }

    pub(super) fn drcr(&mut self, v: &RegisterVtS) -> R<Row> {
        let used = self
            .vt_used
            .get(v.src.0 as usize)
            .cloned()
            .unwrap_or_default();
        let resolver = drcr_inputs(self.schema, self.storage, self.map, v.table);
        if let Some(what) = resolver.form_refusal(&used, false) {
            return Err(later(what));
        }
        let inputs = resolver.resolve()?;
        let area = self.b.security.drcr_areas.get(&v.table).copied();
        if resolver.has_physical_separators() && area.is_none() {
            return Err(ExecError::Later {
                what: "Q6b-DrCrTurnoversAreas",
                stage: "Q7c",
            });
        }
        let mut date_bounds = [None, None];
        for (date, expression) in date_bounds.iter_mut().zip(&v.bounds) {
            let Some(expression) = expression else {
                continue;
            };
            let mut empty = Row::empty();
            let bound = self.expr(expression, &mut empty)?;
            match bound {
                PExpr::Lit(QValue::Date(value)) if empty.width == 0 => *date = Some(value),
                _ => return Err(fatal("a plain Date bound did not bind to Date")),
            }
        }
        let nk = inputs
            .accounts
            .iter()
            .filter(|a| used.contains(&a.output))
            .count();
        let mut keys = Vec::with_capacity(nk);
        for (side, account) in [
            (DrCrSideL::Dr, &inputs.accounts[0]),
            (DrCrSideL::Cr, &inputs.accounts[1]),
        ] {
            if used.contains(&account.output) {
                keys.push(side);
            }
        }
        let selected = self
            .schema
            .table(v.table)
            .fields
            .iter()
            .enumerate()
            .filter(|(i, f)| {
                f.md.is_some() && u16::try_from(*i).is_ok_and(|i| used.contains(&QFieldId(i)))
            })
            .count();
        let mut resources = Vec::with_capacity(selected);
        for (index, field) in self.schema.table(v.table).fields.iter().enumerate() {
            let Some(md) = field.md else { continue };
            let output =
                QFieldId(u16::try_from(index).map_err(|_| fatal("resource output index"))?);
            if used.contains(&output) {
                resources.push(resolver.resource(&inputs, md, output)?);
            }
        }
        let nk = keys.len();
        let nr = resources.len();
        let width = nr
            .checked_mul(raw::PER_RESOURCE)
            .and_then(|n| n.checked_add(nk))
            .and_then(|n| n.checked_add(raw::DIMS))
            .ok_or_else(|| fatal("raw width overflow"))?;
        p16(width as i64)?;
        let mut projection =
            vec![PExpr::Lit(QValue::Null); self.schema.table(v.table).fields.len()];
        for (i, side) in keys.iter().enumerate() {
            let account = match side {
                DrCrSideL::Dr => &inputs.accounts[0],
                DrCrSideL::Cr => &inputs.accounts[1],
            };
            *projection
                .get_mut(usize::from(account.output.0))
                .ok_or_else(|| fatal("account output offset"))? = PExpr::Col(p16(i as i64)?);
        }
        let mut aggs = Vec::with_capacity(nr);
        for (i, resource) in resources.iter().enumerate() {
            *projection
                .get_mut(usize::from(resource.output.0))
                .ok_or_else(|| fatal("resource output offset"))? =
                PExpr::Col(p16((nk + i) as i64)?);
            aggs.push(AggL {
                f: Func::Sum,
                distinct: false,
                arg: Some(PExpr::Col(
                    p16(raw::component(nk, i, raw::TURNOVER) as i64)?,
                )),
                ty: self
                    .schema
                    .field(v.table, resource.output)
                    .ok_or_else(|| fatal("missing resource output"))?
                    .ty
                    .clone()
                    .with_null(),
                arg_dec: resource.main_dec,
            });
        }
        let register = self
            .schema
            .table(v.register)
            .object
            .ok_or_else(|| fatal("missing register UUID"))?;
        let step = RegisterRowsL {
            register,
            kind: v.kind,
            view: v.span,
            rows: RegisterRowsKindL::Accounting(AccountingRowsL {
                inputs,
                keys,
                resources,
                date_bounds,
                area,
            }),
        };
        let raw = RelL::RegisterRows(Box::new(step));
        let grouped = RelL::Aggregate {
            input: Box::new(raw),
            keys: (0..nk)
                .map(|i| p16((raw::DIMS + i) as i64).map(PExpr::Col))
                .collect::<R<Vec<_>>>()?,
            sets: None,
            aggs,
        };
        let having = PExpr::Or(
            (0..nr)
                .map(|i| {
                    Ok(PExpr::Compare(
                        CmpOp::Ne,
                        Box::new(PExpr::Col(p16((nk + i) as i64)?)),
                        Box::new(PExpr::Lit(QValue::Number(bsl_prim::Number::zero()))),
                    ))
                })
                .collect::<R<Vec<_>>>()?,
        );
        self.added = self.added.saturating_add(3);
        self.set_level(v.src);
        Ok(Row {
            signal: None,
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
