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
    command: &'static str,
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
    command: "",
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
    command: "9",
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
    command: "9",
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
    command: "9",
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
    tails: &[25],
    command: "9",
};

const LAYOUTS: [&Layout; 5] = [&V4_59, &V3_48, &V3_49, &V4_49, &V4_50];

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
        (class == Some("Table") && self.table_bag.contains(&version)).then_some(TABLE_BAG_AT)
    }

    pub(crate) fn accepts_item(&self, class: Option<&str>, version: &str, len: usize) -> bool {
        !self.strict
            || self
                .items
                .iter()
                .any(|(c, v, l)| *c == class && *v == version && *l == len)
    }

    pub(crate) fn accepts_tail(&self, len: usize) -> bool {
        !self.strict || self.tails.contains(&len)
    }

    pub(crate) fn accepts_record(&self, section: Section, version: Option<&str>) -> bool {
        !self.strict
            || version
                == Some(match section {
                    Section::Attribute => ATTRIBUTE,
                    Section::Parameter => PARAMETER,
                    Section::Command => self.command,
                })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Attribute,
    Parameter,
    Command,
}
