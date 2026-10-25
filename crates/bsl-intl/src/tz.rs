use crate::data::{TzData, TzZone, decode_tz};
use crate::error::{LibError, msg};
use crate::locale::Locale;
use base64::Engine as _;
use bsl_prim::BslDate;
use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::{AmbiguousOffset, TimeZone};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

static TZ_BIN: &[u8] = include_bytes!("../data/tz.bin");

const UNIX_EPOCH_SECS: i64 = 62_135_596_800;
const MAX_ALIAS_STEPS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneOffsets {
    pub total: i32,
    pub std: i32,
    pub dst: i32,
}

struct Slot {
    zone: TzZone,
    tz: OnceLock<TimeZone>,
}

impl Slot {
    fn tz(&self, name: &str) -> &TimeZone {
        self.tz
            .get_or_init(|| self.build(name).unwrap_or(TimeZone::UTC))
    }

    fn build(&self, name: &str) -> Option<TimeZone> {
        match &self.zone {
            TzZone::Tzif { tzif_b64, .. } => base64::engine::general_purpose::STANDARD
                .decode(tzif_b64)
                .ok()
                .and_then(|b| TimeZone::tzif(name, &b).ok()),
            TzZone::Posix(p) => TimeZone::posix(p).ok(),
            TzZone::Alias(_) => None,
        }
    }

    fn std_periods(&self) -> &[(i64, i32)] {
        match &self.zone {
            TzZone::Tzif { std, .. } => std,
            _ => &[],
        }
    }
}

pub struct TzDb {
    names: Vec<String>,
    name_set: BTreeSet<String>,
    slots: BTreeMap<String, Slot>,
    metazones: BTreeMap<String, Vec<crate::data::MetazoneSpan>>,
}

pub fn tzdb() -> &'static TzDb {
    static DB: OnceLock<TzDb> = OnceLock::new();
    DB.get_or_init(|| TzDb::new(decode_tz(TZ_BIN).ok()))
}

impl TzDb {
    fn new(data: Option<TzData>) -> TzDb {
        let data = data.unwrap_or_else(|| TzData {
            tzdata: String::new(),
            names: Vec::new(),
            zones: BTreeMap::new(),
            metazones: BTreeMap::new(),
        });
        TzDb {
            name_set: data.names.iter().cloned().collect(),
            names: data.names,
            slots: data
                .zones
                .into_iter()
                .map(|(k, zone)| {
                    (
                        k,
                        Slot {
                            zone,
                            tz: OnceLock::new(),
                        },
                    )
                })
                .collect(),
            metazones: data.metazones,
        }
    }

    pub fn valid_names(&self) -> &[String] {
        &self.names
    }

    pub fn zone(&self, name: &[u16]) -> Result<Zone<'_>, LibError> {
        let bad = || {
            let suffix = if name.is_empty() {
                String::new()
            } else {
                format!(" '{}'", String::from_utf16_lossy(name))
            };
            LibError::new(msg::BAD_TZ).with_suffix(suffix)
        };
        let text = String::from_utf16(name).map_err(|_| bad())?;
        if self.name_set.contains(&text) {
            let mut key: &str = self.slots.get_key_value(text.as_str()).ok_or_else(bad)?.0;
            for _ in 0..MAX_ALIAS_STEPS {
                let (k, slot) = self.slots.get_key_value(key).ok_or_else(bad)?;
                match &slot.zone {
                    TzZone::Alias(target) => key = target,
                    _ => {
                        let kind = Kind::Db { slot, key: k };
                        return Ok(Zone {
                            id: text,
                            db: self,
                            kind,
                        });
                    }
                }
            }
            return Err(bad());
        }
        match parse_custom(&text) {
            Some(custom) => Ok(Zone {
                id: custom.id(),
                db: self,
                kind: Kind::Fixed(custom.seconds()),
            }),
            None => Err(bad()),
        }
    }
}

struct Custom {
    negative: bool,
    hours: i32,
    minutes: i32,
}

impl Custom {
    fn seconds(&self) -> i32 {
        let seconds = self.hours * 3600 + self.minutes * 60;
        if self.negative { -seconds } else { seconds }
    }

    fn id(&self) -> String {
        let sign = if self.negative { '-' } else { '+' };
        format!("GMT{sign}{:02}:{:02}", self.hours, self.minutes)
    }
}

fn parse_custom(s: &str) -> Option<Custom> {
    let rest = s.strip_prefix("GMT")?;
    let (negative, rest) = match rest.as_bytes().first()? {
        b'+' => (false, &rest[1..]),
        b'-' => (true, &rest[1..]),
        _ => return None,
    };
    let digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit()) && !t.is_empty();
    let (hours, minutes) = if let Some((h, m)) = rest.split_once(':') {
        if !digits(h) || h.len() > 2 || !digits(m) || m.len() != 2 {
            return None;
        }
        (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?)
    } else {
        if !digits(rest) || rest.len() > 2 {
            return None;
        }
        (rest.parse::<i32>().ok()?, 0)
    };
    (hours <= 23 && minutes <= 59).then_some(Custom {
        negative,
        hours,
        minutes,
    })
}

enum Kind<'a> {
    Fixed(i32),
    Db { slot: &'a Slot, key: &'a str },
}

pub struct Zone<'a> {
    id: String,
    db: &'a TzDb,
    kind: Kind<'a>,
}

fn unix_secs(d: BslDate) -> i64 {
    d.whole_seconds() - UNIX_EPOCH_SECS
}

fn shift(d: BslDate, secs: i64) -> BslDate {
    let t = d
        .ticks()
        .saturating_add(secs.saturating_mul(BslDate::TICKS_PER_SECOND));
    BslDate::from_ticks(t.clamp(0, BslDate::MAX.ticks())).unwrap_or(BslDate::EMPTY)
}

impl Zone<'_> {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn offsets(&self, utc: BslDate) -> ZoneOffsets {
        match &self.kind {
            Kind::Fixed(o) => ZoneOffsets {
                total: *o,
                std: *o,
                dst: 0,
            },
            Kind::Db { slot, key } => {
                let t =
                    unix_secs(utc).clamp(Timestamp::MIN.as_second(), Timestamp::MAX.as_second());
                let ts = Timestamp::from_second(t).unwrap_or(Timestamp::UNIX_EPOCH);
                let total = slot.tz(key).to_offset(ts).seconds();
                let std = match slot.std_periods() {
                    [] => total,
                    p => p[p.partition_point(|(until, _)| *until <= t).min(p.len() - 1)].1,
                };
                ZoneOffsets {
                    total,
                    std,
                    dst: total - std,
                }
            }
        }
    }

    pub fn to_local(&self, utc: BslDate) -> BslDate {
        shift(utc, i64::from(self.offsets(utc).total))
    }

    pub fn to_universal(&self, local: BslDate) -> BslDate {
        let off = match &self.kind {
            Kind::Fixed(o) => *o,
            Kind::Db { slot, key } => {
                let p = local.parts();
                let dt = DateTime::new(
                    p.year as i16,
                    p.month as i8,
                    p.day as i8,
                    p.hour as i8,
                    p.minute as i8,
                    p.second as i8,
                    0,
                );
                let Ok(dt) = dt else { return local };
                match slot.tz(key).to_ambiguous_timestamp(dt).offset() {
                    AmbiguousOffset::Unambiguous { offset } => offset.seconds(),
                    AmbiguousOffset::Fold { after, .. } => after.seconds(),
                    AmbiguousOffset::Gap { before, .. } => before.seconds(),
                }
            }
        };
        shift(local, -i64::from(off))
    }

    pub fn metazone(&self, utc: BslDate) -> Option<&str> {
        let spans = self.db.metazones.get(&self.id)?;
        let t = unix_secs(utc);
        spans
            .iter()
            .find(|s| s.from <= t && t < s.to)
            .map(|s| s.metazone.as_str())
    }
}

pub fn zone_presentation(zone: &Zone<'_>, at_utc: BslDate, loc: &Locale) -> String {
    if let Some(name) = zone
        .metazone(at_utc)
        .and_then(|m| loc.data().zone_names.get(m))
    {
        return name.clone();
    }
    let std = zone.offsets(at_utc).std;
    let (sign, abs) = if std < 0 { ('-', -std) } else { ('+', std) };
    format!("GMT{sign}{:02}:{:02}", abs / 3600, abs % 3600 / 60)
}
