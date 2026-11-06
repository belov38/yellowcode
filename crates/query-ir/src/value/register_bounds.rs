use crate::QValue;
use bsl_prim::BslDate;
use std::cmp::Ordering;

pub type Rec = (u32, [u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Point {
    Date(BslDate),
    Moment(BslDate, Rec),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bnd {
    None,
    Date(BslDate),
    Moment(BslDate, Rec),
    Incl(Point),
    Excl(Point),
}

fn point_of(v: &QValue) -> Option<Point> {
    match v {
        QValue::Date(d) if *d != BslDate::EMPTY => Some(Point::Date(*d)),
        QValue::PointInTime(m) if m.date != BslDate::EMPTY => Some(match m.reference {
            Some(r) => Point::Moment(m.date, r),
            None => Point::Date(m.date),
        }),
        _ => None,
    }
}

pub fn bound_of(v: Option<&QValue>) -> Bnd {
    match v {
        None => Bnd::None,
        Some(QValue::Boundary(b)) => match point_of(&b.point) {
            Some(p) if b.include => Bnd::Incl(p),
            Some(p) => Bnd::Excl(p),
            None => Bnd::None,
        },
        Some(v) => match point_of(v) {
            Some(Point::Date(d)) => Bnd::Date(d),
            Some(Point::Moment(d, r)) => Bnd::Moment(d, r),
            None => Bnd::None,
        },
    }
}

fn cmp_pos(t: BslDate, r: Rec, p: Point, date_side: Ordering) -> Ordering {
    match p {
        Point::Date(d) => t.ticks().cmp(&d.ticks()).then(date_side.reverse()),
        Point::Moment(d, pr) => t.ticks().cmp(&d.ticks()).then_with(|| r.cmp(&pr)),
    }
}

pub fn in_balance(t: BslDate, r: Rec, b: Bnd) -> bool {
    match b {
        Bnd::None => true,
        Bnd::Date(d) => t.ticks() < d.ticks(),
        Bnd::Moment(d, pr) => cmp_pos(t, r, Point::Moment(d, pr), Ordering::Less) == Ordering::Less,
        Bnd::Incl(p) => cmp_pos(t, r, p, Ordering::Greater) != Ordering::Greater,
        Bnd::Excl(p) => cmp_pos(t, r, p, Ordering::Less) == Ordering::Less,
    }
}

pub fn after_start(t: BslDate, r: Rec, b: Bnd) -> bool {
    match b {
        Bnd::None => true,
        Bnd::Date(d) => t.ticks() >= d.ticks(),
        Bnd::Moment(d, pr) => cmp_pos(t, r, Point::Moment(d, pr), Ordering::Less) != Ordering::Less,
        Bnd::Incl(p) => cmp_pos(t, r, p, Ordering::Less) != Ordering::Less,
        Bnd::Excl(p) => cmp_pos(t, r, p, Ordering::Greater) == Ordering::Greater,
    }
}

pub fn before_end(t: BslDate, r: Rec, b: Bnd) -> bool {
    match b {
        Bnd::None => true,
        Bnd::Date(d) => t.ticks() <= d.ticks(),
        Bnd::Moment(d, pr) => {
            cmp_pos(t, r, Point::Moment(d, pr), Ordering::Greater) != Ordering::Greater
        }
        Bnd::Incl(p) => cmp_pos(t, r, p, Ordering::Greater) != Ordering::Greater,
        Bnd::Excl(p) => cmp_pos(t, r, p, Ordering::Less) == Ordering::Less,
    }
}
