use std::cmp::Ordering;
use std::collections::{BTreeMap, btree_map};

use bsl_prim::BslDate;
use query_ir::lower::SliceL;
use query_ir::{ExecError, QValue};

use super::{BoxOp, Cx, Op, eval};
use crate::key::Key;
use crate::model::{keys_model, row_cap_model, vec_block};
use crate::plan::{self, Plan};
use crate::register::{RegisterKind, read_register_settings_with_observation};
use crate::source::Row;

type Recorder = (u32, [u8; 16]);
type Position = (i64, Option<Recorder>);
type Boundary = (BslDate, Option<Recorder>, bool);
struct Group {
    position: Position,
    rows: Vec<Row>,
    bytes: u64,
    key_bytes: u64,
}

pub struct Slice {
    step: SliceL,
    input: Plan,
    totals: Option<Plan>,
    observation: Option<crate::observe::RegisterObservation>,
    out: Option<btree_map::IntoValues<Vec<Key>, Group>>,
    rows: std::vec::IntoIter<Row>,
    held: u64,
    rows_extra: u64,
}

fn point(v: &QValue) -> Option<Boundary> {
    let p = match v {
        QValue::Date(d) => (*d, None, true),
        QValue::PointInTime(m) => (m.date, m.reference, true),
        QValue::Boundary(b) => {
            let (d, r, _) = point(&b.point)?;
            (d, r, b.include)
        }
        _ => return None,
    };
    (p.0 != BslDate::EMPTY).then_some(p)
}
fn recorder(v: Option<&QValue>) -> Option<Recorder> {
    match v {
        Some(QValue::Ref { table, id }) => Some((*table, *id)),
        _ => None,
    }
}
impl Slice {
    pub(crate) fn new(
        step: SliceL,
        input: Plan,
        totals: Option<Plan>,
        observation: Option<crate::observe::RegisterObservation>,
    ) -> Self {
        Self {
            step,
            input,
            totals,
            observation,
            out: None,
            rows: Vec::new().into_iter(),
            held: 0,
            rows_extra: 0,
        }
    }
    fn run<'a>(&mut self, cx: &mut Cx<'a>) -> Result<(), ExecError> {
        let bound = self
            .step
            .bound
            .as_ref()
            .map(|b| eval(b, &[], cx))
            .transpose()?;
        let boundary = bound.as_ref().and_then(point);
        let settings = read_register_settings_with_observation(
            cx.src,
            self.step.register,
            RegisterKind::Information,
            cx.observation.as_ref(),
        )?;
        if settings.slice_using == Some(false) {
            return Err(super::register::totals_off());
        }
        let use_totals =
            boundary.is_none() && settings.slice_using != Some(false) && self.totals.is_some();
        let mut selected = if use_totals {
            self.totals.take().unwrap_or(Plan::Empty)
        } else {
            std::mem::replace(&mut self.input, Plan::Empty)
        };
        if let (Some(obs), Some(session)) = (&self.observation, &cx.observation) {
            obs.publish_slice(session, &mut selected);
        }
        let mut input: BoxOp<'a> = plan::build(&selected);
        let mut groups: BTreeMap<Vec<Key>, Group> = BTreeMap::new();
        let result =
            (|| -> Result<(), ExecError> {
                while let Some(row) = input.next(cx)? {
                    cx.tick()?;
                    if self.step.active.is_some_and(|i| {
                        !matches!(row.get(usize::from(i)), Some(QValue::Bool(true)))
                    }) {
                        cx.mem.release_scratch();
                        continue;
                    }
                    let Some(QValue::Date(date)) = row.get(usize::from(self.step.period)) else {
                        return Err(ExecError::Fatal("slice period is not a date".into()));
                    };
                    let rec = self
                        .step
                        .recorder
                        .and_then(|i| recorder(row.get(usize::from(i))));
                    let position = (date.ticks(), rec);
                    if let Some((d, r, include)) = boundary {
                        let cmp = if r.is_some() && self.step.recorder.is_some() {
                            position.cmp(&(d.ticks(), r))
                        } else {
                            date.ticks().cmp(&d.ticks())
                        };
                        let fits = if self.step.first {
                            cmp == Ordering::Greater || (include && cmp == Ordering::Equal)
                        } else {
                            cmp == Ordering::Less || (include && cmp == Ordering::Equal)
                        };
                        if !fits {
                            cx.mem.release_scratch();
                            continue;
                        }
                    }
                    cx.mem.grow_scratch(vec_block(
                        self.step.dimensions.len(),
                        std::mem::size_of::<Key>(),
                    ))?;
                    let mut key = Vec::with_capacity(self.step.dimensions.len());
                    for i in &self.step.dimensions {
                        key.push(super::join::key_charged(
                            row.get(usize::from(*i)).unwrap_or(&QValue::Null),
                            cx,
                        )?);
                    }
                    let bytes = row_cap_model(&row).saturating_add(128);
                    match groups.get_mut(&key) {
                        Some(group) => {
                            let cmp = position.cmp(&group.position);
                            let better = if self.step.first {
                                cmp == Ordering::Less
                            } else {
                                cmp == Ordering::Greater
                            };
                            if better || cmp == Ordering::Equal {
                                cx.mem.grow(bytes)?;
                                self.held = self.held.saturating_add(bytes);
                                if better {
                                    drop(std::mem::take(&mut group.rows));
                                    cx.mem.shrink(group.bytes);
                                    self.held = self.held.saturating_sub(group.bytes);
                                    group.bytes = 0;
                                    group.position = position;
                                }
                                group.bytes = group.bytes.saturating_add(bytes);
                                group.rows.push(row);
                            }
                        }
                        None => {
                            let key_bytes = keys_model(&key).saturating_add(512);
                            let growth = key_bytes.saturating_add(bytes);
                            cx.mem.grow(growth)?;
                            self.held = self.held.saturating_add(growth);
                            groups.insert(
                                key,
                                Group {
                                    position,
                                    rows: vec![row],
                                    bytes,
                                    key_bytes,
                                },
                            );
                        }
                    }
                    cx.mem.release_scratch();
                }
                Ok(())
            })();
        input.close(cx);
        cx.mem.release_scratch();
        if let Err(error) = result {
            drop(groups);
            self.close(cx);
            return Err(error);
        }
        self.out = Some(groups.into_values());
        Ok(())
    }
}
impl<'a> Op<'a> for Slice {
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.out.is_none() {
            self.run(cx)?;
        }
        loop {
            if let Some(row) = self.rows.next() {
                let mut release = row_cap_model(&row);
                if self.rows.len() == 0 {
                    self.rows = Vec::new().into_iter();
                    release = release.saturating_add(std::mem::take(&mut self.rows_extra));
                }
                cx.mem.shrink(release);
                self.held = self.held.saturating_sub(release);
                return Ok(Some(row));
            }
            match self.out.as_mut().and_then(Iterator::next) {
                Some(g) => {
                    cx.mem.shrink(g.key_bytes);
                    self.held = self.held.saturating_sub(g.key_bytes);
                    self.rows_extra = (g.rows.len() as u64).saturating_mul(128);
                    self.rows = g.rows.into_iter();
                }
                None => {
                    self.close(cx);
                    return Ok(None);
                }
            }
        }
    }
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.out = None;
        self.rows = Vec::new().into_iter();
        cx.mem.shrink(self.held);
        self.held = 0;
        self.rows_extra = 0;
    }
}
