use std::collections::HashMap;

use query_ir::{AggL, ExecError, PExpr, QValue};
use query_syntax::Func;

use super::join::key_charged;
use super::{BoxOp, Cx, Op, eval};
use crate::key::Key;
use crate::model::{HashModel, key_model, keys_model, row_cap_model, vec_block, vec_push_growth};
use crate::num::AggState;
use crate::source::Row;

struct Group {
    rep: Vec<QValue>,
    states: Vec<AggState>,
}

struct SetState {
    idx: Vec<usize>,
    map: HashMap<Vec<Key>, usize>,
    map_model: HashModel,
    groups: Vec<Group>,
}

pub struct HashAggregate<'a> {
    input: BoxOp<'a>,
    keys: Vec<PExpr>,
    sets: Option<Vec<Vec<u16>>>,
    aggs: Vec<AggL>,
    out: Option<std::vec::IntoIter<Row>>,
    held: u64,
}

fn push_growth(
    len: usize,
    cap: usize,
    elem: usize,
    cx: &Cx<'_>,
    held: &mut u64,
) -> Result<u64, ExecError> {
    let (grow, release) = vec_push_growth(len, cap, elem);
    if grow > 0 {
        cx.mem.grow(grow)?;
        *held += grow;
    }
    Ok(release)
}

fn release(n: u64, cx: &Cx<'_>, held: &mut u64) {
    if n > 0 {
        cx.mem.shrink(n);
        *held = held.saturating_sub(n);
    }
}

impl<'a> HashAggregate<'a> {
    pub fn new(
        input: BoxOp<'a>,
        keys: Vec<PExpr>,
        sets: Option<Vec<Vec<u16>>>,
        aggs: Vec<AggL>,
    ) -> HashAggregate<'a> {
        HashAggregate {
            input,
            keys,
            sets,
            aggs,
            out: None,
            held: 0,
        }
    }

    fn new_group(&self, rep: Vec<QValue>) -> Result<Group, ExecError> {
        let mut states = Vec::with_capacity(self.aggs.len());
        for a in &self.aggs {
            let f = if a.f == Func::GroupedBy {
                Func::Count
            } else {
                a.f
            };
            states.push(AggState::new(f, a.arg_dec, a.distinct, false)?);
        }
        Ok(Group { rep, states })
    }

    fn group_bytes(&self, rep: &[QValue], gk: &[Key]) -> u64 {
        vec_block(rep.len(), crate::model::VALUE_SIZE)
            + rep.iter().map(crate::model::value_model).sum::<u64>()
            + keys_model(gk)
            + vec_block(self.aggs.len(), std::mem::size_of::<AggState>())
    }

    fn run(&mut self, cx: &mut Cx<'a>) -> Result<Vec<Row>, ExecError> {
        let kv = (std::mem::size_of::<Vec<Key>>() + std::mem::size_of::<usize>()) as u64;
        let mut sets: Vec<SetState> = match &self.sets {
            None => vec![(0..self.keys.len()).collect::<Vec<_>>()],
            Some(s) => s
                .iter()
                .map(|v| v.iter().map(|&i| usize::from(i)).collect())
                .collect(),
        }
        .into_iter()
        .map(|idx| SetState {
            idx,
            map: HashMap::new(),
            map_model: HashModel::new(kv),
            groups: Vec::new(),
        })
        .collect();
        while let Some(row) = self.input.next(cx)? {
            cx.tick()?;
            cx.mem.grow_scratch(
                vec_block(self.keys.len(), crate::model::VALUE_SIZE)
                    + vec_block(self.keys.len(), std::mem::size_of::<Key>()),
            )?;
            let mut kv_ = Vec::with_capacity(self.keys.len());
            let mut kk = Vec::with_capacity(self.keys.len());
            for k in &self.keys {
                let v = eval(k, &row, cx)?;
                kk.push(key_charged(&v, cx)?);
                kv_.push(v);
            }
            cx.mem.grow_scratch(vec_block(
                self.aggs.len(),
                std::mem::size_of::<Option<QValue>>(),
            ))?;
            let mut args: Vec<Option<QValue>> = Vec::with_capacity(self.aggs.len());
            for a in &self.aggs {
                args.push(match &a.arg {
                    Some(e) => Some(eval(e, &row, cx)?),
                    None => None,
                });
            }
            for si in 0..sets.len() {
                let Some(set) = sets.get(si) else { break };
                cx.mem.grow_scratch(
                    vec_block(set.idx.len(), std::mem::size_of::<Key>())
                        + set
                            .idx
                            .iter()
                            .filter_map(|&i| kk.get(i))
                            .map(key_model)
                            .sum::<u64>(),
                )?;
                let mut gk: Vec<Key> = Vec::with_capacity(set.idx.len());
                gk.extend(set.idx.iter().filter_map(|&i| kk.get(i).cloned()));
                let gi = match set.map.get(&gk) {
                    Some(&g) => g,
                    None => {
                        let mut rep: Vec<QValue> = Vec::with_capacity(set.idx.len());
                        rep.extend(set.idx.iter().filter_map(|&i| kv_.get(i).cloned()));
                        let n = self.group_bytes(&rep, &gk);
                        let table = set.map_model.growth();
                        cx.mem.grow(n + table)?;
                        self.held += n + table;
                        let g = self.new_group(rep)?;
                        let Some(set) = sets.get_mut(si) else { break };
                        let old = push_growth(
                            set.groups.len(),
                            set.groups.capacity(),
                            std::mem::size_of::<Group>(),
                            cx,
                            &mut self.held,
                        )?;
                        set.groups.push(g);
                        release(old, cx, &mut self.held);
                        let gi = set.groups.len() - 1;
                        set.map.insert(gk, gi);
                        let old = set.map_model.inserted();
                        release(old, cx, &mut self.held);
                        gi
                    }
                };
                let Some(g) = sets.get_mut(si).and_then(|s| s.groups.get_mut(gi)) else {
                    continue;
                };
                for ((st, a), arg) in g.states.iter_mut().zip(&self.aggs).zip(&args) {
                    if a.distinct
                        && let Some(v) = arg
                        && st.is_distinct()
                        && !matches!(v, QValue::Null)
                    {
                        let k = key_charged(v, cx)?;
                        if let Some(table) = st.distinct_growth(&k) {
                            let key = key_model(&k);
                            cx.mem.keep_scratch(key)?;
                            cx.mem.grow(table)?;
                            self.held += key + table;
                            let old = st.add_distinct(k, v, cx.env)?;
                            release(old, cx, &mut self.held);
                        }
                        continue;
                    }
                    st.add(arg.as_ref(), cx.env)?;
                }
            }
        }
        let mut out = Vec::new();
        for si in 0..sets.len() {
            let empty_total = sets
                .get(si)
                .is_some_and(|s| s.idx.is_empty() && s.groups.is_empty());
            if empty_total {
                let g = self.new_group(Vec::new())?;
                if let Some(s) = sets.get_mut(si) {
                    s.groups.push(g);
                }
            }
            let Some(set) = sets.get_mut(si) else { break };
            let idx = set.idx.clone();
            let width = self.keys.len() + self.aggs.len();
            for g in std::mem::take(&mut set.groups) {
                let rb = vec_block(width, crate::model::VALUE_SIZE);
                let old = push_growth(
                    out.len(),
                    out.capacity(),
                    std::mem::size_of::<Row>(),
                    cx,
                    &mut self.held,
                )?;
                cx.mem.grow(rb)?;
                self.held += rb;
                let mut row: Row = Vec::with_capacity(width);
                row.resize(self.keys.len(), QValue::Null);
                for (&i, v) in idx.iter().zip(g.rep) {
                    if let Some(slot) = row.get_mut(i) {
                        *slot = v;
                    }
                }
                for (st, a) in g.states.into_iter().zip(&self.aggs) {
                    row.push(if a.f == Func::GroupedBy {
                        QValue::Bool(grouped_by(a, &idx))
                    } else {
                        st.finish(cx.env)?
                    });
                }
                let extra = row_cap_model(&row).saturating_sub(rb);
                cx.mem.grow(extra)?;
                self.held += extra;
                out.push(row);
                release(old, cx, &mut self.held);
            }
        }
        Ok(out)
    }
}

fn grouped_by(a: &AggL, set: &[usize]) -> bool {
    match &a.arg {
        Some(PExpr::Lit(QValue::Number(n))) => n
            .to_i64()
            .and_then(|k| usize::try_from(k).ok())
            .is_some_and(|k| set.contains(&k)),
        _ => false,
    }
}

impl<'a> Op<'a> for HashAggregate<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.out = None;
        cx.mem.shrink(std::mem::take(&mut self.held));
        self.input.close(cx);
    }

    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        if self.out.is_none() {
            let rows = self.run(cx)?;
            self.out = Some(rows.into_iter());
        }
        match self.out.as_mut().and_then(Iterator::next) {
            Some(r) => {
                let n = row_cap_model(&r);
                cx.mem.shrink(n);
                self.held = self.held.saturating_sub(n);
                Ok(Some(r))
            }
            None => {
                self.out = None;
                cx.mem.shrink(std::mem::take(&mut self.held));
                Ok(None)
            }
        }
    }
}
