use brace::Node;

use crate::{Error, Level, Period, Uuid16};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSettings {
    pub id: Uuid16,
    pub level: u32,
    pub period: u32,
    pub tail: Vec<Node>,
    pub counter: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitPeriod {
    None,
    Year,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
}

impl SplitPeriod {
    pub fn from_code(code: u32) -> Option<SplitPeriod> {
        Some(match code {
            0 => SplitPeriod::None,
            1 => SplitPeriod::Year,
            2 => SplitPeriod::Quarter,
            3 => SplitPeriod::Month,
            4 => SplitPeriod::Week,
            5 => SplitPeriod::Day,
            6 => SplitPeriod::Hour,
            _ => return Option::None,
        })
    }

    pub fn code(self) -> u32 {
        match self {
            SplitPeriod::None => 0,
            SplitPeriod::Year => 1,
            SplitPeriod::Quarter => 2,
            SplitPeriod::Month => 3,
            SplitPeriod::Week => 4,
            SplitPeriod::Day => 5,
            SplitPeriod::Hour => 6,
        }
    }
}

pub fn level_code(levels: &[Level]) -> Option<u32> {
    let has = |l: Level| levels.contains(&l);
    match (
        has(Level::Error),
        has(Level::Warning),
        has(Level::Information),
        has(Level::Note),
    ) {
        (true, true, true, true) => Some(0),
        (true, true, true, false) => Some(1),
        (true, true, false, false) => Some(2),
        (true, false, false, false) => Some(3),
        (false, false, false, false) => Some(100),
        _ => None,
    }
}

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

fn bad(what: &str) -> Error {
    Error::Malformed(format!("log.inf: {what}"))
}

fn number(n: &Node, what: &str) -> Result<u32, Error> {
    let Node::Atom(a) = n else {
        return Err(bad(what));
    };
    let v: u32 = a.parse().map_err(|_| bad(what))?;
    if v.to_string() != *a {
        return Err(bad(what));
    }
    Ok(v)
}

impl LogSettings {
    pub fn parse(bytes: &[u8]) -> Result<LogSettings, Error> {
        let body = bytes.strip_prefix(&BOM[..]).ok_or_else(|| bad("no BOM"))?;
        let text = std::str::from_utf8(body).map_err(|_| bad("not UTF-8"))?;
        let root = brace::parse(text).map_err(|e| bad(&e.to_string()))?;
        let Node::List(items) = root else {
            return Err(bad("not a list"));
        };
        let [id, level, period, mid @ .., counter] = items.as_slice() else {
            return Err(bad("too few fields"));
        };
        if mid.len() != 3 {
            return Err(bad("field count"));
        }
        let Node::Atom(id) = id else {
            return Err(bad("id"));
        };
        let id = Uuid16::parse(id).ok_or_else(|| bad("id"))?;
        let s = LogSettings {
            id,
            level: number(level, "level")?,
            period: number(period, "period")?,
            tail: mid.to_vec(),
            counter: number(counter, "counter")?,
        };
        if s.to_bytes() != bytes {
            return Err(bad("non-canonical layout"));
        }
        Ok(s)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut items = vec![
            Node::Atom(self.id.to_string()),
            Node::Atom(self.level.to_string()),
            Node::Atom(self.period.to_string()),
        ];
        items.extend(self.tail.iter().cloned());
        items.push(Node::Atom(self.counter.to_string()));
        let mut out = BOM.to_vec();
        out.extend(brace::to_text(&Node::List(items)).bytes());
        out
    }

    pub fn levels(&self) -> Option<Vec<Level>> {
        match self.level {
            0 => Some(vec![
                Level::Information,
                Level::Error,
                Level::Warning,
                Level::Note,
            ]),
            1 => Some(vec![Level::Error, Level::Warning, Level::Information]),
            2 => Some(vec![Level::Error, Level::Warning]),
            3 => Some(vec![Level::Error]),
            100 => Some(Vec::new()),
            _ => None,
        }
    }

    pub fn split_period(&self) -> Option<SplitPeriod> {
        SplitPeriod::from_code(self.period)
    }

    pub fn split(&self) -> Option<Period> {
        match self.split_period()? {
            SplitPeriod::Day => Some(Period::Day),
            SplitPeriod::Hour => Some(Period::Hour),
            _ => None,
        }
    }

    pub fn with_period(self, code: u32) -> LogSettings {
        LogSettings {
            period: code,
            ..self
        }
    }

    pub fn changed(self, level: u32) -> LogSettings {
        LogSettings {
            level,
            counter: self.counter.saturating_add(1),
            ..self
        }
    }
}
