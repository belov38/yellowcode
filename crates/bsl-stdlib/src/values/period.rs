use crate::bind::{msg, type_mismatch};
use crate::collections::{ctor_not_implemented, not_implemented, property_name, property_ru};
use crate::enums::{as_enum_value, type_by_id};
use crate::serial::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::ui::{enum_val, model_names, model_ordinal};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData};
use brace::Node;
use bsl_lib::xdto::QName;
use bsl_prim::{BslDate, boost_hash};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef, SharedObject,
    SharedValue, TypeId, Value,
};
use platform_catalog::TypeIdx;
use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

const NS_CORE: &str = "http://v8.1c.ru/8.1/data/core";

struct Types {
    period: TypeIdx,
    begin: TypeIdx,
    period_variant: TypeIdx,
    begin_variant: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
fn types() -> &'static Types {
    static T: OnceLock<Types> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        Types {
            period: t("StandardPeriod"),
            begin: t("StandardBeginningDate"),
            period_variant: t("StandardPeriodVariant"),
            begin_variant: t("StandardBeginningDateVariant"),
        }
    })
}

fn core(name: &str) -> QName {
    QName::new(NS_CORE, name)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn days_of(d: BslDate) -> i64 {
    let p = d.parts();
    days_from_civil(i64::from(p.year), p.month, p.day)
}

fn day_start(z: i64) -> BslDate {
    let (y, m, d) = civil_from_days(z);
    u32::try_from(y)
        .ok()
        .and_then(|y| BslDate::from_parts(y, m, d, 0, 0, 0))
        .unwrap_or(BslDate::EMPTY)
}

fn day_end(z: i64) -> BslDate {
    let (y, m, d) = civil_from_days(z);
    u32::try_from(y)
        .ok()
        .and_then(|y| BslDate::from_parts(y, m, d, 23, 59, 59))
        .unwrap_or(BslDate::EMPTY)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (days_from_civil(ny, nm, 1) - days_from_civil(y, m, 1)) as u32
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Day,
    Week,
    TenDays,
    Month,
    Quarter,
    HalfYear,
    Year,
}

impl Unit {
    fn parse(s: &str) -> Option<Unit> {
        Some(match s {
            "Day" => Unit::Day,
            "Week" => Unit::Week,
            "TenDays" => Unit::TenDays,
            "Month" => Unit::Month,
            "Quarter" => Unit::Quarter,
            "HalfYear" => Unit::HalfYear,
            "Year" => Unit::Year,
            _ => return None,
        })
    }

    fn months(self) -> Option<i64> {
        match self {
            Unit::Month => Some(1),
            Unit::Quarter => Some(3),
            Unit::HalfYear => Some(6),
            Unit::Year => Some(12),
            _ => None,
        }
    }
}

fn unit_start(u: Unit, z: i64) -> i64 {
    let (y, m, d) = civil_from_days(z);
    match u {
        Unit::Day => z,
        Unit::Week => z - (z + 3).rem_euclid(7),
        Unit::TenDays => days_from_civil(
            y,
            m,
            if d <= 10 {
                1
            } else if d <= 20 {
                11
            } else {
                21
            },
        ),
        Unit::Month => days_from_civil(y, m, 1),
        Unit::Quarter => days_from_civil(y, ((m - 1) / 3) * 3 + 1, 1),
        Unit::HalfYear => days_from_civil(y, ((m - 1) / 6) * 6 + 1, 1),
        Unit::Year => days_from_civil(y, 1, 1),
    }
}

fn next_start(u: Unit, start: i64) -> i64 {
    let (y, m, d) = civil_from_days(start);
    match u {
        Unit::Day => start + 1,
        Unit::Week => start + 7,
        Unit::TenDays => match d {
            1 => start + 10,
            11 => start + 10,
            _ => add_months_first(y, m, 1),
        },
        _ => add_months_first(y, m, u.months().unwrap_or(1)),
    }
}

fn add_months_first(y: i64, m: u32, delta: i64) -> i64 {
    let idx = y * 12 + i64::from(m) - 1 + delta;
    days_from_civil(idx.div_euclid(12), (idx.rem_euclid(12) + 1) as u32, 1)
}

fn unit_end(u: Unit, z: i64) -> i64 {
    next_start(u, unit_start(u, z)) - 1
}

fn prev_start(u: Unit, z: i64) -> i64 {
    unit_start(u, unit_start(u, z) - 1)
}

fn following_start(u: Unit, z: i64) -> i64 {
    next_start(u, unit_start(u, z))
}

fn shift_months(z: i64, delta: i64) -> i64 {
    let (y, m, d) = civil_from_days(z);
    let first = add_months_first(y, m, delta);
    let (ny, nm, _) = civil_from_days(first);
    first + i64::from(d.min(days_in_month(ny, nm))) - 1
}

fn period_days(name: &str, today: i64) -> Option<(i64, i64)> {
    let z = today;
    match name {
        "Today" => return Some((z, z)),
        "Yesterday" => return Some((z - 1, z - 1)),
        "Tomorrow" => return Some((z + 1, z + 1)),
        "Last7Days" => return Some((z - 6, z)),
        "Next7Days" => return Some((z, z + 6)),
        "Month" => return Some((shift_months(z, -1) + 1, z)),
        _ => {}
    }
    if let Some(u) = name
        .strip_prefix("FromBeginningOfThis")
        .and_then(Unit::parse)
    {
        return Some((unit_start(u, z), z));
    }
    if let Some(u) = name.strip_prefix("TillEndOfThis").and_then(Unit::parse) {
        return Some((z, unit_end(u, z)));
    }
    if let Some(u) = name.strip_prefix("This").and_then(Unit::parse) {
        return Some((unit_start(u, z), unit_end(u, z)));
    }
    for (prefix, sign) in [("Last", -1), ("Next", 1)] {
        let Some(rest) = name.strip_prefix(prefix) else {
            continue;
        };
        let (unit, till) = match ["TillSameWeekDay", "TillSameDayNumber", "TillSameDate"]
            .iter()
            .find_map(|s| rest.strip_suffix(s))
        {
            Some(u) => (Unit::parse(u)?, true),
            None => (Unit::parse(rest)?, false),
        };
        let start = if sign < 0 {
            prev_start(unit, z)
        } else {
            following_start(unit, z)
        };
        if !till {
            return Some((start, unit_end(unit, start)));
        }
        let end = match unit.months() {
            Some(m) => shift_months(z, sign * m),
            None => (start + (z - unit_start(unit, z))).min(unit_end(unit, start)),
        };
        return Some((start, end));
    }
    None
}

fn begin_day(name: &str, today: i64) -> Option<i64> {
    if let Some(u) = name.strip_prefix("BeginningOfThis").and_then(Unit::parse) {
        return Some(unit_start(u, today));
    }
    if let Some(u) = name.strip_prefix("BeginningOfLast").and_then(Unit::parse) {
        return Some(prev_start(u, today));
    }
    name.strip_prefix("BeginningOfNext")
        .and_then(Unit::parse)
        .map(|u| following_start(u, today))
}

fn today(ex: &Execution) -> i64 {
    let now = ex.env().now_utc().trunc_seconds();
    days_of(crate::globals::tz::utc_to_session(ex, now))
}

fn end_of_period(d: BslDate) -> BslDate {
    let p = d.parts();
    if d != BslDate::EMPTY && p.hour == 0 && p.minute == 0 && p.second == 0 {
        BslDate::from_parts(p.year, p.month, p.day, 23, 59, 59).unwrap_or(d)
    } else {
        d
    }
}

fn date_atom(d: BslDate) -> Node {
    let p = d.parts();
    Node::atom(format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        p.year, p.month, p.day, p.hour, p.minute, p.second
    ))
}

fn read_date(n: &Node) -> Option<BslDate> {
    let a = n.as_atom()?;
    if !matches!(a.len(), 8 | 14) || !a.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if a.bytes().all(|b| b == b'0') {
        return Some(BslDate::EMPTY);
    }
    let num = |r: std::ops::Range<usize>| a.get(r)?.parse::<u32>().ok();
    let (h, mi, s) = if a.len() == 14 {
        (num(8..10)?, num(10..12)?, num(12..14)?)
    } else {
        (0, 0, 0)
    };
    BslDate::from_parts(num(0..4)?, num(4..6)?, num(6..8)?, h, mi, s)
}

fn date_arg(v: &Value, param: usize) -> CallResult<BslDate> {
    match v {
        Value::Date(d) => Ok(*d),
        Value::Undefined => Ok(BslDate::EMPTY),
        _ => Err(type_mismatch(param).into()),
    }
}

fn variant_arg(v: &Value, t: TypeIdx, q: &QName) -> Option<usize> {
    let e = as_enum_value(v).filter(|e| e.type_idx() == t)?;
    model_ordinal(q, e.name_en())
}

fn variant_name(q: &QName, n: usize) -> &'static str {
    model_names(q).get(n).map_or("", String::as_str)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PeriodState {
    variant: usize,
    start: BslDate,
    end: BslDate,
}

pub(crate) struct StdPeriod {
    st: Cell<PeriodState>,
}

fn period_variant_type() -> QName {
    core("StandardPeriodVariant")
}

fn new_period(st: PeriodState) -> Value {
    Value::Object(Rc::new(StdPeriod { st: Cell::new(st) }))
}

pub(super) fn period_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let custom = PeriodState {
        variant: 0,
        start: BslDate::EMPTY,
        end: BslDate::EMPTY,
    };
    match args.len() {
        0 => Ok(new_period(custom)),
        1 => {
            let v = crate::bind::arg_value(ex, args.first())?;
            match variant_arg(&v, types().period_variant, &period_variant_type()) {
                Some(variant) => Ok(new_period(PeriodState { variant, ..custom })),
                None => Err(type_mismatch(1).into()),
            }
        }
        2 => {
            let a = crate::bind::arg_value(ex, args.first())?;
            let b = crate::bind::arg_value(ex, args.get(1))?;
            Ok(new_period(PeriodState {
                variant: 0,
                start: day_start(days_of(date_arg(&a, 1)?)),
                end: end_of_period(date_arg(&b, 2)?),
            }))
        }
        _ => Err(ctor_not_implemented(types().period).into()),
    }
}

impl StdPeriod {
    fn dates(&self, ex: &Execution) -> (BslDate, BslDate) {
        let st = self.st.get();
        let name = variant_name(&period_variant_type(), st.variant);
        match period_days(name, today(ex)) {
            Some((a, b)) => (day_start(a), day_end(b)),
            None => (st.start, st.end),
        }
    }

    fn variant_value(&self) -> Value {
        let st = self.st.get();
        enum_val(
            types().period_variant,
            variant_name(&period_variant_type(), st.variant),
        )
    }
}

impl HostObject for StdPeriod {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().period)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().period;
        match property_name(t, m) {
            Some("Variant") => Ok(self.variant_value()),
            Some("StartDate") => Ok(Value::Date(self.dates(ex).0)),
            Some("EndDate") => Ok(Value::Date(self.dates(ex).1)),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let t = types().period;
        let mut st = self.st.get();
        match property_name(t, m) {
            Some("Variant") => {
                let variant = variant_arg(&v, types().period_variant, &period_variant_type())
                    .ok_or_else(|| type_mismatch(1))?;
                if variant_name(&period_variant_type(), variant) == "Custom"
                    && variant_name(&period_variant_type(), st.variant) != "Custom"
                {
                    (st.start, st.end) = self.dates(ex);
                }
                st.variant = variant;
            }
            Some(p @ ("StartDate" | "EndDate")) => {
                let d = date_arg(&v, 1)?;
                let (a, b) = self.dates(ex);
                st.variant = 0;
                (st.start, st.end) = if p == "StartDate" {
                    (d, b)
                } else {
                    (a, end_of_period(d))
                };
            }
            _ => {
                return match property_ru(t, m) {
                    "" => Err(not_implemented(t, m).into()),
                    ru => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                        .with_arg(ru)
                        .raw()
                        .into()),
                };
            }
        }
        self.st.set(st);
        Ok(())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().period, m).into())
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<StdPeriod>()?;
        let (a, b) = (self.st.get(), o.st.get());
        let q = period_variant_type();
        Some(a.variant == b.variant && (!is_custom(&q, a.variant) || a == b))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(boost_hash::ordinal(self.st.get().variant as u32))
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedPeriod(self.st.get()))))
    }
}

struct SharedPeriod(PeriodState);

impl SharedObject for SharedPeriod {
    fn to_value(&self, _ex: &mut Execution) -> Result<Value, Exception> {
        Ok(new_period(self.0))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BeginState {
    variant: usize,
    date: BslDate,
}

pub(crate) struct StdBegin {
    st: Cell<BeginState>,
}

fn begin_variant_type() -> QName {
    core("StandardBeginningDateVariant")
}

fn new_begin(st: BeginState) -> Value {
    Value::Object(Rc::new(StdBegin { st: Cell::new(st) }))
}

pub(super) fn begin_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let custom = BeginState {
        variant: 0,
        date: BslDate::EMPTY,
    };
    match args.len() {
        0 => Ok(new_begin(custom)),
        1 => {
            let v = crate::bind::arg_value(ex, args.first())?;
            if let Some(variant) = variant_arg(&v, types().begin_variant, &begin_variant_type()) {
                return Ok(new_begin(BeginState { variant, ..custom }));
            }
            Ok(new_begin(BeginState {
                variant: 0,
                date: date_arg(&v, 1)?,
            }))
        }
        _ => Err(ctor_not_implemented(types().begin).into()),
    }
}

impl StdBegin {
    fn date(&self, ex: &Execution) -> BslDate {
        let st = self.st.get();
        let name = variant_name(&begin_variant_type(), st.variant);
        match begin_day(name, today(ex)) {
            Some(z) => day_start(z),
            None => st.date,
        }
    }
}

impl HostObject for StdBegin {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().begin)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().begin;
        match property_name(t, m) {
            Some("Variant") => Ok(enum_val(
                types().begin_variant,
                variant_name(&begin_variant_type(), self.st.get().variant),
            )),
            Some("Date") => Ok(Value::Date(self.date(ex))),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let t = types().begin;
        let mut st = self.st.get();
        match property_name(t, m) {
            Some("Variant") => {
                st.variant = variant_arg(&v, types().begin_variant, &begin_variant_type())
                    .ok_or_else(|| type_mismatch(1))?;
            }
            Some("Date") => {
                st.variant = 0;
                st.date = date_arg(&v, 1)?;
            }
            _ => return Err(not_implemented(t, m).into()),
        }
        self.st.set(st);
        Ok(())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().begin, m).into())
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<StdBegin>()?;
        let (a, b) = (self.st.get(), o.st.get());
        let q = begin_variant_type();
        Some(a.variant == b.variant && (!is_custom(&q, a.variant) || a == b))
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(boost_hash::ordinal(self.st.get().variant as u32))
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedBegin(self.st.get()))))
    }
}

struct SharedBegin(BeginState);

impl SharedObject for SharedBegin {
    fn to_value(&self, _ex: &mut Execution) -> Result<Value, Exception> {
        Ok(new_begin(self.0))
    }
}

pub(super) struct PeriodCodec;
pub(super) struct BeginCodec;

fn in_model(q: &QName, n: usize) -> Option<usize> {
    (n < model_names(q).len()).then_some(n)
}

fn is_custom(q: &QName, n: usize) -> bool {
    matches!(variant_name(q, n), "Custom" | "Month")
}

impl ValueCodec for PeriodCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(p) = v.as_any().downcast_ref::<StdPeriod>() else {
            return Ok(None);
        };
        let st = p.st.get();
        w.account(ex, 64)?;
        Ok(Some(vec![Node::List(vec![
            Node::int(st.variant as i64),
            date_atom(st.start),
            date_atom(st.end),
        ])]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let read = || -> Option<PeriodState> {
            let b = items.first()?.as_list()?;
            let variant = usize::try_from(b.first()?.as_i64()?).ok()?;
            Some(PeriodState {
                variant: in_model(&period_variant_type(), variant)?,
                start: read_date(b.get(1)?)?,
                end: read_date(b.get(2)?)?,
            })
        };
        match read() {
            Some(st) => Ok(new_period(st)),
            None => Err(r.format_error(ex)),
        }
    }
}

impl ValueCodec for BeginCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(p) = v.as_any().downcast_ref::<StdBegin>() else {
            return Ok(None);
        };
        let st = p.st.get();
        w.account(ex, 48)?;
        Ok(Some(vec![Node::List(vec![
            Node::int(st.variant as i64),
            date_atom(st.date),
        ])]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let read = || -> Option<BeginState> {
            let b = items.first()?.as_list()?;
            let variant = usize::try_from(b.first()?.as_i64()?).ok()?;
            Some(BeginState {
                variant: in_model(&begin_variant_type(), variant)?,
                date: read_date(b.get(1)?)?,
            })
        };
        match read() {
            Some(st) => Ok(new_begin(st)),
            None => Err(r.format_error(ex)),
        }
    }
}

fn date_data(name: &str, d: BslDate) -> (String, Vec<XdtoData>) {
    (
        name.to_string(),
        vec![XdtoData::Value {
            projection: None,
            ty: QName::new(crate::xdto::XS, "dateTime"),
            lexical: d.to_xml_string(),
            value: Value::Date(d),
        }],
    )
}

fn variant_data(q: QName, name: &str) -> (String, Vec<XdtoData>) {
    (
        "variant".to_string(),
        vec![XdtoData::Value {
            projection: None,
            ty: q,
            lexical: name.to_string(),
            value: Value::from(name),
        }],
    )
}

fn date_of(props: &[(String, Vec<XdtoData>)], name: &str) -> Option<BslDate> {
    match crate::ui::prop(props, name)? {
        XdtoData::Value {
            value: Value::Date(d),
            ..
        } => Some(*d),
        _ => None,
    }
}

impl XdtoCodec for PeriodCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(p) = v.as_any().downcast_ref::<StdPeriod>() else {
            return Err(s.mismatch(ex));
        };
        let st = p.st.get();
        let q = period_variant_type();
        let mut props = vec![variant_data(q.clone(), variant_name(&q, st.variant))];
        if is_custom(&q, st.variant) {
            props.push(date_data("startDate", st.start));
            props.push(date_data("endDate", st.end));
        }
        s.account(ex, 192)?;
        Ok(XdtoData::Object {
            ty: core("StandardPeriod"),
            props,
        })
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let XdtoData::Object { props, .. } = d else {
            return Err(s.lexical_error(ex, "", &core("StandardPeriod")));
        };
        let q = period_variant_type();
        let text = crate::ui::prop_text(props, "variant").unwrap_or("");
        let Some(variant) = model_ordinal(&q, text) else {
            return Err(s.lexical_error(ex, text, &q));
        };
        Ok(new_period(PeriodState {
            variant,
            start: date_of(props, "startDate").unwrap_or(BslDate::EMPTY),
            end: date_of(props, "endDate").unwrap_or(BslDate::EMPTY),
        }))
    }

    fn xdto_type(&self) -> QName {
        core("StandardPeriod")
    }
}

impl XdtoCodec for BeginCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(p) = v.as_any().downcast_ref::<StdBegin>() else {
            return Err(s.mismatch(ex));
        };
        let st = p.st.get();
        let q = begin_variant_type();
        let mut props = vec![variant_data(q.clone(), variant_name(&q, st.variant))];
        if is_custom(&q, st.variant) {
            props.push(date_data("date", st.date));
        }
        s.account(ex, 128)?;
        Ok(XdtoData::Object {
            ty: core("StandardBeginningDate"),
            props,
        })
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        let XdtoData::Object { props, .. } = d else {
            return Err(s.lexical_error(ex, "", &core("StandardBeginningDate")));
        };
        let q = begin_variant_type();
        let text = crate::ui::prop_text(props, "variant").unwrap_or("");
        let Some(variant) = model_ordinal(&q, text) else {
            return Err(s.lexical_error(ex, text, &q));
        };
        Ok(new_begin(BeginState {
            variant,
            date: date_of(props, "date").unwrap_or(BslDate::EMPTY),
        }))
    }

    fn xdto_type(&self) -> QName {
        core("StandardBeginningDate")
    }
}

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    vec![
        (types().period, Arc::new(PeriodCodec) as Arc<dyn ValueCodec>),
        (types().begin, Arc::new(BeginCodec)),
    ]
}

pub(crate) fn xdto_codecs() -> Vec<(TypeIdx, Arc<dyn XdtoCodec>)> {
    vec![
        (types().period, Arc::new(PeriodCodec) as Arc<dyn XdtoCodec>),
        (types().begin, Arc::new(BeginCodec)),
    ]
}

pub(crate) fn register(reg: &mut bsl_vm::Registry) {
    reg.type_impl(
        types().period,
        bsl_vm::TypeImpl {
            ctor: Some(Arc::new(period_ctor)),
        },
    );
    reg.type_impl(
        types().begin,
        bsl_vm::TypeImpl {
            ctor: Some(Arc::new(begin_ctor)),
        },
    );
}
