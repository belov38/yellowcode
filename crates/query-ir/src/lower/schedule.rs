use super::deref::plan_expr;
use super::layout::{R, Row, internal, p16};
use super::rel::Lower;
use super::{AggL, Dec, PExpr, RelL};
use crate::ir::RegisterVtS;
use crate::schema::{FieldRole, QFieldId, QTableId, QuerySchema, StorageBinding};
use crate::{ExecError, QType, QValue};
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, StorageMap, TableId};
use query_syntax::ast::{CmpOp, JoinKind};
use query_syntax::{Func, Kw};

pub(crate) struct Inputs {
    main: TableId,
    act: TableId,
    graph: TableId,
    date: FieldId,
    resource: FieldId,
    links: Vec<(FieldId, FieldId)>,
    base: bool,
    resource_type: QType,
}
impl Inputs {
    pub(crate) fn tables(&self) -> [TableId; 3] {
        [self.main, self.act, self.graph]
    }
}
fn missing(what: &str) -> ExecError {
    ExecError::Fatal(format!("ScheduleData: {what}"))
}
fn standard(map: &StorageMap, t: TableId, f: StdField) -> R<FieldId> {
    map.try_table(t)
        .and_then(|t| t.field_by_std(f))
        .ok_or_else(|| missing(&format!("input lacks {f:?}")))
}
fn metadata(map: &StorageMap, t: TableId, md: brace::Guid) -> R<FieldId> {
    map.try_table(t)
        .and_then(|t| t.fields.iter().position(|f| f.md == Some(md)))
        .map(|i| p16(i as i64).map(FieldId))
        .transpose()?
        .ok_or_else(|| missing("metadata field is not bound"))
}
pub(crate) fn schedule_inputs(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
) -> R<Inputs> {
    let source = schema.table(table);
    let owner = source
        .owner
        .ok_or_else(|| missing("register owner is absent"))?;
    let object = schema
        .table(owner)
        .object
        .ok_or_else(|| missing("register UUID is absent"))?;
    let facts = schema
        .schedule(table)
        .ok_or_else(|| missing("metadata facts are absent"))?;
    if let Some(what) = facts.error {
        return Err(ExecError::Later { what, stage: "Q6c" });
    }
    let main = storage
        .table(table)
        .ok_or_else(|| missing("main storage is not bound"))?;
    if storage.table(owner) != Some(main) {
        return internal("ScheduleData main binding differs from its register");
    }
    let act = map
        .object(object)
        .and_then(|o| o.service.iter().find(|(r, _)| r == role::ACTION_PERIODS))
        .map(|(_, id)| *id)
        .ok_or_else(|| missing("ActionPeriods storage is not bound"))?;
    let graph_q = facts
        .graph
        .ok_or_else(|| missing("schedule metadata is absent"))?;
    let graph = storage
        .table(graph_q)
        .ok_or_else(|| missing("schedule storage is not bound"))?;
    for (index, field) in source.fields.iter().enumerate() {
        if field.role != FieldRole::Virtual
            && storage.field(table, QFieldId(p16(index as i64)?)).is_none()
        {
            return Err(missing("main field is not bound"));
        }
    }
    for (t, fields) in [
        (
            main,
            vec![
                StdField::Recorder,
                StdField::LineNo,
                StdField::Period,
                StdField::ActionPeriod,
                StdField::ApDateFrom,
                StdField::ApDateTill,
            ],
        ),
        (
            act,
            vec![
                StdField::Recorder,
                StdField::LineNo,
                StdField::ApDateFrom,
                StdField::ApDateTill,
            ],
        ),
    ] {
        for f in fields {
            standard(map, t, f)?;
        }
    }
    if facts.base {
        for f in [StdField::BpDateFrom, StdField::BpDateTill] {
            standard(map, main, f)?;
        }
    }
    let date = metadata(
        map,
        graph,
        facts
            .date
            .ok_or_else(|| missing("schedule date is absent"))?,
    )?;
    let resource_md = facts
        .resource
        .ok_or_else(|| missing("schedule resource is absent"))?;
    let resource = metadata(map, graph, resource_md)?;
    let resource_type = schema
        .table(graph_q)
        .fields
        .iter()
        .find(|f| f.md == Some(resource_md))
        .ok_or_else(|| missing("schedule resource type is absent"))?
        .ty
        .clone();
    let links = facts
        .links
        .iter()
        .map(|(a, b)| Ok((metadata(map, main, *a)?, metadata(map, graph, *b)?)))
        .collect::<R<_>>()?;
    Ok(Inputs {
        main,
        act,
        graph,
        date,
        resource,
        links,
        base: facts.base,
        resource_type,
    })
}
fn col(n: usize) -> R<PExpr> {
    p16(n as i64).map(PExpr::Col)
}
fn position(fields: &[FieldId], f: FieldId) -> R<PExpr> {
    col(fields
        .iter()
        .position(|x| *x == f)
        .ok_or_else(|| missing("scan field is absent"))?)
}
fn compare(op: CmpOp, a: PExpr, b: PExpr) -> PExpr {
    PExpr::Compare(op, Box::new(a), Box::new(b))
}
fn not_null(a: PExpr) -> PExpr {
    PExpr::IsNull {
        l: Box::new(a),
        not: true,
    }
}
fn month_end(a: PExpr) -> PExpr {
    PExpr::Func {
        f: Func::DateAdd,
        args: vec![
            a,
            PExpr::Unit(Kw::Month),
            PExpr::Lit(QValue::Number(bsl_prim::Number::from_i64(1))),
        ],
        ty: QType::date(crate::types::DateQ::DateTime),
        arg_dec: Dec::UNKNOWN,
    }
}
fn copy(row: &Row) -> Row {
    Row {
        rel: row.rel.clone(),
        signal: row.signal,
        width: row.width,
        rep: row.rep,
        keys: row.keys,
    }
}
fn data_width(row: &Row) -> usize {
    row.width - usize::from(row.signal.is_some())
}
fn project(mut input: Row, mut exprs: Vec<PExpr>) -> R<Row> {
    let signal = input.signal.map(|_| p16(exprs.len() as i64)).transpose()?;
    if signal.is_some() {
        exprs.push(input.signal_expr());
    }
    Ok(Row {
        width: exprs.len(),
        signal,
        rel: RelL::Project {
            input: input.take(),
            exprs,
        },
        rep: None,
        keys: None,
    })
}
fn combine(mut l: Row, mut r: Row, on: PExpr, eq: Vec<(PExpr, PExpr)>) -> R<Row> {
    let left = data_width(&l);
    let right = data_width(&r);
    let offset = l.width;
    let mut exprs = (0..left).map(col).collect::<R<Vec<_>>>()?;
    exprs.extend((0..right).map(|i| col(offset + i)).collect::<R<Vec<_>>>()?);
    let signal = if l.signal.is_some() || r.signal.is_some() {
        let pos = p16(exprs.len() as i64)?;
        let a = l.signal_expr();
        let b = r
            .signal
            .map(|s| p16(offset as i64 + i64::from(s)).map(super::security::neutral))
            .transpose()?
            .unwrap_or(PExpr::Lit(QValue::Bool(true)));
        exprs.push(PExpr::And(vec![a, b]));
        Some(pos)
    } else {
        None
    };
    let joined = RelL::Join {
        kind: JoinKind::Left,
        l: l.take(),
        r: r.take(),
        on,
        eq,
        by_id: false,
    };
    Ok(Row {
        width: exprs.len(),
        signal,
        rel: RelL::Project {
            input: Box::new(joined),
            exprs,
        },
        rep: None,
        keys: None,
    })
}
fn join(l: Row, r: Row, width: usize, pairs: Vec<(PExpr, usize)>) -> R<Row> {
    if data_width(&l) != width {
        return internal("ScheduleData join width");
    }
    let offset = l.width;
    let eq = pairs
        .iter()
        .map(|(l, i)| Ok((l.clone(), col(*i)?)))
        .collect::<R<_>>()?;
    let on = PExpr::And(
        pairs
            .into_iter()
            .map(|(l, i)| Ok(compare(CmpOp::Eq, l, col(offset + i)?)))
            .collect::<R<_>>()?,
    );
    combine(l, r, on, eq)
}
fn sum(mut input: Row, keys: Vec<PExpr>, arg: PExpr, ty: &QType, arg_type: &QType) -> R<Row> {
    let mut aggs = vec![AggL {
        f: Func::Sum,
        distinct: false,
        arg: Some(arg),
        ty: ty.clone(),
        arg_dec: super::dec_of(arg_type, None),
    }];
    let signal = input
        .signal
        .map(|s| {
            aggs.push(super::security::min(s));
            p16((keys.len() + 1) as i64)
        })
        .transpose()?;
    Ok(Row {
        width: keys.len() + aggs.len(),
        signal,
        rel: RelL::Aggregate {
            input: input.take(),
            keys,
            sets: None,
            aggs,
        },
        rep: None,
        keys: None,
    })
}
fn dedup(mut row: Row) -> R<Row> {
    if let Some(signal) = row.signal {
        let width = data_width(&row);
        row.rel = RelL::Aggregate {
            input: row.take(),
            keys: (0..width).map(col).collect::<R<_>>()?,
            sets: None,
            aggs: vec![super::security::min(signal)],
        };
        row.signal = Some(p16(width as i64)?);
    } else {
        row.rel = RelL::Distinct(row.take());
    }
    Ok(row)
}

impl Inputs {
    fn interval(&self, main: &Row, stored: &[FieldId], family: usize) -> R<Row> {
        let (start, end) = match family {
            0 => {
                let start = position(
                    stored,
                    standard_field(self, stored, StdField::ActionPeriod)?,
                )?;
                (start.clone(), month_end(start))
            }
            2 => (
                position(stored, standard_field(self, stored, StdField::BpDateFrom)?)?,
                position(stored, standard_field(self, stored, StdField::BpDateTill)?)?,
            ),
            3 => {
                let start = position(stored, standard_field(self, stored, StdField::Period)?)?;
                (start.clone(), month_end(start))
            }
            _ => return internal("ScheduleData interval family"),
        };
        let mut exprs = vec![start, end];
        for (link, _) in &self.links {
            exprs.push(position(stored, *link)?);
        }
        project(copy(main), exprs)
    }
    fn interval_sum(&self, interval: Row, graph: Row, ty: &QType, inclusive: bool) -> R<Row> {
        let interval = dedup(interval)?;
        let offset = interval.width;
        let width = 2 + self.links.len();
        let mut on = vec![
            compare(CmpOp::Ge, col(offset)?, col(0)?),
            compare(
                if inclusive { CmpOp::Le } else { CmpOp::Lt },
                col(offset)?,
                col(1)?,
            ),
        ];
        let mut eq = Vec::new();
        for i in 0..self.links.len() {
            on.push(compare(CmpOp::Eq, col(2 + i)?, col(offset + 2 + i)?));
            eq.push((col(2 + i)?, col(2 + i)?));
        }
        let mut filtered = combine(interval, graph, PExpr::And(on), eq)?;
        filtered.rel = RelL::Filter {
            input: filtered.take(),
            pred: not_null(col(width)?),
        };
        sum(
            filtered,
            (0..width).map(col).collect::<R<_>>()?,
            col(width + 1)?,
            ty,
            &self.resource_type,
        )
    }
}

const MAIN_STD: [StdField; 8] = [
    StdField::Recorder,
    StdField::LineNo,
    StdField::Period,
    StdField::ActionPeriod,
    StdField::ApDateFrom,
    StdField::ApDateTill,
    StdField::BpDateFrom,
    StdField::BpDateTill,
];
fn standard_field(inputs: &Inputs, stored: &[FieldId], field: StdField) -> R<FieldId> {
    let index = MAIN_STD
        .iter()
        .position(|f| *f == field)
        .ok_or_else(|| missing("unknown standard role"))?;
    if index >= 6 && !inputs.base {
        return Err(missing("base period is unavailable"));
    }
    stored
        .get(index)
        .copied()
        .ok_or_else(|| missing("hidden field is unavailable"))
}

impl Lower<'_> {
    pub(super) fn schedule_data(&mut self, v: &RegisterVtS) -> R<Row> {
        self.calculation_consumer(v)?;
        let inputs = schedule_inputs(self.schema, self.storage, self.map, v.table)?;
        let vt = self.schema.table(v.table);
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
        let required = (0..vt.fields.len())
            .map(|i| p16(i as i64).map(QFieldId))
            .collect::<R<Vec<_>>>()?
            .into_iter()
            .filter(|f| needed.contains(f))
            .collect::<Vec<_>>();
        self.slice_cols.insert(v.src, required.clone());
        self.slice_cols.insert(v.inner, required.clone());
        let mut stored = MAIN_STD
            .iter()
            .take(if inputs.base { 8 } else { 6 })
            .map(|f| standard(self.map, inputs.main, *f))
            .collect::<R<Vec<_>>>()?;
        for (f, _) in &inputs.links {
            if !stored.contains(f) {
                stored.push(*f);
            }
        }
        let mut plans = Vec::new();
        let mut families = Vec::new();
        let resource_md = self.schema.schedule(v.table).and_then(|f| f.resource);
        let resource_name = vt
            .fields
            .iter()
            .find(|f| f.role == FieldRole::Virtual && f.md == resource_md)
            .and_then(|f| f.name_ru.strip_suffix("ПериодДействия"))
            .ok_or_else(|| missing("derived schedule fields are absent"))?;
        let suffixes = [
            "ПериодДействия",
            "ФактическийПериодДействия",
            "БазовыйПериод",
            "ПериодРегистрации",
        ];
        let sum_type = vt
            .fields
            .iter()
            .find(|f| f.name_ru == format!("{resource_name}ПериодДействия"))
            .ok_or_else(|| missing("schedule sum type is absent"))?
            .ty
            .clone();
        for f in &required {
            let info = vt
                .fields
                .get(usize::from(f.0))
                .ok_or_else(|| missing("schema field is absent"))?;
            let family = if info.role == FieldRole::Virtual && info.md == resource_md {
                suffixes
                    .iter()
                    .position(|s| info.name_ru == format!("{resource_name}{s}"))
            } else {
                None
            };
            families.push(family);
            if family.is_none() {
                let plan = self.plan(v.table, *f)?;
                let mut fields = Vec::new();
                plan.needs(&mut fields);
                for (t, f) in fields {
                    if t != inputs.main {
                        return internal("ScheduleData field outside main");
                    }
                    if !stored.contains(&f) {
                        stored.push(f);
                    }
                }
                plans.push(Some(plan));
            } else {
                plans.push(None);
            }
        }
        let main = self.calculation_scan(v, false, inputs.main, &mut stored)?;
        let mut graph_fields = vec![inputs.date, inputs.resource];
        graph_fields.extend(inputs.links.iter().map(|(_, f)| *f));
        let graph = self.calculation_scan(v, true, inputs.graph, &mut graph_fields)?;
        let secured = main.rep.is_some() || graph.rep.is_some();
        let act_fields = [
            StdField::Recorder,
            StdField::LineNo,
            StdField::ApDateFrom,
            StdField::ApDateTill,
        ]
        .iter()
        .map(|f| standard(self.map, inputs.act, *f))
        .collect::<R<Vec<_>>>()?;
        let joined = join(
            copy(&main),
            Row {
                width: act_fields.len(),
                signal: None,
                rep: None,
                keys: None,
                rel: RelL::Scan {
                    table: inputs.act,
                    fields: act_fields,
                },
            },
            stored.len(),
            vec![(col(0)?, 0), (col(1)?, 1)],
        )?;
        let mut actual = vec![col(0)?, col(1)?];
        for (original, fragment) in [(4, 2), (5, 3)] {
            actual.push(PExpr::Case {
                operand: None,
                whens: vec![(
                    PExpr::IsNull {
                        l: Box::new(col(stored.len() + fragment)?),
                        not: false,
                    },
                    col(original)?,
                )],
                else_: Some(Box::new(col(stored.len() + fragment)?)),
            });
        }
        for (f, _) in &inputs.links {
            actual.push(position(&stored, *f)?);
        }
        let actual = project(joined, actual)?;
        let actual_width = 4 + inputs.links.len();
        let interval_width = 2 + inputs.links.len();
        let actual_sum = inputs.interval_sum(
            project(copy(&actual), (2..actual_width).map(col).collect::<R<_>>()?)?,
            copy(&graph),
            &sum_type,
            true,
        )?;
        let actual_join = join(
            actual,
            actual_sum,
            actual_width,
            (0..interval_width)
                .map(|i| Ok((col(i + 2)?, i)))
                .collect::<R<_>>()?,
        )?;
        let actual_summary = sum(
            actual_join,
            vec![col(0)?, col(1)?],
            col(actual_width + interval_width)?,
            &sum_type,
            &sum_type,
        )?;
        let mut rel = copy(&main);
        let mut width = stored.len();
        let mut outputs = [None; 4];
        for (family, out) in outputs.iter_mut().enumerate() {
            if secured && !families.contains(&Some(family)) {
                continue;
            }
            if family == 2 && !inputs.base {
                continue;
            }
            if family == 1 {
                rel = join(
                    rel,
                    copy(&actual_summary),
                    width,
                    vec![(col(0)?, 0), (col(1)?, 1)],
                )?;
                *out = Some(width + 2);
                width += 3;
            } else {
                let interval = inputs.interval(&main, &stored, family)?;
                let summary =
                    inputs.interval_sum(interval, copy(&graph), &sum_type, family == 2)?;
                let projected = inputs.interval(&main, &stored, family)?;
                let RelL::Project { mut exprs, .. } = projected.rel else {
                    return internal("ScheduleData interval projection");
                };
                exprs.truncate(interval_width);
                rel = join(
                    rel,
                    summary,
                    width,
                    exprs.into_iter().enumerate().map(|(i, e)| (e, i)).collect(),
                )?;
                *out = Some(width + interval_width);
                width += interval_width + 1;
            }
        }
        let mut projection = Vec::new();
        for (family, plan) in families.iter().zip(plans) {
            projection.push(if let Some(family) = family {
                col(outputs
                    .get(*family)
                    .copied()
                    .flatten()
                    .ok_or_else(|| missing("sum family is unavailable"))?)?
            } else {
                plan_expr(
                    plan.as_deref()
                        .ok_or_else(|| missing("field plan is absent"))?,
                    &mut |(_, f)| position(&stored, f),
                )?
            });
        }
        self.added = self.added.saturating_add(48);
        let mut row = project(rel, projection)?;
        row.rep = Some(v.inner);
        self.set_level(v.inner);
        self.inner += 1;
        let result = (|| {
            if let Some(condition) = &v.cond {
                row = self.filter_row(row, condition)?;
            }
            let mut exprs = (0..required.len()).map(col).collect::<R<Vec<_>>>()?;
            let signal = row
                .signal
                .map(|_| {
                    let index = exprs.len();
                    exprs.push(row.signal_expr());
                    p16(index as i64)
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
