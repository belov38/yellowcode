use super::chartspec::Part;
use super::read::{R as ReadResult, Read};
use super::values::{count, is, slice};
use crate::templates::allocation::Allocation;
use brace::Node;

pub(crate) const A_START: Pos = 18;
const A_END: Pos = 117;
pub(crate) const B_START: Pos = 118;
const B_END: Pos = 140;
pub(crate) const C_START: Pos = 144;
const C_END: Pos = 156;
const D_START: Pos = 160;
const D_END: Pos = 164;
pub(crate) const E_START: Pos = 166;
const E_END: Pos = 192;
pub(crate) const F_START: Pos = 193;
const F_END: Pos = 203;
pub(crate) const G_START: Pos = 204;
const SERIES_LEN: Pos = 11;
const ITEM_LEN: Pos = 3;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fields<'a> {
    pub head: &'a [Node],
    pub series: Groups<'a>,
    pub exseries: Option<&'a [Node]>,
    pub points_head: &'a [Node],
    pub points: Groups<'a>,
    pub a: &'a [Node],
    pub items: Groups<'a>,
    pub b: &'a [Node],
    pub k: &'a Node,
    pub sids: &'a [Node],
    pub trend: &'a [Node],
    pub c: &'a [Node],
    pub scales: &'a [Node],
    pub d: &'a [Node],
    pub pext: &'a [Node],
    pub sext: &'a [Node],
    pub e: &'a [Node],
    pub f: &'a [Node],
    pub iext: &'a [Node],
    pub g: &'a [Node],
}

impl<'a> Fields<'a> {
    pub(crate) fn part(&self, p: Part) -> (&'a [Node], Pos) {
        match p {
            Part::A => (self.a, A_START),
            Part::B => (self.b, B_START),
            Part::C => (self.c, C_START),
            Part::E => (self.e, E_START),
            Part::F => (self.f, F_START),
        }
    }
}

pub(crate) type Pos = i128;

fn cl(x: Pos, n: usize) -> i64 {
    let n = i128::try_from(n).unwrap_or(i128::MAX / 2);
    i64::try_from(x.clamp(-n - 1, n + 1)).unwrap_or(0)
}

fn sl(s: &[Node], a: Pos, n: Pos) -> &[Node] {
    slice(s, cl(a, s.len()), cl(a + n, s.len()))
}

fn get<'s>(s: &'s [Node], p: Pos, read: Read<'_>) -> ReadResult<&'s Node> {
    read.at(s, cl(p, s.len()))
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Groups<'a> {
    source: &'a [Node],
    start: Pos,
    stride: Pos,
    len: usize,
}
impl<'a> Groups<'a> {
    fn empty() -> Self {
        Self {
            source: &[],
            start: 0,
            stride: 0,
            len: 0,
        }
    }
    pub(crate) fn len(&self) -> usize {
        self.len
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &'a [Node]> + Clone {
        let groups = *self;
        (0..groups.len).map(move |i| {
            sl(
                groups.source,
                groups.start + Pos::try_from(i).unwrap_or(0) * groups.stride,
                groups.stride,
            )
        })
    }
}
impl PartialEq for Groups<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl<'a> IntoIterator for &Groups<'a> {
    type Item = &'a [Node];
    type IntoIter = GroupIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        GroupIter {
            groups: *self,
            at: 0,
        }
    }
}
pub(crate) struct GroupIter<'a> {
    groups: Groups<'a>,
    at: usize,
}
impl<'a> Iterator for GroupIter<'a> {
    type Item = &'a [Node];
    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.groups.len {
            return None;
        }
        let at = self.at;
        self.at += 1;
        Some(sl(
            self.groups.source,
            self.groups.start + Pos::try_from(at).ok()? * self.groups.stride,
            self.groups.stride,
        ))
    }
}

fn groups<'s>(s: &'s [Node], p: Pos, n: i64, len: Pos, read: Read<'_>) -> ReadResult<Groups<'s>> {
    let n = Pos::try_from(count(n)).unwrap_or(0);
    let total = Pos::try_from(s.len()).unwrap_or(Pos::MAX);
    if n > 0 && p + n * len > total {
        return Err(read.semantic("chart stream too short for groups", || {
            format!("chart stream too short for {n} groups of {len}")
        }));
    }
    Ok(Groups {
        source: s,
        start: p,
        stride: len,
        len: usize::try_from(n).unwrap_or(0),
    })
}

pub(crate) fn split_with<'s>(s: &'s [Node], allocation: Allocation<'_>) -> ReadResult<Fields<'s>> {
    let read = Read::new(allocation);
    if !is(read.at(s, 0)?, "75") {
        let node = read.at(s, 0)?;
        return Err(read.semantic("chart version", || {
            format!("chart version {}", brace::to_compact(node))
        }));
    }
    let head = sl(s, 0, 5);
    let mut p: Pos = 5;
    let design_s = is(read.at(s, 3)?, "1");
    let ns = read.int(read.at(s, 4)?)?;
    let (series, exseries) = if design_s {
        let g = groups(s, p, ns, SERIES_LEN, read)?;
        p += Pos::from(ns) * SERIES_LEN;
        let ex = sl(s, p, SERIES_LEN);
        p += SERIES_LEN;
        (g, Some(ex))
    } else {
        (Groups::empty(), None)
    };
    let points_head = sl(s, p, 2);
    let design_p = is(get(s, p, read)?, "1");
    let np = read.int(get(s, p + 1, read)?)?;
    p += 2;
    let points = if design_p {
        let g = groups(s, p, np, SERIES_LEN, read)?;
        p += Pos::from(np) * SERIES_LEN;
        g
    } else {
        Groups::empty()
    };
    let npts = Pos::try_from(points.len()).unwrap_or(0);
    let nitems = Pos::try_from(series.len()).unwrap_or(0) * npts;
    let a = sl(s, p, A_END - A_START + 1);
    p += A_END - A_START + 1;
    let items = groups(
        s,
        p,
        i64::try_from(nitems).map_err(|_| read.invalid("chart items"))?,
        ITEM_LEN,
        read,
    )?;
    p += nitems * ITEM_LEN;
    let b = sl(s, p, B_END - B_START + 1);
    p += B_END - B_START + 1;
    let k_node = get(s, p, read)?;
    let k = Pos::from(read.int(k_node)?);
    p += 1;
    let mut take = |n: Pos| -> &[Node] {
        let x = sl(s, p, n);
        p += n;
        x
    };
    let sids = take(k);
    let trend = take(k);
    let c = take(C_END - C_START + 1);
    let scales = take(3);
    let d = take(D_END - D_START + 1);
    let pext = take(npts);
    let sext = take(k);
    let e = take(E_END - E_START + 1);
    let f = take(F_END - F_START + 1);
    let iext = take(nitems);
    let g = slice(s, cl(p, s.len()), cl(Pos::MAX, s.len()));
    Ok(Fields {
        head,
        series,
        exseries,
        points_head,
        points,
        a,
        items,
        b,
        k: k_node,
        sids,
        trend,
        c,
        scales,
        d,
        pext,
        sext,
        e,
        f,
        iext,
        g,
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn join(f: &Fields<'_>) -> Vec<Node> {
    let mut out: Vec<Node> = f.head.to_vec();
    for x in &f.series {
        out.extend_from_slice(x);
    }
    if let Some(x) = f.exseries {
        out.extend_from_slice(x);
    }
    out.extend_from_slice(f.points_head);
    for x in &f.points {
        out.extend_from_slice(x);
    }
    out.extend_from_slice(f.a);
    for x in &f.items {
        out.extend_from_slice(x);
    }
    out.extend_from_slice(f.b);
    out.push(f.k.clone());
    for part in [
        f.sids, f.trend, f.c, f.scales, f.d, f.pext, f.sext, f.e, f.f, f.iext, f.g,
    ] {
        out.extend_from_slice(part);
    }
    out
}
