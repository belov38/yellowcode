use super::deref::plan_expr;
use super::layout::{R, Row, fatal, internal, p16};
use super::rel::Lower;
use super::{PExpr, RelL};
use crate::ir::RegisterVtS;
use crate::schema::{FieldRole, QFieldId, QTableId, QuerySchema, StdRole, StorageBinding};
use crate::value::QValue;
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, StorageMap, TableId};
use query_syntax::ast::{CmpOp, JoinKind};

pub(crate) fn actual_inputs(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
) -> R<(TableId, TableId)> {
    let source = schema.table(table);
    let owner = source.owner.ok_or_else(|| {
        crate::ExecError::Fatal("actual action period without register owner".into())
    })?;
    let object = schema.table(owner).object.ok_or_else(|| {
        crate::ExecError::Fatal("actual action period without register UUID".into())
    })?;
    let Some(main) = storage.table(table) else {
        return fatal(format!(
            "таблица {} не привязана к хранению",
            source.name_ru
        ));
    };
    if storage.table(owner) != Some(main) {
        return internal("actual action period main binding differs from its register");
    }
    let Some(act) = map
        .object(object)
        .and_then(|o| o.service.iter().find(|(r, _)| r == role::ACTION_PERIODS))
        .map(|(_, id)| *id)
    else {
        return fatal(format!(
            "{}: ActionPeriods storage is not bound",
            source.name_ru
        ));
    };
    for id in [main, act] {
        let Some(info) = map.try_table(id) else {
            return internal("actual action period input missing from storage map");
        };
        for field in [
            StdField::Recorder,
            StdField::LineNo,
            StdField::ApDateFrom,
            StdField::ApDateTill,
        ] {
            if info.field_by_std(field).is_none() {
                return fatal(format!(
                    "{}: actual action period input {} lacks {field:?}",
                    source.name_ru, info.logical
                ));
            }
        }
    }
    Ok((main, act))
}

fn position(fields: &[FieldId], field: FieldId, offset: usize) -> R<PExpr> {
    fields
        .iter()
        .position(|f| *f == field)
        .ok_or_else(|| crate::ExecError::Fatal("actual action period scan field missing".into()))
        .and_then(|i| p16((offset + i) as i64).map(PExpr::Col))
}

impl Lower<'_> {
    pub(super) fn actual_action_period(&mut self, v: &RegisterVtS) -> R<Row> {
        self.calculation_consumer(v)?;
        self.added = self.added.saturating_add(7);
        let (main, act) = actual_inputs(self.schema, self.storage, self.map, v.table)?;
        let vt = self.schema.table(v.table);
        let field_role = |field: QFieldId| {
            vt.fields
                .get(usize::from(field.0))
                .map(|f| f.role)
                .ok_or_else(|| {
                    crate::ExecError::Fatal("actual action period schema field missing".into())
                })
        };
        let mut needed = std::collections::HashSet::new();
        for src in [v.src, v.inner] {
            needed.extend(
                self.vt_used
                    .get(src.0 as usize)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
        }
        let required: Vec<_> = (0..vt.fields.len())
            .map(|i| p16(i as i64).map(QFieldId))
            .collect::<R<Vec<_>>>()?
            .into_iter()
            .filter(|f| needed.contains(f))
            .collect();
        self.slice_cols.insert(v.src, required.clone());
        self.slice_cols.insert(v.inner, required.clone());
        let main_info = self.map.table(main);
        let act_info = self.map.table(act);
        let std = |info: &ibschema::storage::TableInfo, field| {
            info.field_by_std(field).ok_or_else(|| {
                crate::ExecError::Fatal("actual action period standard field missing".into())
            })
        };
        let main_keys = [
            std(main_info, StdField::Recorder)?,
            std(main_info, StdField::LineNo)?,
        ];
        let act_keys = [
            std(act_info, StdField::Recorder)?,
            std(act_info, StdField::LineNo)?,
        ];
        let start = std(act_info, StdField::ApDateFrom)?;
        let mut stored = main_keys.to_vec();
        let mut fragments = vec![act_keys[0], act_keys[1], start];
        let mut plans = Vec::with_capacity(required.len());
        for f in &required {
            let plan = self.plan(v.table, *f)?;
            let mut fields = Vec::new();
            plan.needs(&mut fields);
            for (input, field) in fields {
                if input != main {
                    return internal("actual action period field outside main input");
                }
                if !stored.contains(&field) {
                    stored.push(field);
                }
            }
            if field_role(*f)? == FieldRole::Std(StdRole::EndOfActionPeriod) {
                let end = std(act_info, StdField::ApDateTill)?;
                if !fragments.contains(&end) {
                    fragments.push(end);
                }
            }
            plans.push(plan);
        }
        let mut physical = self.calculation_scan(v, false, main, &mut stored)?;
        let main_width = physical.width;
        let input_signal = physical.signal;
        let absent = PExpr::IsNull {
            l: Box::new(position(&fragments, start, main_width)?),
            not: false,
        };
        let mut projection = Vec::with_capacity(required.len());
        for (f, plan) in required.iter().zip(&plans) {
            let original = plan_expr(plan, &mut |(_, field)| position(&stored, field, 0))?;
            let fragment = match field_role(*f)? {
                FieldRole::Std(StdRole::BegOfActionPeriod) => Some(start),
                FieldRole::Std(StdRole::EndOfActionPeriod) => {
                    Some(std(act_info, StdField::ApDateTill)?)
                }
                _ => None,
            };
            projection.push(if let Some(field) = fragment {
                PExpr::Case {
                    operand: None,
                    whens: vec![(absent.clone(), original)],
                    else_: Some(Box::new(position(&fragments, field, main_width)?)),
                }
            } else {
                original
            });
        }
        let eq = main_keys
            .iter()
            .zip(act_keys)
            .map(|(l, r)| Ok((position(&stored, *l, 0)?, position(&fragments, r, 0)?)))
            .collect::<R<Vec<_>>>()?;
        let on = PExpr::And(
            main_keys
                .iter()
                .zip(act_keys)
                .map(|(l, r)| {
                    Ok(PExpr::Compare(
                        CmpOp::Eq,
                        Box::new(position(&stored, *l, 0)?),
                        Box::new(position(&fragments, r, main_width)?),
                    ))
                })
                .collect::<R<Vec<_>>>()?,
        );
        let sentinel = PExpr::Or(vec![
            absent,
            PExpr::Compare(
                CmpOp::Ne,
                Box::new(position(&fragments, start, main_width)?),
                Box::new(PExpr::Lit(QValue::Date(bsl_prim::BslDate::EMPTY))),
            ),
        ]);
        let output_signal = input_signal
            .map(|_| p16(projection.len() as i64))
            .transpose()?;
        if input_signal.is_some() {
            projection.push(physical.signal_expr());
        }
        let mut row = Row {
            signal: output_signal,
            rel: RelL::Project {
                input: Box::new(RelL::Filter {
                    input: Box::new(RelL::Join {
                        kind: JoinKind::Left,
                        l: physical.take(),
                        r: Box::new(RelL::Scan {
                            table: act,
                            fields: fragments,
                        }),
                        on,
                        eq,
                        by_id: false,
                    }),
                    pred: sentinel,
                }),
                exprs: projection,
            },
            width: required.len() + usize::from(output_signal.is_some()),
            rep: Some(v.inner),
            keys: None,
        };
        self.set_level(v.inner);
        self.inner += 1;
        let result = (|| {
            if let Some(condition) = &v.cond {
                row = self.filter_row(row, condition)?;
            }
            let mut exprs = (0..required.len())
                .map(|i| p16(i as i64).map(PExpr::Col))
                .collect::<R<Vec<_>>>()?;
            let signal = row
                .signal
                .map(|_| {
                    let pos = exprs.len();
                    exprs.push(row.signal_expr());
                    p16(pos as i64)
                })
                .transpose()?;
            row.width = exprs.len();
            row.rel = RelL::Project {
                input: row.take(),
                exprs,
            };
            row.signal = signal;
            row.rep = Some(v.src);
            self.set_level(v.src);
            Ok(row)
        })();
        self.inner -= 1;
        result
    }
}
