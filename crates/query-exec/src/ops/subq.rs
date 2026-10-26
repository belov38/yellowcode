use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use query_ir::{ExecError, QValue, RelL};

use super::Cx;
use super::join::{KeySet, run_correlated};
use crate::key::Key;
use crate::plan::{Plan, PlanOptions, build, plan_rel};

type SectionRows = HashMap<(u32, [u8; 16]), QValue>;

#[derive(Default)]
pub struct SubCache {
    opts: PlanOptions,
    plans: RefCell<HashMap<usize, Rc<Plan>>>,
    sets: RefCell<HashMap<usize, Rc<KeySet>>>,
    sections: RefCell<HashMap<usize, Rc<SectionRows>>>,
}

impl SubCache {
    pub(crate) fn new(opts: PlanOptions) -> SubCache {
        SubCache {
            opts,
            ..SubCache::default()
        }
    }
}

fn plan_of(id: usize, q: &RelL, cx: &Cx<'_>) -> Result<Rc<Plan>, ExecError> {
    if let Some(p) = cx.subs.plans.borrow().get(&id) {
        return Ok(p.clone());
    }
    let mut planned = plan_rel(q, 0, cx.src, cx.temps, cx.subs.opts)?;
    if let Some(obs) = &cx.observation {
        obs.publish_deferred(id, &mut planned);
    }
    let p = Rc::new(planned);
    cx.subs.plans.borrow_mut().insert(id, p.clone());
    Ok(p)
}

pub(crate) fn in_query<'a>(
    lv: &[Key],
    q: &Arc<RelL>,
    correlated: bool,
    row: &[QValue],
    cx: &Cx<'a>,
) -> Result<Option<bool>, ExecError> {
    let id = Arc::as_ptr(q) as usize;
    let width = lv.len();
    if !correlated {
        let cached = cx.subs.sets.borrow().get(&id).cloned();
        let set = match cached {
            Some(s) => s,
            None => {
                let plan = plan_of(id, q, cx)?;
                let mut sub = cx.child(Vec::new());
                let mut set = KeySet::default();
                let held = set.fill(build(&plan), width, &mut sub)?;
                cx.absorb(&sub);
                sub.mem.shrink(held);
                cx.mem.hold(held)?;
                let set = Rc::new(set);
                cx.subs.sets.borrow_mut().insert(id, set.clone());
                set
            }
        };
        return Ok(set.probe(lv));
    }
    let plan = plan_of(id, q, cx)?;
    let copies = crate::model::vec_block(cx.outer.len() + 1, std::mem::size_of::<Vec<QValue>>())
        + cx.outer
            .iter()
            .map(crate::model::row_cap_model)
            .sum::<u64>()
        + crate::model::row_model(row);
    cx.mem.grow_scratch(copies + super::join::CORRELATED_TREE)?;
    let mut outer = Vec::with_capacity(cx.outer.len() + 1);
    outer.extend(cx.outer.iter().cloned());
    outer.push(row.to_vec());
    let mut sub = cx.child(outer);
    let mut op = build(&plan);
    let r = run_correlated(&mut op, lv, width, &mut sub);
    op.close(&mut sub);
    cx.absorb(&sub);
    r
}

pub(crate) fn section_value<'a>(
    owner: &QValue,
    rel: &Arc<RelL>,
    empty: &QValue,
    cx: &Cx<'a>,
) -> Result<QValue, ExecError> {
    let QValue::Ref { table, id } = owner else {
        return Ok(empty.clone());
    };
    let key = Arc::as_ptr(rel) as usize;
    let cached = cx.subs.sections.borrow().get(&key).cloned();
    let map = match cached {
        Some(m) => m,
        None => {
            let plan = plan_of(key, rel, cx)?;
            let mut sub = cx.child(Vec::new());
            let mut op = build(&plan);
            let mut map = HashMap::new();
            let mut model = crate::model::HashModel::new(
                (std::mem::size_of::<(u32, [u8; 16])>() + crate::model::VALUE_SIZE) as u64,
            );
            let mut held = 0u64;
            while let Some(row) = op.next(&mut sub)? {
                sub.tick()?;
                if let [QValue::Ref { table, id }, v] = row.as_slice() {
                    let kept = crate::model::value_model(v);
                    let growth = model.growth();
                    sub.mem.grow(kept + growth)?;
                    held += kept + growth;
                    map.insert((*table, *id), v.clone());
                    let old = model.inserted();
                    if old > 0 {
                        sub.mem.shrink(old);
                        held = held.saturating_sub(old);
                    }
                }
            }
            op.close(&mut sub);
            cx.absorb(&sub);
            sub.mem.shrink(held);
            cx.mem.hold(held)?;
            let m = Rc::new(map);
            cx.subs.sections.borrow_mut().insert(key, m.clone());
            m
        }
    };
    Ok(map
        .get(&(*table, *id))
        .cloned()
        .unwrap_or_else(|| empty.clone()))
}
