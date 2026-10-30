use crate::coll::{ExecCollation, Keys};
use bsl_prim::{Collation, Strength};
use query_ir::{ColumnInfo, ExecError, NestedResult, QType, QValue};

use crate::key::Key;

pub const BLOCK_HEADER: u64 = 16;

pub const fn block(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    n.div_ceil(16)
        .saturating_mul(16)
        .saturating_add(BLOCK_HEADER)
}

pub const fn vec_block(cap: usize, elem: usize) -> u64 {
    block((cap as u64).saturating_mul(elem as u64))
}

pub const VALUE_SIZE: usize = std::mem::size_of::<QValue>();

pub fn value_model(v: &QValue) -> u64 {
    match v {
        QValue::String(s) => block(16 + 2 * s.0.len() as u64),
        QValue::Binary(b) | QValue::Storage(b) => block(16 + b.len() as u64),
        QValue::Number(n) => {
            let limbs = n.size_hint_limbs();
            if limbs >= 2 { block(4 * limbs) } else { 0 }
        }
        QValue::Type(t) => type_model(t),
        QValue::Nested(n) => nested_model(n),
        QValue::PointInTime(_) => block(std::mem::size_of::<query_ir::value::Moment>() as u64),
        QValue::Boundary(b) => {
            block(std::mem::size_of::<query_ir::value::Boundary>() as u64) + value_model(&b.point)
        }
        QValue::Null
        | QValue::Undefined
        | QValue::Bool(_)
        | QValue::Date(_)
        | QValue::Uuid(_)
        | QValue::Ref { .. }
        | QValue::SysEnum(..) => 0,
    }
}

pub fn type_model(t: &QType) -> u64 {
    vec_block(t.refs.capacity(), std::mem::size_of::<query_ir::RefAtom>())
        + vec_block(t.others.capacity(), std::mem::size_of::<query_ir::Other>())
}

pub fn column_model(c: &ColumnInfo) -> u64 {
    block(c.name.capacity() as u64)
        + type_model(&c.ty)
        + c.nested.as_ref().map_or(0, |n| columns_model(n))
}

pub fn columns_model(cs: &[ColumnInfo]) -> u64 {
    vec_block(cs.len(), std::mem::size_of::<ColumnInfo>())
        + cs.iter().map(column_model).sum::<u64>()
}

pub fn nested_model(n: &NestedResult) -> u64 {
    block(16 + std::mem::size_of::<NestedResult>() as u64)
        + columns_model(&n.columns)
        + rows_model(&n.rows)
}

pub fn row_model(r: &[QValue]) -> u64 {
    vec_block(r.len(), VALUE_SIZE) + r.iter().map(value_model).sum::<u64>()
}

pub fn row_cap_model(r: &Vec<QValue>) -> u64 {
    vec_block(r.capacity(), VALUE_SIZE) + r.iter().map(value_model).sum::<u64>()
}

pub fn rows_model(rows: &Vec<Vec<QValue>>) -> u64 {
    let mut seen = Payloads::default();
    vec_block(rows.capacity(), std::mem::size_of::<Vec<QValue>>())
        + rows
            .iter()
            .map(|r| {
                vec_block(r.capacity(), VALUE_SIZE)
                    + r.iter().map(|v| seen.value_model(v)).sum::<u64>()
            })
            .sum::<u64>()
}

pub fn rows_clone_model(rows: &[Vec<QValue>]) -> u64 {
    rows_clone_model_of(rows.len(), rows.iter())
}

pub fn rows_clone_model_of<'a>(n: usize, rows: impl Iterator<Item = &'a Vec<QValue>>) -> u64 {
    let mut seen = Payloads::default();
    vec_block(n, std::mem::size_of::<Vec<QValue>>())
        + rows
            .map(|r| {
                vec_block(r.len(), VALUE_SIZE) + r.iter().map(|v| seen.value_model(v)).sum::<u64>()
            })
            .sum::<u64>()
}

fn payload_id(v: &QValue) -> Option<(usize, usize)> {
    match v {
        QValue::String(s) if !s.0.is_empty() => Some((s.0.as_ptr() as usize, s.0.len())),
        QValue::Binary(b) | QValue::Storage(b) if !b.is_empty() => {
            Some((b.as_ptr() as usize, b.len()))
        }
        QValue::Nested(n) => {
            let r: &NestedResult = n;
            Some((std::ptr::from_ref(r) as usize, 0))
        }
        _ => None,
    }
}

#[derive(Default)]
pub struct Payloads {
    seen: std::collections::HashSet<(usize, usize)>,
    model: Option<HashModel>,
}

impl Payloads {
    pub fn value_model(&mut self, v: &QValue) -> u64 {
        match payload_id(v) {
            Some(id) if !self.seen.insert(id) => 0,
            _ => value_model(v),
        }
    }

    pub fn value_model_charged(
        &mut self,
        v: &QValue,
        charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
    ) -> Result<(u64, u64), ExecError> {
        let Some(id) = payload_id(v) else {
            return Ok((value_model(v), 0));
        };
        if self.seen.contains(&id) {
            return Ok((0, 0));
        }
        let model = self
            .model
            .get_or_insert_with(|| HashModel::new(std::mem::size_of::<(usize, usize)>() as u64));
        charge(model.growth())?;
        self.seen.insert(id);
        Ok((value_model(v), model.inserted()))
    }

    pub fn table(&self) -> u64 {
        self.model.as_ref().map_or(0, HashModel::table)
    }
}

pub fn row_cap_model_shared(
    r: &Vec<QValue>,
    seen: &mut Payloads,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<(u64, u64), ExecError> {
    let mut bytes = vec_block(r.capacity(), VALUE_SIZE);
    let mut release = 0;
    for v in r {
        let (b, old) = seen.value_model_charged(v, charge)?;
        bytes += b;
        release += old;
    }
    Ok((bytes, release))
}

pub fn key_model(k: &Key) -> u64 {
    block(k.0.capacity() as u64)
}

pub fn keys_model(ks: &[Key]) -> u64 {
    vec_block(ks.len(), std::mem::size_of::<Key>()) + ks.iter().map(key_model).sum::<u64>()
}

pub const PREFLIGHT_UNITS: usize = 512;

pub fn key_bound(v: &QValue) -> u64 {
    1 + match v {
        QValue::String(s) => 40 * crate::key::key_text(&s.0).len() as u64 + 64,
        QValue::Number(n) => 24 * n.size_hint_limbs() + 24,
        QValue::Date(_) => 8,
        QValue::Bool(_) => 1,
        QValue::Uuid(_) => 16,
        QValue::Ref { .. } => 20,
        QValue::Binary(b) | QValue::Storage(b) => b.len() as u64,
        QValue::Type(t) => 64 + 40 * (t.refs.len() + t.others.len()) as u64,
        QValue::SysEnum(..) => 6,
        QValue::PointInTime(_) => 30,
        QValue::Boundary(b) => 1 + key_bound(&b.point),
        QValue::Null | QValue::Undefined | QValue::Nested(_) => 0,
    }
}

pub fn key_cost(v: &QValue, coll: &dyn Collation) -> u64 {
    key_sizes(v, coll).1
}

pub fn charge_key(
    v: &QValue,
    coll: &dyn Collation,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<u64, ExecError> {
    try_charge_key(v, ExecCollation::Legacy(coll), charge)
}

pub fn try_charge_key(
    v: &QValue,
    coll: ExecCollation<'_>,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<u64, ExecError> {
    let walk = walk_bytes(v);
    let (_, peak) = key_sizes_charged(v, coll, charge)?;
    charge(peak - walk)?;
    Ok(peak)
}

pub fn key_sizes_charged(
    v: &QValue,
    coll: ExecCollation<'_>,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<(u64, u64), ExecError> {
    let walk = walk_bytes(v);
    if walk > 0 {
        charge(walk)?;
    }
    key_sizes_with(v, coll)
}

pub fn walk_bytes(v: &QValue) -> u64 {
    match v {
        QValue::String(s) => text_walk_bytes(crate::key::key_text(&s.0)),
        _ => 0,
    }
}

fn text_walk_bytes(text: &[u16]) -> u64 {
    if text.len() > PREFLIGHT_UNITS {
        block(2 * text.len() as u64 + 2)
    } else {
        0
    }
}

pub fn try_charge_totals_key(
    v: &QValue,
    coll: ExecCollation<'_>,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<u64, ExecError> {
    let QValue::String(s) = v else {
        return try_charge_key(v, coll, charge);
    };
    let walk = text_walk_bytes(&s.0);
    if walk > 0 {
        charge(walk)?;
    }
    let (_, peak) = text_key_sizes(&s.0, coll)?;
    charge(peak.saturating_sub(walk))?;
    Ok(peak.max(walk))
}

pub fn key_sizes(v: &QValue, coll: &dyn Collation) -> (u64, u64) {
    crate::coll::infallible(key_sizes_with(v, coll))
}

pub fn try_key_sizes(v: &QValue, coll: ExecCollation<'_>) -> Result<(u64, u64), ExecError> {
    key_sizes_with(v, coll)
}

fn key_sizes_with<C: Keys>(v: &QValue, coll: C) -> Result<(u64, u64), C::Error> {
    let QValue::String(s) = v else {
        let n = block(2 * key_bound(v));
        return Ok((n, n));
    };
    text_key_sizes(crate::key::key_text(&s.0), coll)
}

fn text_key_sizes<C: Keys>(text: &[u16], coll: C) -> Result<(u64, u64), C::Error> {
    let units = text.len() as u64;
    let bound = || {
        let peak = block(2 * (1 + 40 * units + 64));
        (block(37 * units + 48).min(peak), peak)
    };
    if text.len() <= PREFLIGHT_UNITS {
        return Ok(bound());
    }
    Ok(match coll.key_len(Strength::Secondary, text)? {
        Some(need) => (block(need as u64 + 1), icu_build_cost(units, need as u64)),
        None => bound(),
    })
}

fn icu_build_cost(units: u64, need: u64) -> u64 {
    let src = block(2 * units + 2);
    let first = block(4 * units + 32 + 4);
    let second = block(need + 4);
    src + block(need) + first.max(second)
}

pub fn vec_push_growth(len: usize, cap: usize, elem: usize) -> (u64, u64) {
    if len < cap {
        return (0, 0);
    }
    (
        vec_block(grown_cap(len, cap, elem), elem),
        vec_block(cap, elem),
    )
}

fn grown_cap(len: usize, cap: usize, elem: usize) -> usize {
    let min = if elem == 1 {
        8
    } else if elem <= 1024 {
        4
    } else {
        1
    };
    (cap * 2).max(min).max(len + 1)
}

pub fn vec_pushes_peak(n: usize, elem: usize) -> u64 {
    let (mut cap, mut peak) = (0usize, 0u64);
    while cap < n {
        let (new, old) = vec_push_growth(cap, cap, elem);
        peak = peak.max(new + old);
        cap = grown_cap(cap, cap, elem);
    }
    peak
}

pub fn hash_table_model(buckets: usize, kv: u64) -> u64 {
    if buckets == 0 {
        return 0;
    }
    block((buckets as u64).saturating_mul(kv) + buckets as u64 + 16)
}

pub fn hash_buckets_for(cap: usize) -> usize {
    if cap == 0 {
        0
    } else if cap < 4 {
        4
    } else if cap < 8 {
        8
    } else {
        (cap * 8 / 7).next_power_of_two()
    }
}

pub fn hash_capacity(buckets: usize) -> usize {
    if buckets < 8 {
        buckets.saturating_sub(1)
    } else {
        buckets / 8 * 7
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HashModel {
    len: usize,
    buckets: usize,
    kv: u64,
}

impl HashModel {
    pub fn new(kv: u64) -> HashModel {
        HashModel {
            len: 0,
            buckets: 0,
            kv,
        }
    }

    pub fn growth(&self) -> u64 {
        if self.len < hash_capacity(self.buckets) {
            return 0;
        }
        hash_table_model(self.next_buckets(), self.kv)
    }

    fn next_buckets(&self) -> usize {
        if self.buckets == 0 {
            hash_buckets_for(1)
        } else {
            (self.buckets * 2).max(hash_buckets_for(self.len + 1))
        }
    }

    pub fn inserted(&mut self) -> u64 {
        let freed = if self.len < hash_capacity(self.buckets) {
            0
        } else {
            let old = hash_table_model(self.buckets, self.kv);
            self.buckets = self.next_buckets();
            old
        };
        self.len += 1;
        freed
    }

    pub fn table(&self) -> u64 {
        hash_table_model(self.buckets, self.kv)
    }
}

pub fn hash_inserts_peak(n: usize, kv: u64) -> u64 {
    let mut m = HashModel::new(kv);
    let mut peak = 0;
    while m.len < n {
        let grow = m.growth();
        if grow > 0 {
            peak = peak.max(grow + m.table());
            m.inserted();
        } else {
            m.len = hash_capacity(m.buckets).min(n);
        }
    }
    peak
}
