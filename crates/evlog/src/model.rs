use brace::Node;
pub use bsl_prim::BslDate;

pub type Uuid16 = brace::Guid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Information,
    Error,
    Warning,
    Note,
}

impl Level {
    pub fn letter(self) -> char {
        match self {
            Level::Information => 'I',
            Level::Error => 'E',
            Level::Warning => 'W',
            Level::Note => 'N',
        }
    }
    pub fn from_letter(s: &str) -> Option<Level> {
        Some(match s {
            "I" => Level::Information,
            "E" => Level::Error,
            "W" => Level::Warning,
            "N" => Level::Note,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxStatus {
    None,
    Unfinished,
    Committed,
    RolledBack,
}

impl TxStatus {
    pub fn letter(self) -> char {
        match self {
            TxStatus::None => 'N',
            TxStatus::Unfinished => 'U',
            TxStatus::Committed => 'C',
            TxStatus::RolledBack => 'R',
        }
    }
    pub fn from_letter(s: &str) -> Option<TxStatus> {
        Some(match s {
            "N" => TxStatus::None,
            "U" => TxStatus::Unfinished,
            "C" => TxStatus::Committed,
            "R" => TxStatus::RolledBack,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TxnToken(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserRef {
    pub uuid: Uuid16,
    pub name: String,
}

impl UserRef {
    pub fn nobody() -> UserRef {
        let b = [
            0x07, 0x15, 0x23, 0xa4, 0x51, 0x6f, 0x4f, 0xce, 0xba, 0x4b, 0x0d, 0x11, 0xab, 0x7a,
            0x18, 0x93,
        ];
        UserRef {
            uuid: Uuid16::from_bytes(b),
            name: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MetaRef {
    pub uuid: Uuid16,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Meta {
    None,
    One(MetaRef),
    Many(Vec<MetaRef>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SeparatorRef {
    pub uuid: Uuid16,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeparatorValue {
    pub separator: SeparatorRef,
    pub value: Node,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub time: BslDate,
    pub level: Level,
    pub status: TxStatus,
    pub txn: Option<TxnToken>,
    pub user: Option<UserRef>,
    pub computer: String,
    pub app: String,
    pub connection: u32,
    pub session: u32,
    pub event: String,
    pub comment: String,
    pub metadata: Meta,
    pub data: Node,
    pub data_presentation: String,
    pub server: String,
    pub main_port: u32,
    pub aux_port: u32,
    pub separators: Vec<SeparatorValue>,
}

pub mod data {
    use super::{BslDate, Node, fmt_time};

    fn tagged(tag: &str, v: Node) -> Node {
        Node::list(vec![Node::str(tag), v])
    }

    pub fn undefined() -> Node {
        Node::list(vec![Node::str("U")])
    }
    pub fn boolean(b: bool) -> Node {
        tagged("B", Node::int(i64::from(b)))
    }
    pub fn number(canonical: &str) -> Node {
        tagged("N", Node::atom(canonical))
    }
    pub fn string(s: &str) -> Node {
        tagged("S", Node::str(s))
    }
    pub fn date(d: BslDate) -> Node {
        tagged("D", Node::atom(fmt_time(d)))
    }
    pub fn reference(table: u32, id_hex32: &str) -> Node {
        tagged("R", Node::atom(format!("{table}:{id_hex32}")))
    }
    pub fn authentication(name: &str, method: &str) -> Node {
        tagged(
            "P",
            Node::list(vec![Node::int(514), string(name), string(method)]),
        )
    }

    pub fn props(items: Vec<Node>) -> Node {
        let mut inner = vec![Node::int(items.len() as i64)];
        inner.extend(items);
        tagged("P", Node::list(inner))
    }
}

pub fn fmt_time(t: BslDate) -> String {
    let p = t.parts();
    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        p.year, p.month, p.day, p.hour, p.minute, p.second
    )
}
