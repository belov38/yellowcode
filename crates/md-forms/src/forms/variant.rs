use std::fmt;

use brace::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Serialization {
    pub outer: u16,
    pub form: u16,
}

impl Serialization {
    pub(crate) fn of(entry: &Node) -> Option<Serialization> {
        let t = entry.as_list()?;
        let outer = canonical(t.first()?.as_atom()?)?;
        let form = canonical(t.get(1)?.as_list()?.first()?.as_atom()?)?;
        Some(Serialization { outer, form })
    }

    pub fn is_4_59(self) -> bool {
        self == V4_59.serialization
    }

    pub(crate) fn of_text(text: &[u8]) -> Option<Serialization> {
        let mut rest = text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(text);
        let mut number = || -> Option<u16> {
            let skip = |r: &mut &[u8]| {
                while let [c, tail @ ..] = *r
                    && c.is_ascii_whitespace()
                {
                    *r = tail;
                }
            };
            skip(&mut rest);
            rest = rest.strip_prefix(b"{")?;
            skip(&mut rest);
            let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
            let (n, tail) = rest.split_at(digits);
            let n = canonical(std::str::from_utf8(n).ok()?)?;
            rest = tail;
            skip(&mut rest);
            rest = rest.strip_prefix(b",")?;
            Some(n)
        };
        let outer = number()?;
        let form = number()?;
        Some(Serialization { outer, form })
    }

    pub(crate) fn with_text<R>(self, f: impl FnOnce(&str) -> R) -> R {
        use std::io::Write;
        let mut buf = [0u8; 11];
        let left = {
            let mut cur = &mut buf[..];
            let _written = write!(cur, "{}/{}", self.outer, self.form).is_ok();
            cur.len()
        };
        let used = buf.len().saturating_sub(left);
        f(std::str::from_utf8(buf.get(..used).unwrap_or_default()).unwrap_or_default())
    }
}

fn canonical(atom: &str) -> Option<u16> {
    let digits = !atom.is_empty() && atom.bytes().all(|c| c.is_ascii_digit());
    if !digits || (atom.len() > 1 && atom.starts_with('0')) {
        return None;
    }
    atom.parse().ok()
}

impl fmt::Display for Serialization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.outer, self.form)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub serialization: Serialization,
    pub strict: bool,
    items: &'static [(Option<&'static str>, &'static str, usize)],
    table_bag: &'static [&'static str],
    tails: &'static [usize],
    command: &'static [&'static str],
}

pub(crate) const TABLE_BAG_AT: usize = 54;
const ATTRIBUTE: &str = "9";
const PARAMETER: &str = "0";

const fn s(outer: u16, form: u16) -> Serialization {
    Serialization { outer, form }
}

const G: Option<&str> = Some("Group");
const F: Option<&str> = Some("Field");
const D: Option<&str> = Some("Decoration");
const B: Option<&str> = Some("Button");
const T: Option<&str> = Some("Table");
const N: Option<&str> = None;

pub(crate) const V4_59: Layout = Layout {
    serialization: s(4, 59),
    strict: false,
    items: &[],
    table_bag: &["73"],
    tails: &[],
    command: &[],
};

pub(crate) const V3_48: Layout = Layout {
    serialization: s(3, 48),
    strict: true,
    items: &[
        (G, "22", 30),
        (F, "34", 56),
        (D, "11", 35),
        (B, "29", 49),
        (T, "53", 97),
        (N, "22", 29),
        (N, "11", 33),
        (N, "5", 24),
    ],
    table_bag: &["53"],
    tails: &[22, 23],
    command: &["9"],
};

pub(crate) const V3_49: Layout = Layout {
    serialization: s(3, 49),
    strict: true,
    items: &[
        (G, "22", 30),
        (F, "34", 56),
        (B, "29", 49),
        (T, "54", 98),
        (N, "22", 29),
        (N, "11", 33),
        (N, "5", 24),
    ],
    table_bag: &["54"],
    tails: &[24],
    command: &["9"],
};

pub(crate) const V4_49: Layout = Layout {
    serialization: s(4, 49),
    strict: true,
    items: &[
        (G, "22", 30),
        (F, "35", 57),
        (F, "37", 59),
        (D, "12", 36),
        (B, "30", 52),
        (B, "31", 52),
        (T, "55", 99),
        (N, "22", 29),
        (N, "12", 34),
        (N, "5", 24),
    ],
    table_bag: &["55"],
    tails: &[24],
    command: &["9"],
};

pub(crate) const V4_50: Layout = Layout {
    serialization: s(4, 50),
    strict: true,
    items: &[
        (G, "22", 30),
        (F, "37", 59),
        (D, "12", 36),
        (B, "31", 52),
        (T, "55", 99),
        (N, "22", 29),
        (N, "12", 34),
        (N, "5", 24),
    ],
    table_bag: &["55"],
    tails: &[24, 25],
    command: &["9"],
};

pub(crate) const V4_55: Layout = Layout {
    serialization: s(4, 55),
    strict: true,
    items: &[
        (G, "22", 30),
        (D, "12", 36),
        (F, "44", 68),
        (F, "45", 69),
        (F, "46", 71),
        (F, "47", 72),
        (B, "33", 58),
        (B, "34", 59),
        (T, "64", 112),
        (T, "64", 114),
        (T, "64", 116),
        (T, "69", 117),
        (T, "70", 120),
        (N, "22", 29),
        (N, "22", 30),
        (N, "12", 34),
        (N, "12", 36),
        (N, "5", 24),
    ],
    table_bag: &["64", "69", "70"],
    tails: &[32, 33, 34, 35, 36, 37],
    command: &["9", "10", "11"],
};

pub(crate) const V2_35: Layout = Layout {
    serialization: s(2, 35),
    strict: true,
    items: &[
        (G, "19", 26),
        (F, "32", 53),
        (B, "21", 33),
        (T, "44", 74),
        (N, "19", 25),
        (N, "8", 25),
    ],
    table_bag: &["44"],
    tails: &[7],
    command: &["6"],
};

pub(crate) const V2_37: Layout = Layout {
    serialization: s(2, 37),
    strict: true,
    items: &[
        (G, "19", 26),
        (F, "32", 53),
        (B, "22", 34),
        (T, "47", 83),
        (N, "19", 25),
        (N, "8", 25),
        (N, "3", 21),
    ],
    table_bag: &["47"],
    tails: &[8],
    command: &["7"],
};

pub(crate) const V3_42: Layout = Layout {
    serialization: s(3, 42),
    strict: true,
    items: &[
        (G, "21", 29),
        (F, "33", 55),
        (D, "10", 34),
        (B, "28", 48),
        (T, "49", 91),
        (T, "50", 92),
        (N, "21", 28),
        (N, "10", 32),
        (N, "4", 23),
    ],
    table_bag: &["49", "50"],
    tails: &[17],
    command: &["7", "8"],
};

const LAYOUTS: [&Layout; 9] = [
    &V4_59, &V2_35, &V2_37, &V3_42, &V3_48, &V3_49, &V4_49, &V4_50, &V4_55,
];

const STRICT: [&Layout; 8] = [
    &V2_35, &V2_37, &V3_42, &V3_48, &V3_49, &V4_49, &V4_50, &V4_55,
];

const STRICT_EXTRA: [(Option<&str>, &str, usize); 2] = [(Some("Addition"), "5", 24), (B, "30", 51)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoLayout {
    Shape,
    Unsupported(Serialization),
}

pub(crate) fn detect(entry: &Node) -> Result<&'static Layout, NoLayout> {
    let s = Serialization::of(entry).ok_or(NoLayout::Shape)?;
    LAYOUTS
        .iter()
        .copied()
        .find(|l| l.serialization == s)
        .ok_or(NoLayout::Unsupported(s))
}

impl Layout {
    pub(crate) fn bag_at(&self, class: Option<&str>, version: &str) -> Option<usize> {
        let known = if self.strict {
            STRICT.iter().any(|l| l.table_bag.contains(&version))
        } else {
            self.table_bag.contains(&version)
        };
        (class == Some("Table") && known).then_some(TABLE_BAG_AT)
    }

    pub(crate) fn accepts_item(&self, class: Option<&str>, version: &str, len: usize) -> bool {
        !self.strict
            || STRICT
                .iter()
                .flat_map(|l| l.items.iter())
                .chain(STRICT_EXTRA.iter())
                .any(|(c, v, l)| *c == class && *v == version && *l == len)
    }

    pub(crate) fn accepts_tail(&self, len: usize) -> bool {
        !self.strict || self.tails.contains(&len)
    }

    pub(crate) fn accepts_record(&self, section: Section, version: Option<&str>) -> bool {
        !self.strict
            || version.is_some_and(|v| match section {
                Section::Attribute => v == ATTRIBUTE,
                Section::Parameter => v == PARAMETER,
                Section::Command => self.command.contains(&v),
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Attribute,
    Parameter,
    Command,
}
