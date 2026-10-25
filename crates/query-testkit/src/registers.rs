use std::collections::BTreeMap;

pub type Secs = i64;

const DAY: Secs = 86_400;

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_before_year(y: i64) -> i64 {
    let p = y - 1;
    p * 365 + p / 4 - p / 100 + p / 400
}

fn month_days(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(y) => 29,
        _ => 28,
    }
}

pub fn secs(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Secs {
    let mut days = days_before_year(y);
    for m in 1..mo {
        days += month_days(y, m);
    }
    days += d - 1;
    days * DAY + h * 3600 + mi * 60 + s
}

pub fn ymd(t: Secs) -> (i64, i64, i64) {
    let mut days = t.div_euclid(DAY);
    let mut y = days / 366 + 1;
    while days_before_year(y + 1) <= days {
        y += 1;
    }
    days -= days_before_year(y);
    let mut m = 1;
    while days >= month_days(y, m) {
        days -= month_days(y, m);
        m += 1;
    }
    (y, m, days + 1)
}

pub fn current_totals_period() -> Secs {
    secs(3999, 11, 1, 0, 0, 0)
}

pub fn month_start(t: Secs) -> Secs {
    let (y, m, _) = ymd(t);
    secs(y, m, 1, 0, 0, 0)
}

pub fn next_month(t: Secs) -> Secs {
    let (y, m, _) = ymd(t);
    if m == 12 {
        secs(y + 1, 1, 1, 0, 0, 0)
    } else {
        secs(y, m + 1, 1, 0, 0, 0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Periodicity {
    None,
    Second,
    Minute,
    Hour,
    Day,
    Week { sunday: bool },
    Decade,
    Month,
    Quarter,
    HalfYear,
    Year,
    Recorder,
    Record,
}

pub fn period_start(t: Secs, p: Periodicity) -> Secs {
    let (y, m, d) = ymd(t);
    match p {
        Periodicity::None | Periodicity::Second | Periodicity::Recorder | Periodicity::Record => t,
        Periodicity::Minute => t - t.rem_euclid(60),
        Periodicity::Hour => t - t.rem_euclid(3600),
        Periodicity::Day => t - t.rem_euclid(DAY),
        Periodicity::Week { sunday } => {
            let day = t.div_euclid(DAY);
            let wd = day.rem_euclid(7);
            let back = if sunday { (wd + 1) % 7 } else { wd };
            (day - back) * DAY
        }
        Periodicity::Decade => secs(
            y,
            m,
            if d <= 10 {
                1
            } else if d <= 20 {
                11
            } else {
                21
            },
            0,
            0,
            0,
        ),
        Periodicity::Month => secs(y, m, 1, 0, 0, 0),
        Periodicity::Quarter => secs(y, 3 * ((m - 1) / 3) + 1, 1, 0, 0, 0),
        Periodicity::HalfYear => secs(y, if m <= 6 { 1 } else { 7 }, 1, 0, 0, 0),
        Periodicity::Year => secs(y, 1, 1, 0, 0, 0),
    }
}

pub type Recorder = (u32, [u8; 16]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Movement {
    pub period: Secs,
    pub recorder: Recorder,
    pub line: u32,
    pub active: bool,
    pub receipt: bool,
    pub dims: Vec<u32>,
    pub res: Vec<i128>,
}

impl Movement {
    pub fn position(&self) -> Position {
        Position {
            t: self.period,
            r: Some(self.recorder),
        }
    }

    fn signed(&self) -> Vec<i128> {
        self.res
            .iter()
            .map(|v| if self.receipt { *v } else { -*v })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub t: Secs,
    pub r: Option<Recorder>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    None,
    Date(Secs),
    Moment(Secs, Recorder),
    Incl(Position),
    Excl(Position),
}

fn cmp_pos(m: &Movement, p: Position, date_side: std::cmp::Ordering) -> std::cmp::Ordering {
    m.period.cmp(&p.t).then_with(|| match p.r {
        Some(r) => m.recorder.cmp(&r),
        None => date_side.reverse(),
    })
}

use std::cmp::Ordering::{Greater, Less};

pub fn in_balance(m: &Movement, b: Bound) -> bool {
    match b {
        Bound::None => true,
        Bound::Date(d) => m.period < d,
        Bound::Moment(d, r) => cmp_pos(m, Position { t: d, r: Some(r) }, Less) == Less,
        Bound::Incl(p) => cmp_pos(m, p, Greater) != Greater,
        Bound::Excl(p) => cmp_pos(m, p, Less) == Less,
    }
}

pub fn after_start(m: &Movement, b: Bound) -> bool {
    match b {
        Bound::None => true,
        Bound::Date(d) => m.period >= d,
        Bound::Moment(d, r) => cmp_pos(m, Position { t: d, r: Some(r) }, Less) != Less,
        Bound::Incl(p) => cmp_pos(m, p, Less) != Less,
        Bound::Excl(p) => cmp_pos(m, p, Greater) == Greater,
    }
}

pub fn before_end(m: &Movement, b: Bound) -> bool {
    match b {
        Bound::None => true,
        Bound::Date(d) => m.period <= d,
        Bound::Moment(d, r) => cmp_pos(m, Position { t: d, r: Some(r) }, Greater) != Greater,
        Bound::Incl(p) => cmp_pos(m, p, Greater) != Greater,
        Bound::Excl(p) => cmp_pos(m, p, Less) == Less,
    }
}

pub fn bound_date(b: Bound) -> Option<Secs> {
    match b {
        Bound::None => None,
        Bound::Date(d) | Bound::Moment(d, _) => Some(d),
        Bound::Incl(p) | Bound::Excl(p) => Some(p.t),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TotalsState {
    pub current: bool,
    pub calculated: Option<Secs>,
    pub min_period: Secs,
    pub split: bool,
    pub agg_mode: bool,
}

impl Default for TotalsState {
    fn default() -> TotalsState {
        TotalsState {
            current: true,
            calculated: None,
            min_period: current_totals_period(),
            split: false,
            agg_mode: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TotalsRow {
    pub period: Secs,
    pub dims: Vec<u32>,
    pub res: Vec<i128>,
    pub splitter: u32,
}

fn add(acc: &mut [i128], v: &[i128]) {
    for (a, x) in acc.iter_mut().zip(v) {
        *a += x;
    }
}

fn push_split(out: &mut Vec<TotalsRow>, period: Secs, dims: Vec<u32>, res: Vec<i128>, split: bool) {
    if split {
        let half: Vec<i128> = res.iter().map(|v| v / 2).collect();
        let rest: Vec<i128> = res.iter().zip(&half).map(|(v, h)| v - h).collect();
        out.push(TotalsRow {
            period,
            dims: dims.clone(),
            res: half,
            splitter: 0,
        });
        out.push(TotalsRow {
            period,
            dims,
            res: rest,
            splitter: 1,
        });
    } else {
        out.push(TotalsRow {
            period,
            dims,
            res,
            splitter: 0,
        });
    }
}

pub fn write_totals(movements: &[Movement], n_res: usize, st: &TotalsState) -> Vec<TotalsRow> {
    let mut by: BTreeMap<Vec<u32>, Vec<&Movement>> = BTreeMap::new();
    for m in movements.iter().filter(|m| m.active) {
        by.entry(m.dims.clone()).or_default().push(m);
    }
    let mut out = Vec::new();
    for (dims, ms) in &by {
        if st.current {
            let mut acc = vec![0i128; n_res];
            for m in ms {
                add(&mut acc, &m.signed());
            }
            push_split(
                &mut out,
                current_totals_period(),
                dims.clone(),
                acc,
                st.split,
            );
        }
        let Some(calc) = st.calculated else { continue };
        let first = ms.iter().map(|m| m.period).min().unwrap_or(calc);
        let mut p = next_month(first);
        while p <= calc {
            let mut acc = vec![0i128; n_res];
            for m in ms.iter().filter(|m| m.period < p) {
                add(&mut acc, &m.signed());
            }
            if p >= st.min_period {
                acc.iter_mut().for_each(|v| *v += 1);
            }
            push_split(&mut out, p, dims.clone(), acc, st.split);
            p = next_month(p);
        }
    }
    out
}

pub fn write_turnovers(
    movements: &[Movement],
    n_res: usize,
    in_totals: &[bool],
    st: &TotalsState,
) -> Vec<TotalsRow> {
    if st.agg_mode {
        return Vec::new();
    }
    let mut by: BTreeMap<(Secs, Vec<u32>), Vec<i128>> = BTreeMap::new();
    for m in movements.iter().filter(|m| m.active) {
        let dims: Vec<u32> = m
            .dims
            .iter()
            .zip(in_totals)
            .filter(|(_, keep)| **keep)
            .map(|(d, _)| *d)
            .collect();
        let acc = by
            .entry((month_start(m.period), dims))
            .or_insert_with(|| vec![0; n_res]);
        add(acc, &m.signed());
    }
    let mut out = Vec::new();
    for ((p, dims), res) in by {
        push_split(&mut out, p, dims, res, st.split);
    }
    out
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Comp {
    pub b: i128,
    pub p: i128,
    pub r: i128,
}

impl Comp {
    pub fn turnover(&self) -> i128 {
        self.p - self.r
    }
    pub fn closing(&self) -> i128 {
        self.b + self.turnover()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub dims: Vec<usize>,
    pub res: Vec<(usize, Want)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Balance,
    Receipt,
    Expense,
    Turnover,
    Opening,
    Closing,
}

pub type PeriodKey = (Secs, Option<Recorder>, u32);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Out {
    pub period: Option<PeriodKey>,
    pub dims: Vec<u32>,
    pub res: Vec<(i128, i128, i128)>,
}

fn row_of(period: Option<PeriodKey>, dims: Vec<u32>, c: &[Comp]) -> Out {
    Out {
        period,
        dims,
        res: c.iter().map(|x| (x.b, x.p, x.r)).collect(),
    }
}

fn keep(c: &[Comp], sel: &Selection, kind: Kind) -> bool {
    if sel.res.is_empty() {
        return match kind {
            Kind::Balance => c.iter().any(|x| x.b != 0),
            Kind::Turnovers => c.iter().any(|x| x.p != 0 || x.r != 0),
            Kind::BalanceAndTurnovers => c.iter().any(|x| x.b != 0 || x.p != 0 || x.r != 0),
        };
    }
    sel.res.iter().any(|(i, w)| {
        let Some(x) = c.get(*i) else { return false };
        match w {
            Want::Balance => x.b != 0,
            Want::Receipt => x.p != 0,
            Want::Expense => x.r != 0,
            Want::Turnover => x.turnover() != 0,
            Want::Opening => x.b != 0,
            Want::Closing => x.b != 0 || x.turnover() != 0,
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Balance,
    Turnovers,
    BalanceAndTurnovers,
}

fn pick(dims: &[u32], sel: &Selection) -> Vec<u32> {
    sel.dims
        .iter()
        .filter_map(|i| dims.get(*i).copied())
        .collect()
}

pub fn balance(
    movements: &[Movement],
    n_res: usize,
    b: Bound,
    cond: &dyn Fn(&[u32]) -> bool,
    sel: &Selection,
) -> Vec<Out> {
    let mut by: BTreeMap<Vec<u32>, Vec<Comp>> = BTreeMap::new();
    for m in movements
        .iter()
        .filter(|m| m.active && in_balance(m, b) && cond(&m.dims))
    {
        let acc = by
            .entry(pick(&m.dims, sel))
            .or_insert_with(|| vec![Comp::default(); n_res]);
        for (a, v) in acc.iter_mut().zip(m.signed()) {
            a.b += v;
        }
    }
    if sel.dims.is_empty() {
        let c = by
            .remove(&Vec::new())
            .unwrap_or_else(|| vec![Comp::default(); n_res]);
        return vec![row_of(None, Vec::new(), &c)];
    }
    by.into_iter()
        .filter(|(_, c)| keep(c, sel, Kind::Balance))
        .map(|(d, c)| row_of(None, d, &c))
        .collect()
}

pub fn turnovers(
    movements: &[Movement],
    n_res: usize,
    start: Bound,
    end: Bound,
    p: Periodicity,
    cond: &dyn Fn(&[u32]) -> bool,
    sel: &Selection,
) -> Vec<Out> {
    let cut = bound_date(start);
    let mut by: BTreeMap<(Option<PeriodKey>, Vec<u32>), Vec<Comp>> = BTreeMap::new();
    for m in movements
        .iter()
        .filter(|m| m.active && after_start(m, start) && before_end(m, end) && cond(&m.dims))
    {
        let period = match p {
            Periodicity::None => None,
            Periodicity::Recorder => Some((m.period, Some(m.recorder), 0)),
            Periodicity::Record => Some((m.period, Some(m.recorder), m.line)),
            _ => {
                let s = period_start(m.period, p);
                Some((cut.map_or(s, |c| s.max(c)), None, 0))
            }
        };
        let acc = by
            .entry((period, pick(&m.dims, sel)))
            .or_insert_with(|| vec![Comp::default(); n_res]);
        for (a, v) in acc.iter_mut().zip(&m.res) {
            if m.receipt {
                a.p += v;
            } else {
                a.r += v;
            }
        }
    }
    by.into_iter()
        .filter(|(_, c)| keep(c, sel, Kind::Turnovers))
        .map(|((per, d), c)| row_of(per, d, &c))
        .collect()
}

pub fn balance_and_turnovers(
    movements: &[Movement],
    n_res: usize,
    start: Bound,
    end: Bound,
    cond: &dyn Fn(&[u32]) -> bool,
    sel: &Selection,
) -> Vec<Out> {
    let mut by: BTreeMap<Vec<u32>, Vec<Comp>> = BTreeMap::new();
    for m in movements.iter().filter(|m| m.active && cond(&m.dims)) {
        let inside = after_start(m, start) && before_end(m, end);
        let before = !after_start(m, start);
        if !inside && !before {
            continue;
        }
        let acc = by
            .entry(pick(&m.dims, sel))
            .or_insert_with(|| vec![Comp::default(); n_res]);
        for (a, v) in acc.iter_mut().zip(&m.res) {
            let s = if m.receipt { *v } else { -*v };
            if before {
                a.b += s;
            } else if m.receipt {
                a.p += v;
            } else {
                a.r += v;
            }
        }
    }
    if sel.dims.is_empty() {
        let c = by
            .remove(&Vec::new())
            .unwrap_or_else(|| vec![Comp::default(); n_res]);
        return vec![row_of(None, Vec::new(), &c)];
    }
    by.into_iter()
        .filter(|(_, c)| keep(c, sel, Kind::BalanceAndTurnovers))
        .map(|(d, c)| row_of(None, d, &c))
        .collect()
}
