use super::deref::plan_expr;
use super::layout::{R, Row, internal, p16};
use super::rel::Lower;
use super::{AggL, ArithDec, Dec, Opd, PExpr, RelL};
use crate::ir::RegisterVtS;
use crate::schema::calculation_base::BaseDependence;
use crate::schema::{FieldRole, QFieldId, QTableId, QuerySchema, StorageBinding};
use crate::{ExecError, QType, QValue};
use ibschema::names::{StdField, role};
use ibschema::storage::{FieldId, StorageMap, TableId};
use query_syntax::ast::{BinOp, CmpOp, JoinKind};
use query_syntax::{Func, Kw};
use std::collections::HashSet;

pub(crate) struct BaseScheduleInputs {
    pub graph: TableId,
    pub date: FieldId,
    pub resource: FieldId,
    pub links: Vec<(FieldId, FieldId)>,
}

pub(crate) struct BaseInputs {
    pub main: TableId,
    pub base: TableId,
    pub base_act: Option<TableId>,
    pub chart: TableId,
    pub chart_id: FieldId,
    pub chart_base_as_act: Option<FieldId>,
    pub base_section: TableId,
    pub section_owner: FieldId,
    pub section_base_kind: FieldId,
    pub dependence: BaseDependence,
    pub owner_action_period: bool,
    pub owner_base_period: bool,
    pub schedule: Option<BaseScheduleInputs>,
}

impl BaseInputs {
    pub(crate) fn tables(&self) -> Vec<TableId> {
        let mut out = Vec::with_capacity(6);
        let mut push = |id: TableId| {
            if !out.contains(&id) {
                out.push(id);
            }
        };
        push(self.main);
        push(self.base);
        if let Some(act) = self.base_act {
            push(act);
        }
        push(self.chart);
        push(self.base_section);
        if let Some(sched) = &self.schedule {
            push(sched.graph);
        }
        out
    }
}

fn missing(what: &str) -> ExecError {
    ExecError::Fatal(format!("CalculationBase: {what}"))
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

pub(crate) fn base_inputs(
    schema: &QuerySchema,
    storage: &StorageBinding,
    map: &StorageMap,
    table: QTableId,
) -> R<BaseInputs> {
    let source = schema.table(table);
    let facts = schema
        .calculation_base(table)
        .ok_or_else(|| missing("metadata facts are absent"))?;
    let Some(dependence) = facts.dependence else {
        return Err(ExecError::Later {
            what: "Q6d-CalculationBaseMetadata",
            stage: "Q6d",
        });
    };
    if !facts.owner_base_period && !facts.owner_action_period {
        return Err(ExecError::Later {
            what: "Q6d-CalculationBaseMetadata",
            stage: "Q6d",
        });
    }
    if dependence == BaseDependence::OnActionPeriod {
        if !facts.owner_periodicity_month || !facts.base_periodicity_month {
            return Err(ExecError::Later {
                what: "Q6d-CalculationBasePeriodicity",
                stage: "Q6d",
            });
        }
        if !facts.base_action_period {
            return Err(ExecError::Later {
                what: "Q6d-CalculationBaseMetadata",
                stage: "Q6d",
            });
        }
    }
    let main = storage
        .table(table)
        .ok_or_else(|| missing("main storage is not bound"))?;
    if storage.table(facts.owner) != Some(main) {
        return internal("CalculationBase main binding differs from its register");
    }
    let base = storage
        .table(facts.base)
        .ok_or_else(|| missing("base register storage is not bound"))?;
    let chart_q = facts
        .chart
        .ok_or_else(|| missing("chart of calculation types metadata is absent"))?;
    let chart = storage
        .table(chart_q)
        .ok_or_else(|| missing("chart of calculation types storage is not bound"))?;
    let base_section = match facts.base_section {
        Some(section_q) => {
            if schema.is_unstored(section_q) {
                return Err(ExecError::Later {
                    what: "Q6d-CalculationBaseMetadata",
                    stage: "Q6d",
                });
            }
            storage
                .table(section_q)
                .ok_or_else(|| missing("BaseCalculationTypes storage is not bound"))?
        }
        None => {
            let chart_md = schema
                .table(chart_q)
                .object
                .ok_or_else(|| missing("chart of calculation types uuid is absent"))?;
            map.object(chart_md)
                .and_then(|o| o.list(ibschema::storage::StdList::BaseCalculationTypes))
                .ok_or_else(|| missing("BaseCalculationTypes storage is not bound"))?
        }
    };

    for (index, field) in source.fields.iter().enumerate() {
        if field.role != FieldRole::Virtual
            && storage.field(table, QFieldId(p16(index as i64)?)).is_none()
        {
            return Err(missing("main field is not bound"));
        }
    }

    for f in [
        StdField::Recorder,
        StdField::LineNo,
        StdField::Period,
        StdField::CalcKind,
    ] {
        standard(map, main, f)?;
    }
    if facts.owner_base_period {
        for f in [StdField::BpDateFrom, StdField::BpDateTill] {
            standard(map, main, f)?;
        }
    }
    if facts.owner_action_period {
        for f in [StdField::ApDateFrom, StdField::ApDateTill] {
            standard(map, main, f)?;
        }
    }
    for f in [
        StdField::Recorder,
        StdField::LineNo,
        StdField::Period,
        StdField::CalcKind,
        StdField::Active,
    ] {
        standard(map, base, f)?;
    }

    let chart_id = standard(map, chart, StdField::Id)?;
    let chart_base_as_act = map
        .try_table(chart)
        .and_then(|t| t.field_by_std(StdField::BaseAsAct));
    let section_owner = standard(map, base_section, StdField::Id)?;
    let section_base_kind = standard(map, base_section, StdField::BaseCkBaseCk)?;

    let mut base_act = None;
    let mut schedule = None;
    if dependence == BaseDependence::OnActionPeriod {
        for f in [
            StdField::ActionPeriod,
            StdField::ApDateFrom,
            StdField::ApDateTill,
        ] {
            standard(map, base, f)?;
        }
        let base_object = schema
            .table(facts.base)
            .object
            .ok_or_else(|| missing("base register UUID is absent"))?;
        let act = map
            .object(base_object)
            .and_then(|o| o.service.iter().find(|(r, _)| r == role::ACTION_PERIODS))
            .map(|(_, id)| *id)
            .ok_or_else(|| missing("base ActionPeriods storage is not bound"))?;
        for f in [
            StdField::Recorder,
            StdField::LineNo,
            StdField::ApDateFrom,
            StdField::ApDateTill,
        ] {
            standard(map, act, f)?;
        }
        base_act = Some(act);

        if let Some(sched_facts) = schema.schedule_of_register(facts.base)
            && let Some(graph_q) = sched_facts.graph
        {
            if let Some(what) = sched_facts.error {
                return Err(ExecError::Later { what, stage: "Q6c" });
            }
            let graph = storage
                .table(graph_q)
                .ok_or_else(|| missing("base schedule storage is not bound"))?;
            let date = metadata(
                map,
                graph,
                sched_facts
                    .date
                    .ok_or_else(|| missing("base schedule date is absent"))?,
            )?;
            let res_md = sched_facts
                .resource
                .ok_or_else(|| missing("base schedule resource is absent"))?;
            let resource = metadata(map, graph, res_md)?;
            let links = sched_facts
                .links
                .iter()
                .map(|(a, b)| Ok((metadata(map, base, *a)?, metadata(map, graph, *b)?)))
                .collect::<R<_>>()?;
            schedule = Some(BaseScheduleInputs {
                graph,
                date,
                resource,
                links,
            });
        }
    }

    Ok(BaseInputs {
        main,
        base,
        base_act,
        chart,
        chart_id,
        chart_base_as_act,
        base_section,
        section_owner,
        section_base_kind,
        dependence,
        owner_action_period: facts.owner_action_period,
        owner_base_period: facts.owner_base_period,
        schedule,
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

fn pos_idx(fields: &[FieldId], f: FieldId) -> R<usize> {
    fields
        .iter()
        .position(|x| *x == f)
        .ok_or_else(|| missing("scan field is absent"))
}

fn compare(op: CmpOp, a: PExpr, b: PExpr) -> PExpr {
    PExpr::Compare(op, Box::new(a), Box::new(b))
}

fn is_null(a: PExpr, not: bool) -> PExpr {
    PExpr::IsNull {
        l: Box::new(a),
        not,
    }
}

fn date_add(a: PExpr, unit: Kw, amount: i64) -> PExpr {
    PExpr::Func {
        f: Func::DateAdd,
        args: vec![
            a,
            PExpr::Unit(unit),
            PExpr::Lit(QValue::Number(bsl_prim::Number::from_i64(amount))),
        ],
        ty: QType::date(crate::types::DateQ::DateTime),
        arg_dec: Dec::UNKNOWN,
    }
}

fn month_end(a: PExpr) -> PExpr {
    date_add(a, Kw::Month, 1)
}

fn prev_day(a: PExpr) -> PExpr {
    date_add(a, Kw::Day, -1)
}

fn num_any() -> QType {
    QType::num(crate::types::NumQ {
        digits: 0,
        scale: 0,
        nonneg: false,
    })
}

fn zero_num() -> PExpr {
    PExpr::Lit(QValue::Number(bsl_prim::Number::from_i64(0)))
}

fn is_partial_pred(slave_ap: PExpr, bp_from: PExpr, bp_till: PExpr) -> PExpr {
    let next_m = month_end(slave_ap.clone());
    let last_day = prev_day(next_m.clone());
    PExpr::Or(vec![
        PExpr::And(vec![
            compare(CmpOp::Gt, next_m, bp_from.clone()),
            compare(CmpOp::Lt, slave_ap.clone(), bp_from),
        ]),
        PExpr::And(vec![
            compare(CmpOp::Le, slave_ap, bp_till.clone()),
            compare(CmpOp::Gt, last_day, bp_till),
        ]),
    ])
}

fn project_rel(input: RelL, exprs: Vec<PExpr>) -> RelL {
    RelL::Project {
        input: Box::new(input),
        exprs,
    }
}

fn join_rel(
    kind: JoinKind,
    l: RelL,
    r: RelL,
    on: PExpr,
    eq: Vec<(PExpr, PExpr)>,
    by_id: bool,
) -> RelL {
    RelL::Join {
        kind,
        l: Box::new(l),
        r: Box::new(r),
        on,
        eq,
        by_id,
    }
}

fn schedule_interval_sum(intervals: RelL, sched: &BaseScheduleInputs, links_len: usize) -> R<RelL> {
    let key_width = 2 + links_len;
    let distinct = RelL::Distinct(Box::new(intervals));
    let mut graph_fields = Vec::with_capacity(2 + links_len);
    graph_fields.push(sched.date);
    graph_fields.push(sched.resource);
    for (_, g) in &sched.links {
        graph_fields.push(*g);
    }
    let graph_scan = RelL::Scan {
        table: sched.graph,
        fields: graph_fields,
    };
    let mut on = vec![
        compare(CmpOp::Ge, col(key_width)?, col(0)?),
        compare(CmpOp::Le, col(key_width)?, col(1)?),
    ];
    let mut eq = Vec::with_capacity(links_len);
    for i in 0..links_len {
        on.push(compare(CmpOp::Eq, col(2 + i)?, col(key_width + 2 + i)?));
        eq.push((col(2 + i)?, col(2 + i)?));
    }
    let joined = join_rel(
        JoinKind::Left,
        distinct,
        graph_scan,
        PExpr::And(on),
        eq,
        false,
    );
    let val_expr = PExpr::Case {
        operand: None,
        whens: vec![(is_null(col(key_width + 1)?, false), zero_num())],
        else_: Some(Box::new(col(key_width + 1)?)),
    };
    let keys = (0..key_width).map(col).collect::<R<Vec<_>>>()?;
    Ok(RelL::Aggregate {
        input: Box::new(joined),
        keys,
        sets: None,
        aggs: vec![AggL {
            f: Func::Sum,
            distinct: false,
            arg: Some(val_expr),
            ty: num_any(),
            arg_dec: Dec::UNKNOWN,
        }],
    })
}

fn push_unique(list: &mut Vec<FieldId>, field: FieldId) {
    if !list.contains(&field) {
        list.push(field);
    }
}

impl Lower<'_> {
    pub(super) fn calculation_base(&mut self, v: &RegisterVtS) -> R<Row> {
        let Some(spec) = v.base.as_ref() else {
            return Err(ExecError::Later {
                what: "Q6d-CalculationBaseArguments",
                stage: "Q6d",
            });
        };
        let facts = self
            .schema
            .calculation_base(v.table)
            .ok_or_else(|| missing("metadata facts are absent"))?
            .clone();
        let inputs = base_inputs(self.schema, self.storage, self.map, v.table)?;
        let dim_pairs = self
            .b
            .base_dims
            .get(&v.src)
            .ok_or_else(|| missing("bound dimension pairs are absent"))?
            .clone();

        let vt = self.schema.table(v.table);
        let mut needed = HashSet::new();
        for src in [v.src, v.inner] {
            needed.extend(
                self.vt_used
                    .get(src.0 as usize)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
        }
        let required: Vec<QFieldId> = (0..vt.fields.len())
            .map(|i| p16(i as i64).map(QFieldId))
            .collect::<R<Vec<_>>>()?
            .into_iter()
            .filter(|f| needed.contains(f))
            .collect();
        let breakdown = spec.breakdown.clone();
        self.slice_cols.insert(v.src, required.clone());
        self.slice_cols.insert(v.inner, required.clone());
        self.slice_extra.insert(v.src, required.len());
        self.slice_extra.insert(v.inner, required.len());

        let main_rec = standard(self.map, inputs.main, StdField::Recorder)?;
        let main_line = standard(self.map, inputs.main, StdField::LineNo)?;
        let main_period = standard(self.map, inputs.main, StdField::Period)?;
        let main_kind = standard(self.map, inputs.main, StdField::CalcKind)?;
        let mut main_stored = vec![main_rec, main_line, main_period, main_kind];
        let main_bp = if inputs.owner_base_period {
            let from = standard(self.map, inputs.main, StdField::BpDateFrom)?;
            let till = standard(self.map, inputs.main, StdField::BpDateTill)?;
            push_unique(&mut main_stored, from);
            push_unique(&mut main_stored, till);
            Some((from, till))
        } else {
            None
        };
        let main_ap = if inputs.owner_action_period {
            let from = standard(self.map, inputs.main, StdField::ApDateFrom)?;
            let till = standard(self.map, inputs.main, StdField::ApDateTill)?;
            push_unique(&mut main_stored, from);
            push_unique(&mut main_stored, till);
            Some((from, till))
        } else {
            None
        };
        let mut main_dim_fields = Vec::with_capacity(dim_pairs.len());
        let mut base_dim_fields = Vec::with_capacity(dim_pairs.len());
        for &(mf, bf) in &dim_pairs {
            let m_phys = self
                .storage
                .field(facts.owner, mf)
                .ok_or_else(|| missing("main dimension field is not bound"))?;
            let b_phys = self
                .storage
                .field(facts.base, bf)
                .ok_or_else(|| missing("base dimension field is not bound"))?;
            push_unique(&mut main_stored, m_phys);
            main_dim_fields.push(m_phys);
            base_dim_fields.push(b_phys);
        }

        let mut main_plans = Vec::with_capacity(required.len());
        let mut res_slots: Vec<Option<usize>> = Vec::with_capacity(required.len());
        let mut base_resources: Vec<(FieldId, QType)> = Vec::new();
        for f in &required {
            if let Some((_, base_field, _)) = facts.resources.iter().find(|(vf, _, _)| vf == f) {
                let b_phys = self
                    .storage
                    .field(facts.base, *base_field)
                    .ok_or_else(|| missing("base resource field is not bound"))?;
                let sum_ty = vt
                    .fields
                    .get(usize::from(f.0))
                    .ok_or_else(|| missing("schema base resource field is absent"))?
                    .ty
                    .clone();
                let idx = base_resources.len();
                base_resources.push((b_phys, sum_ty));
                res_slots.push(Some(idx));
                main_plans.push(None);
            } else {
                let plan = self.plan(v.table, *f)?;
                let mut fields = Vec::new();
                plan.needs(&mut fields);
                for (t, field) in fields {
                    if t != inputs.main {
                        return internal("CalculationBase main field outside main input");
                    }
                    push_unique(&mut main_stored, field);
                }
                res_slots.push(None);
                main_plans.push(Some(plan));
            }
        }

        let main_full_scan = RelL::Scan {
            table: inputs.main,
            fields: main_stored.clone(),
        };

        let dim_count = dim_pairs.len();
        let main_keys_width = 6 + dim_count;
        let main_keys = if let (Some(base_as_act), Some((bp_f, bp_t)), Some((ap_f, ap_t))) =
            (inputs.chart_base_as_act, main_bp, main_ap)
        {
            let chart_scan = RelL::Scan {
                table: inputs.chart,
                fields: vec![inputs.chart_id, base_as_act],
            };
            let main_w = main_stored.len();
            let joined = join_rel(
                JoinKind::Left,
                main_full_scan.clone(),
                chart_scan,
                compare(CmpOp::Eq, col(3)?, col(main_w)?),
                vec![(col(3)?, col(0)?)],
                true,
            );
            let is_act = compare(CmpOp::Eq, col(main_w + 1)?, PExpr::Lit(QValue::Bool(true)));
            let bp_from = PExpr::Case {
                operand: None,
                whens: vec![(is_act.clone(), position(&main_stored, ap_f)?)],
                else_: Some(Box::new(position(&main_stored, bp_f)?)),
            };
            let bp_till = PExpr::Case {
                operand: None,
                whens: vec![(is_act, position(&main_stored, ap_t)?)],
                else_: Some(Box::new(position(&main_stored, bp_t)?)),
            };
            let mut exprs = vec![col(0)?, col(1)?, col(2)?, col(3)?, bp_from, bp_till];
            for &d in &main_dim_fields {
                exprs.push(position(&main_stored, d)?);
            }
            project_rel(joined, exprs)
        } else {
            let (bp_f, bp_t) = main_bp
                .or(main_ap)
                .ok_or_else(|| missing("main period fields are absent"))?;
            let mut exprs = vec![
                col(0)?,
                col(1)?,
                col(2)?,
                col(3)?,
                position(&main_stored, bp_f)?,
                position(&main_stored, bp_t)?,
            ];
            for &d in &main_dim_fields {
                exprs.push(position(&main_stored, d)?);
            }
            project_rel(main_full_scan.clone(), exprs)
        };

        let base_ck = RelL::Distinct(Box::new(RelL::Scan {
            table: inputs.base_section,
            fields: vec![inputs.section_owner, inputs.section_base_kind],
        }));
        let main_ck_joined = join_rel(
            JoinKind::Inner,
            main_keys,
            base_ck,
            compare(CmpOp::Eq, col(3)?, col(main_keys_width)?),
            vec![(col(3)?, col(0)?)],
            false,
        );
        let mut main_ck_proj = vec![
            col(0)?,
            col(1)?,
            col(2)?,
            col(main_keys_width + 1)?,
            col(4)?,
            col(5)?,
        ];
        for i in 0..dim_count {
            main_ck_proj.push(col(6 + i)?);
        }
        let main_with_base_ck = project_rel(main_ck_joined, main_ck_proj);

        let slave_rec = standard(self.map, inputs.base, StdField::Recorder)?;
        let slave_line = standard(self.map, inputs.base, StdField::LineNo)?;
        let slave_period = standard(self.map, inputs.base, StdField::Period)?;
        let slave_kind = standard(self.map, inputs.base, StdField::CalcKind)?;
        let slave_active_f = standard(self.map, inputs.base, StdField::Active)?;
        let mut slave_stored = vec![
            slave_rec,
            slave_line,
            slave_period,
            slave_kind,
            slave_active_f,
        ];
        let slave_ap_fields = if inputs.dependence == BaseDependence::OnActionPeriod {
            let ap = standard(self.map, inputs.base, StdField::ActionPeriod)?;
            let from = standard(self.map, inputs.base, StdField::ApDateFrom)?;
            let till = standard(self.map, inputs.base, StdField::ApDateTill)?;
            push_unique(&mut slave_stored, ap);
            push_unique(&mut slave_stored, from);
            push_unique(&mut slave_stored, till);
            Some((ap, from, till))
        } else {
            None
        };
        for &d in &base_dim_fields {
            push_unique(&mut slave_stored, d);
        }
        for &(r, _) in &base_resources {
            push_unique(&mut slave_stored, r);
        }
        let mut breakdown_phys = Vec::with_capacity(breakdown.len());
        for &bf in &breakdown {
            let phys = self
                .storage
                .field(facts.base, bf)
                .ok_or_else(|| missing("breakdown field is not bound"))?;
            push_unique(&mut slave_stored, phys);
            breakdown_phys.push(phys);
        }
        if let Some(sched) = &inputs.schedule {
            for &(b_link, _) in &sched.links {
                push_unique(&mut slave_stored, b_link);
            }
        }
        let slave_active = RelL::Filter {
            input: Box::new(RelL::Scan {
                table: inputs.base,
                fields: slave_stored.clone(),
            }),
            pred: compare(CmpOp::Eq, col(4)?, PExpr::Lit(QValue::Bool(true))),
        };
        let slave_w = slave_stored.len();

        let brk_count = breakdown_phys.len();
        let res_count = base_resources.len();

        let contributions = match inputs.dependence {
            BaseDependence::OnRegistrationPeriod => {
                let mut eq = vec![(col(3)?, col(3)?)];
                let mut on = vec![
                    compare(CmpOp::Eq, col(3)?, col(main_keys_width + 3)?),
                    compare(CmpOp::Ge, col(main_keys_width + 2)?, col(4)?),
                    compare(CmpOp::Le, col(main_keys_width + 2)?, col(5)?),
                ];
                for (k, &bd) in base_dim_fields.iter().enumerate() {
                    let b_idx = pos_idx(&slave_stored, bd)?;
                    eq.push((col(6 + k)?, col(b_idx)?));
                    on.push(compare(
                        CmpOp::Eq,
                        col(6 + k)?,
                        col(main_keys_width + b_idx)?,
                    ));
                }
                let joined = join_rel(
                    JoinKind::Inner,
                    main_with_base_ck,
                    slave_active,
                    PExpr::And(on),
                    eq,
                    false,
                );
                let mut proj = vec![col(0)?, col(1)?];
                for &bp in &breakdown_phys {
                    proj.push(col(main_keys_width + pos_idx(&slave_stored, bp)?)?);
                }
                for &(rf, _) in &base_resources {
                    proj.push(col(main_keys_width + pos_idx(&slave_stored, rf)?)?);
                }
                project_rel(joined, proj)
            }
            BaseDependence::OnActionPeriod => {
                let (ap_f, ap_from_f, ap_till_f) = slave_ap_fields
                    .ok_or_else(|| missing("base action period fields are absent"))?;
                let ap_idx = pos_idx(&slave_stored, ap_f)?;
                let ap_from_idx = pos_idx(&slave_stored, ap_from_f)?;
                let ap_till_idx = pos_idx(&slave_stored, ap_till_f)?;

                type JoinConds = (Vec<(PExpr, PExpr)>, Vec<PExpr>);
                let build_common = |slave_offset: usize,
                                    dim_indices: &[usize],
                                    slave_period_col: usize,
                                    slave_kind_col: usize,
                                    slave_ap_col: usize|
                 -> R<JoinConds> {
                    let mut eq = vec![(col(3)?, col(slave_kind_col)?)];
                    let mut on = vec![
                        compare(CmpOp::Eq, col(3)?, col(slave_offset + slave_kind_col)?),
                        PExpr::Or(vec![
                            compare(
                                CmpOp::Lt,
                                col(slave_offset + slave_period_col)?,
                                month_end(col(2)?),
                            ),
                            compare(CmpOp::Le, col(slave_offset + slave_period_col)?, col(5)?),
                        ]),
                        compare(
                            CmpOp::Gt,
                            month_end(col(slave_offset + slave_ap_col)?),
                            col(4)?,
                        ),
                        compare(CmpOp::Le, col(slave_offset + slave_ap_col)?, col(5)?),
                    ];
                    for (k, &b_idx) in dim_indices.iter().enumerate() {
                        eq.push((col(6 + k)?, col(b_idx)?));
                        on.push(compare(CmpOp::Eq, col(6 + k)?, col(slave_offset + b_idx)?));
                    }
                    Ok((eq, on))
                };

                let base_dim_indices = base_dim_fields
                    .iter()
                    .map(|f| pos_idx(&slave_stored, *f))
                    .collect::<R<Vec<_>>>()?;

                let (eq1, mut on1) =
                    build_common(main_keys_width, &base_dim_indices, 2, 3, ap_idx)?;
                on1.push(PExpr::Not(Box::new(is_partial_pred(
                    col(main_keys_width + ap_idx)?,
                    col(4)?,
                    col(5)?,
                ))));
                let full_join = join_rel(
                    JoinKind::Inner,
                    main_with_base_ck.clone(),
                    slave_active.clone(),
                    PExpr::And(on1),
                    eq1,
                    false,
                );
                let mut proj1 = vec![col(0)?, col(1)?];
                for &bp in &breakdown_phys {
                    proj1.push(col(main_keys_width + pos_idx(&slave_stored, bp)?)?);
                }
                for &(rf, _) in &base_resources {
                    proj1.push(col(main_keys_width + pos_idx(&slave_stored, rf)?)?);
                }
                let branch1 = project_rel(full_join, proj1);

                let act_table = inputs
                    .base_act
                    .ok_or_else(|| missing("base ActionPeriods table is absent"))?;
                let act_fields = [
                    StdField::Recorder,
                    StdField::LineNo,
                    StdField::ApDateFrom,
                    StdField::ApDateTill,
                ]
                .iter()
                .map(|f| standard(self.map, act_table, *f))
                .collect::<R<Vec<_>>>()?;
                let act_scan = RelL::Scan {
                    table: act_table,
                    fields: act_fields,
                };
                let slave_act_join = join_rel(
                    JoinKind::Left,
                    slave_active,
                    act_scan,
                    PExpr::And(vec![
                        compare(CmpOp::Eq, col(0)?, col(slave_w)?),
                        compare(CmpOp::Eq, col(1)?, col(slave_w + 1)?),
                    ]),
                    vec![(col(0)?, col(0)?), (col(1)?, col(1)?)],
                    false,
                );
                let mut frag_proj = (0..slave_w).map(col).collect::<R<Vec<_>>>()?;
                frag_proj.push(PExpr::Case {
                    operand: None,
                    whens: vec![(is_null(col(slave_w + 2)?, false), col(ap_from_idx)?)],
                    else_: Some(Box::new(col(slave_w + 2)?)),
                });
                frag_proj.push(PExpr::Case {
                    operand: None,
                    whens: vec![(is_null(col(slave_w + 3)?, false), col(ap_till_idx)?)],
                    else_: Some(Box::new(col(slave_w + 3)?)),
                });
                let slave_frags = project_rel(slave_act_join, frag_proj);
                let frag_df_col = slave_w;
                let frag_dt_col = slave_w + 1;
                let frag_w = slave_w + 2;

                let branch2 = if let Some(sched) = &inputs.schedule {
                    let links_len = sched.links.len();
                    let link_indices = sched
                        .links
                        .iter()
                        .map(|(b_link, _)| pos_idx(&slave_stored, *b_link))
                        .collect::<R<Vec<_>>>()?;

                    let mut frag_int_proj = vec![col(frag_df_col)?, col(frag_dt_col)?];
                    for &li in &link_indices {
                        frag_int_proj.push(col(li)?);
                    }
                    let frag_intervals = project_rel(slave_frags.clone(), frag_int_proj);
                    let frag_hours = schedule_interval_sum(frag_intervals, sched, links_len)?;
                    let key_w = 2 + links_len;
                    let mut fh_on = vec![
                        compare(CmpOp::Eq, col(frag_df_col)?, col(frag_w)?),
                        compare(CmpOp::Eq, col(frag_dt_col)?, col(frag_w + 1)?),
                    ];
                    let mut fh_eq =
                        vec![(col(frag_df_col)?, col(0)?), (col(frag_dt_col)?, col(1)?)];
                    for (i, &li) in link_indices.iter().enumerate() {
                        fh_on.push(compare(CmpOp::Eq, col(li)?, col(frag_w + 2 + i)?));
                        fh_eq.push((col(li)?, col(2 + i)?));
                    }
                    let frags_with_fh = join_rel(
                        JoinKind::Left,
                        slave_frags.clone(),
                        frag_hours,
                        PExpr::And(fh_on),
                        fh_eq,
                        false,
                    );
                    let calend_by_rec = RelL::Aggregate {
                        input: Box::new(frags_with_fh),
                        keys: vec![col(0)?, col(1)?],
                        sets: None,
                        aggs: vec![
                            AggL {
                                f: Func::Sum,
                                distinct: false,
                                arg: Some(col(frag_w + key_w)?),
                                ty: num_any(),
                                arg_dec: Dec::UNKNOWN,
                            },
                            AggL {
                                f: Func::Min,
                                distinct: false,
                                arg: Some(col(frag_df_col)?),
                                ty: QType::date(crate::types::DateQ::DateTime),
                                arg_dec: Dec::UNKNOWN,
                            },
                        ],
                    };
                    let frags_cal_join = join_rel(
                        JoinKind::Left,
                        slave_frags,
                        calend_by_rec,
                        PExpr::And(vec![
                            compare(CmpOp::Eq, col(0)?, col(frag_w)?),
                            compare(CmpOp::Eq, col(1)?, col(frag_w + 1)?),
                        ]),
                        vec![(col(0)?, col(0)?), (col(1)?, col(1)?)],
                        false,
                    );
                    let mut frags_cal_proj = (0..frag_w).map(col).collect::<R<Vec<_>>>()?;
                    frags_cal_proj.push(col(frag_w + 2)?);
                    frags_cal_proj.push(col(frag_w + 3)?);
                    let slave_frags_cal = project_rel(frags_cal_join, frags_cal_proj);
                    let calend_col = frag_w;
                    let df_min_col = frag_w + 1;

                    let (eq2, mut on2) =
                        build_common(main_keys_width, &base_dim_indices, 2, 3, ap_idx)?;
                    on2.push(is_partial_pred(
                        col(main_keys_width + ap_idx)?,
                        col(4)?,
                        col(5)?,
                    ));
                    on2.push(compare(
                        CmpOp::Le,
                        col(main_keys_width + frag_df_col)?,
                        col(5)?,
                    ));
                    on2.push(compare(
                        CmpOp::Ge,
                        col(main_keys_width + frag_dt_col)?,
                        col(4)?,
                    ));
                    let matched = join_rel(
                        JoinKind::Inner,
                        main_with_base_ck,
                        slave_frags_cal,
                        PExpr::And(on2),
                        eq2,
                        false,
                    );

                    let ov_from = PExpr::Case {
                        operand: None,
                        whens: vec![(
                            compare(CmpOp::Gt, col(4)?, col(main_keys_width + frag_df_col)?),
                            col(4)?,
                        )],
                        else_: Some(Box::new(col(main_keys_width + frag_df_col)?)),
                    };
                    let ov_till = PExpr::Case {
                        operand: None,
                        whens: vec![(
                            compare(CmpOp::Lt, col(5)?, col(main_keys_width + frag_dt_col)?),
                            col(5)?,
                        )],
                        else_: Some(Box::new(col(main_keys_width + frag_dt_col)?)),
                    };
                    let mut m_proj = vec![col(0)?, col(1)?, ov_from, ov_till];
                    for &li in &link_indices {
                        m_proj.push(col(main_keys_width + li)?);
                    }
                    m_proj.push(col(main_keys_width + calend_col)?);
                    m_proj.push(col(main_keys_width + df_min_col)?);
                    for &bp in &breakdown_phys {
                        m_proj.push(col(main_keys_width + pos_idx(&slave_stored, bp)?)?);
                    }
                    for &(rf, _) in &base_resources {
                        m_proj.push(col(main_keys_width + pos_idx(&slave_stored, rf)?)?);
                    }
                    let m_width = m_proj.len();
                    let matched_proj = project_rel(matched, m_proj);

                    let ov_int_proj = (2..4 + links_len).map(col).collect::<R<Vec<_>>>()?;
                    let ov_intervals = project_rel(matched_proj.clone(), ov_int_proj);
                    let ov_hours = schedule_interval_sum(ov_intervals, sched, links_len)?;
                    let mut ov_on = vec![
                        compare(CmpOp::Eq, col(2)?, col(m_width)?),
                        compare(CmpOp::Eq, col(3)?, col(m_width + 1)?),
                    ];
                    let mut ov_eq = vec![(col(2)?, col(0)?), (col(3)?, col(1)?)];
                    for i in 0..links_len {
                        ov_on.push(compare(CmpOp::Eq, col(4 + i)?, col(m_width + 2 + i)?));
                        ov_eq.push((col(4 + i)?, col(2 + i)?));
                    }
                    let with_ov = join_rel(
                        JoinKind::Left,
                        matched_proj,
                        ov_hours,
                        PExpr::And(ov_on),
                        ov_eq,
                        false,
                    );
                    let ov_hours_col = m_width + 2 + links_len;
                    let m_calend_col = 4 + links_len;
                    let m_df_min_col = 5 + links_len;
                    let m_brk_start = 6 + links_len;
                    let m_res_start = m_brk_start + brk_count;

                    let mut contrib_proj = vec![col(0)?, col(1)?];
                    for i in 0..brk_count {
                        contrib_proj.push(col(m_brk_start + i)?);
                    }
                    let unknown_step = ArithDec {
                        rhs: Dec::UNKNOWN,
                        out: Dec::UNKNOWN,
                        lk: Opd::Value,
                        rk: Opd::Value,
                    };
                    for i in 0..res_count {
                        let res_col = col(m_res_start + i)?;
                        let calend_e = col(m_calend_col)?;
                        let ov_h_e = PExpr::Case {
                            operand: None,
                            whens: vec![(is_null(col(ov_hours_col)?, false), zero_num())],
                            else_: Some(Box::new(col(ov_hours_col)?)),
                        };
                        let scaled = PExpr::Arith {
                            first: Box::new(res_col.clone()),
                            first_dec: Dec::UNKNOWN,
                            steps: vec![
                                (BinOp::Mul, ov_h_e, num_any()),
                                (BinOp::Div, calend_e.clone(), num_any()),
                            ],
                            decs: vec![unknown_step, unknown_step],
                        };
                        let zero_cal = PExpr::Case {
                            operand: None,
                            whens: vec![(
                                compare(CmpOp::Eq, col(m_df_min_col)?, col(2)?),
                                res_col.clone(),
                            )],
                            else_: Some(Box::new(zero_num())),
                        };
                        contrib_proj.push(PExpr::Case {
                            operand: None,
                            whens: vec![
                                (is_null(calend_e.clone(), false), res_col),
                                (compare(CmpOp::Ne, calend_e, zero_num()), scaled),
                            ],
                            else_: Some(Box::new(zero_cal)),
                        });
                    }
                    project_rel(with_ov, contrib_proj)
                } else {
                    let (eq2, mut on2) =
                        build_common(main_keys_width, &base_dim_indices, 2, 3, ap_idx)?;
                    on2.push(is_partial_pred(
                        col(main_keys_width + ap_idx)?,
                        col(4)?,
                        col(5)?,
                    ));
                    on2.push(compare(
                        CmpOp::Le,
                        col(main_keys_width + frag_df_col)?,
                        col(5)?,
                    ));
                    on2.push(compare(
                        CmpOp::Ge,
                        col(main_keys_width + frag_dt_col)?,
                        col(4)?,
                    ));
                    let matched = join_rel(
                        JoinKind::Inner,
                        main_with_base_ck,
                        slave_frags,
                        PExpr::And(on2),
                        eq2,
                        false,
                    );
                    let mut dedup_proj = vec![
                        col(0)?,
                        col(1)?,
                        col(main_keys_width)?,
                        col(main_keys_width + 1)?,
                    ];
                    for &bp in &breakdown_phys {
                        dedup_proj.push(col(main_keys_width + pos_idx(&slave_stored, bp)?)?);
                    }
                    for &(rf, _) in &base_resources {
                        dedup_proj.push(col(main_keys_width + pos_idx(&slave_stored, rf)?)?);
                    }
                    let deduped = RelL::Distinct(Box::new(project_rel(matched, dedup_proj)));
                    let mut out_proj = vec![col(0)?, col(1)?];
                    for i in 0..brk_count + res_count {
                        out_proj.push(col(4 + i)?);
                    }
                    project_rel(deduped, out_proj)
                };

                RelL::Union {
                    parts: vec![branch1, branch2],
                    all: vec![true],
                }
            }
        };

        let agg_keys = (0..2 + brk_count).map(col).collect::<R<Vec<_>>>()?;
        let mut aggs = Vec::with_capacity(res_count);
        for (i, (_, sum_ty)) in base_resources.iter().enumerate() {
            aggs.push(AggL {
                f: Func::Sum,
                distinct: false,
                arg: Some(col(2 + brk_count + i)?),
                ty: sum_ty.clone(),
                arg_dec: Dec::UNKNOWN,
            });
        }
        let aggregated = RelL::Aggregate {
            input: Box::new(contributions),
            keys: agg_keys,
            sets: None,
            aggs,
        };

        let main_w = main_stored.len();
        let final_join = join_rel(
            JoinKind::Inner,
            main_full_scan,
            aggregated,
            PExpr::And(vec![
                compare(CmpOp::Eq, col(0)?, col(main_w)?),
                compare(CmpOp::Eq, col(1)?, col(main_w + 1)?),
            ]),
            vec![(col(0)?, col(0)?), (col(1)?, col(1)?)],
            false,
        );

        let mut projection = Vec::with_capacity(required.len() + brk_count);
        for (res_slot, plan) in res_slots.iter().zip(main_plans) {
            if let Some(r_idx) = res_slot {
                let (_, sum_ty) = base_resources
                    .get(*r_idx)
                    .ok_or_else(|| missing("base resource slot out of range"))?;
                let raw_sum = col(main_w + 2 + brk_count + *r_idx)?;
                projection.push(PExpr::Cast {
                    x: Box::new(raw_sum),
                    to: sum_ty.clone(),
                    num: sum_ty.number,
                    ref_tables: Vec::new(),
                    load: false,
                });
            } else {
                let plan = plan
                    .as_deref()
                    .ok_or_else(|| missing("main field plan is absent"))?;
                projection.push(plan_expr(plan, &mut |(_, field)| {
                    position(&main_stored, field)
                })?);
            }
        }
        for i in 0..brk_count {
            projection.push(col(main_w + 2 + i)?);
        }

        let visible_width = required.len() + brk_count;
        self.added = self.added.saturating_add(64);
        let mut row = Row {
            rel: project_rel(final_join, projection),
            signal: None,
            width: visible_width,
            rep: Some(v.inner),
            keys: None,
        };
        self.set_level(v.inner);
        self.inner += 1;
        let result = (|| {
            if let Some(condition) = &v.cond {
                row = self.filter_row(row, condition)?;
            }
            let mut exprs = (0..visible_width).map(col).collect::<R<Vec<_>>>()?;
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
