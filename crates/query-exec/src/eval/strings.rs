use std::collections::HashMap;

use bsl_prim::{Collation, Number, Strength};
use query_ir::{ExecError, QType, QValue};
use query_syntax::Func;

use super::{charge_string, code_points, fatal, push_units, str_value};
use crate::model::{HashModel, block, vec_block};
use crate::ops::MemBudget;
use crate::source::ExecEnv;

fn text(v: &QValue) -> Result<&[u16], ExecError> {
    match v {
        QValue::String(s) => Ok(&s.0),
        _ => Err(fatal("a string argument is expected")),
    }
}

pub(super) fn int_arg(v: &QValue) -> Result<i64, ExecError> {
    match v {
        QValue::Number(n) => Ok(n.trunc_to_i64_saturating()),
        _ => Err(fatal("a number argument is expected")),
    }
}

fn len_value(n: usize) -> QValue {
    QValue::Number(Number::from_u64(n as u64))
}

pub(super) fn pad_fixed(v: QValue, ty: &QType, mem: &MemBudget<'_>) -> Result<QValue, ExecError> {
    Ok(match (&v, ty.string) {
        (QValue::String(s), Some(q)) if q.fixed && (s.0.len() as u64) < u64::from(q.len) => {
            charge_string(u64::from(q.len), mem)?;
            let mut u = Vec::with_capacity(q.len as usize);
            u.extend_from_slice(&s.0);
            u.resize(q.len as usize, 0x20);
            str_value(u)
        }
        _ => v,
    })
}

struct Classes<'c> {
    coll: &'c dyn Collation,
    mem: &'c MemBudget<'c>,
    by_cp: HashMap<u32, u32>,
    by_cp_model: HashModel,
    by_key: HashMap<Vec<u8>, u32>,
    by_key_model: HashModel,
}

impl<'c> Classes<'c> {
    fn new(coll: &'c dyn Collation, mem: &'c MemBudget<'c>) -> Self {
        Classes {
            coll,
            mem,
            by_cp: HashMap::new(),
            by_cp_model: HashModel::new(8),
            by_key: HashMap::new(),
            by_key_model: HashModel::new((std::mem::size_of::<Vec<u8>>() + 4) as u64),
        }
    }

    fn class(&mut self, cp: u32) -> Result<u32, ExecError> {
        if let Some(&c) = self.by_cp.get(&cp) {
            return Ok(c);
        }
        let mut u = Vec::with_capacity(2);
        push_units(cp, &mut u);
        self.mem.grow_scratch(block(32))?;
        let key = self.coll.sort_key(Strength::Secondary, &u);
        let next = self.by_key.len() as u32;
        let id = match self.by_key.get(&key) {
            Some(&id) => id,
            None => {
                self.mem.grow_scratch(self.by_key_model.growth())?;
                self.by_key.insert(key, next);
                self.mem.shrink_scratch(self.by_key_model.inserted());
                next
            }
        };
        self.mem.grow_scratch(self.by_cp_model.growth())?;
        self.by_cp.insert(cp, id);
        self.mem.shrink_scratch(self.by_cp_model.inserted());
        Ok(id)
    }

    fn of_text(&mut self, t: &[u16]) -> Result<(Vec<u32>, Vec<usize>), ExecError> {
        self.mem
            .grow_scratch(vec_block(t.len(), 4) + vec_block(t.len() + 1, 8))?;
        let (mut ids, mut offs) = (Vec::with_capacity(t.len()), Vec::with_capacity(t.len() + 1));
        let mut at = 0usize;
        for cp in code_points(t) {
            offs.push(at);
            ids.push(self.class(cp)?);
            at += if cp > 0xFFFF { 2 } else { 1 };
        }
        offs.push(at);
        Ok((ids, offs))
    }
}

fn occurrences(hay: &[u32], needle: &[u32], mem: &MemBudget<'_>) -> Result<Vec<usize>, ExecError> {
    let m = needle.len();
    let mut out = Vec::new();
    if m == 0 || hay.len() < m {
        return Ok(out);
    }
    mem.grow_scratch(vec_block(m, 8) + vec_block(hay.len() / m, 8))?;
    out.reserve(hay.len() / m);
    let mut fail = vec![0usize; m];
    let mut k = 0;
    for i in 1..m {
        while k > 0 && needle.get(i) != needle.get(k) {
            k = fail.get(k - 1).copied().unwrap_or(0);
        }
        if needle.get(i) == needle.get(k) {
            k += 1;
        }
        if let Some(f) = fail.get_mut(i) {
            *f = k;
        }
    }
    k = 0;
    for (i, h) in hay.iter().enumerate() {
        while k > 0 && needle.get(k) != Some(h) {
            k = fail.get(k - 1).copied().unwrap_or(0);
        }
        if needle.get(k) == Some(h) {
            k += 1;
        }
        if k == m {
            out.push(i + 1 - m);
            k = 0;
        }
    }
    Ok(out)
}

fn map_case(t: &[u16], upper: bool) -> Vec<u16> {
    let mut out = Vec::with_capacity(t.len());
    for cp in code_points(t) {
        let mapped = match char::from_u32(cp) {
            Some('\u{130}') if !upper => 'i',
            Some(ch) => {
                let mut it: Box<dyn Iterator<Item = char>> = if upper {
                    Box::new(ch.to_uppercase())
                } else {
                    Box::new(ch.to_lowercase())
                };
                match (it.next(), it.next()) {
                    (Some(m), None) => m,
                    _ => ch,
                }
            }
            None => {
                out.push(cp as u16);
                continue;
            }
        };
        push_units(u32::from(mapped), &mut out);
    }
    out
}

fn trim(t: &[u16], left: bool, right: bool) -> Vec<u16> {
    let mut s = t;
    if left {
        let n = s.iter().take_while(|&&u| u == 0x20).count();
        s = s.get(n..).unwrap_or(&[]);
    }
    if right {
        let n = s.iter().rev().take_while(|&&u| u == 0x20).count();
        s = s.get(..s.len() - n).unwrap_or(&[]);
    }
    s.to_vec()
}

fn substring(t: &[u16], start: i64, len: i64) -> Vec<u16> {
    if len <= 0 {
        return Vec::new();
    }
    let start = (start.max(1) - 1) as u64;
    let begin = usize::try_from(start).unwrap_or(usize::MAX).min(t.len());
    let end = begin
        .saturating_add(usize::try_from(len).unwrap_or(usize::MAX))
        .min(t.len());
    t.get(begin..end).unwrap_or(&[]).to_vec()
}

pub fn present(v: &QValue) -> Result<Vec<u16>, ExecError> {
    let s = match v {
        QValue::String(s) => return Ok(s.0.to_vec()),
        QValue::Bool(b) => (if *b { "Да" } else { "Нет" }).to_string(),
        QValue::Number(n) => {
            let int = n.int_digits();
            let int = int.trim_start_matches('0');
            let int = if int.is_empty() { "0" } else { int };
            let frac = n.frac_digits();
            let frac = frac.trim_end_matches('0');
            let mut out = String::new();
            let all_zero = int == "0" && frac.is_empty();
            if n.is_negative() && !all_zero {
                out.push('-');
            }
            let first = int.len() % 3;
            for (i, ch) in int.chars().enumerate() {
                if i > 0 && (i + 3 - first) % 3 == 0 {
                    out.push('\u{a0}');
                }
                out.push(ch);
            }
            if !frac.is_empty() {
                out.push(',');
                out.push_str(frac);
            }
            out
        }
        QValue::Date(d) => {
            let p = d.parts();
            format!(
                "{:02}.{:02}.{:04} {}:{:02}:{:02}",
                p.day, p.month, p.year, p.hour, p.minute, p.second
            )
        }
        _ => return Err(fatal("СТРОКА of this value is not supported")),
    };
    Ok(s.encode_utf16().collect())
}

pub(super) fn call(
    f: Func,
    args: &[QValue],
    env: &ExecEnv<'_>,
    mem: &MemBudget<'_>,
) -> Result<QValue, ExecError> {
    let arg = |i: usize| args.get(i).ok_or_else(|| fatal("too few arguments"));
    let bounded = |t: &[u16]| charge_string(t.len() as u64, mem);
    Ok(match f {
        Func::String => str_value(super::text_of_charged(arg(0)?, mem)?),
        Func::StringLength => len_value(text(arg(0)?)?.len()),
        Func::TrimL => {
            let t = text(arg(0)?)?;
            bounded(t)?;
            str_value(trim(t, true, false))
        }
        Func::TrimR => {
            let t = text(arg(0)?)?;
            bounded(t)?;
            str_value(trim(t, false, true))
        }
        Func::TrimAll => {
            let t = text(arg(0)?)?;
            bounded(t)?;
            str_value(trim(t, true, true))
        }
        Func::Upper => {
            let t = text(arg(0)?)?;
            bounded(t)?;
            str_value(map_case(t, true))
        }
        Func::Lower => {
            let t = text(arg(0)?)?;
            bounded(t)?;
            str_value(map_case(t, false))
        }
        Func::Substring => {
            let (t, start, len) = (text(arg(0)?)?, int_arg(arg(1)?)?, int_arg(arg(2)?)?);
            let units = u64::try_from(len.max(0)).unwrap_or(0).min(t.len() as u64);
            charge_string(units, mem)?;
            str_value(substring(t, start, len))
        }
        Func::Left => {
            let (t, n) = (text(arg(0)?)?, int_arg(arg(1)?)?);
            let units = u64::try_from(n.max(0)).unwrap_or(0).min(t.len() as u64);
            charge_string(units, mem)?;
            str_value(substring(t, 1, n))
        }
        Func::Right => {
            let (t, n) = (text(arg(0)?)?, int_arg(arg(1)?)?);
            if n <= 0 {
                str_value(Vec::new())
            } else {
                let cut = t
                    .len()
                    .saturating_sub(usize::try_from(n).unwrap_or(usize::MAX));
                charge_string((t.len() - cut) as u64, mem)?;
                str_value(t.get(cut..).unwrap_or(&[]).to_vec())
            }
        }
        Func::StrFind => {
            let (hay, needle) = (text(arg(0)?)?, text(arg(1)?)?);
            if needle.is_empty() {
                return Ok(len_value(1));
            }
            let mut cl = Classes::new(env.collation, mem);
            let ((h, offs), (nd, _)) = (cl.of_text(hay)?, cl.of_text(needle)?);
            let pos = occurrences(&h, &nd, mem)?
                .first()
                .and_then(|&i| offs.get(i))
                .map_or(0, |&o| o + 1);
            len_value(pos)
        }
        Func::StrReplace => {
            let (hay, needle, repl) = (text(arg(0)?)?, text(arg(1)?)?, text(arg(2)?)?);
            if needle.is_empty() {
                bounded(hay)?;
                return Ok(str_value(hay.to_vec()));
            }
            let mut cl = Classes::new(env.collation, mem);
            let ((h, offs), (nd, _)) = (cl.of_text(hay)?, cl.of_text(needle)?);
            let occ = occurrences(&h, &nd, mem)?;
            let needle_units: u64 = occ
                .iter()
                .map(|&i| {
                    let a = offs.get(i).copied().unwrap_or(hay.len());
                    let b = offs.get(i + nd.len()).copied().unwrap_or(hay.len());
                    (b - a) as u64
                })
                .sum();
            let len_out = (hay.len() as u64 - needle_units)
                .saturating_add((occ.len() as u64).saturating_mul(repl.len() as u64));
            charge_string(len_out, mem)?;
            let mut out = Vec::with_capacity(usize::try_from(len_out).unwrap_or(usize::MAX));
            let mut done = 0usize;
            for i in occ {
                let (a, b) = (
                    offs.get(i).copied().unwrap_or(hay.len()),
                    offs.get(i + nd.len()).copied().unwrap_or(hay.len()),
                );
                out.extend_from_slice(hay.get(done..a).unwrap_or(&[]));
                out.extend_from_slice(repl);
                done = b;
            }
            out.extend_from_slice(hay.get(done..).unwrap_or(&[]));
            str_value(out)
        }
        _ => return Err(fatal("not a string function")),
    })
}
