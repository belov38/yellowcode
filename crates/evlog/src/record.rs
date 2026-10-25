use brace::Node;

use crate::dict::{DictKey, Dicts, EventIdx};
use crate::{
    BslDate, Error, Event, Level, Meta, MetaRef, SeparatorRef, SeparatorValue, TxStatus, UserRef,
    Uuid16, fmt_time,
};

pub const HEADER_LINE: &str = "1CV8LOG(ver 2.0)";

pub fn header(guid: &Uuid16) -> Vec<u8> {
    let mut out = vec![0xEF, 0xBB, 0xBF];
    out.extend(format!("{HEADER_LINE}\r\n{guid}\r\n\r\n").as_bytes());
    out
}

pub fn parse_header(bytes: &[u8]) -> Result<(Uuid16, usize), Error> {
    let bad = || Error::Malformed("bad log header".into());
    let head = bytes.get(..bytes.len().min(64)).unwrap_or_default();
    let text = match std::str::from_utf8(head) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(head.get(..e.valid_up_to()).unwrap_or_default())
            .map_err(|_| bad())?,
    };
    let rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    let bom = text.len() - rest.len();
    let rest = rest
        .strip_prefix(HEADER_LINE)
        .and_then(|r| r.strip_prefix("\r\n"))
        .ok_or_else(bad)?;
    let line = rest.get(..36).ok_or_else(bad)?;
    let guid = Uuid16::parse(line).ok_or_else(|| Error::Malformed("bad log guid".into()))?;
    if !rest.get(36..).is_some_and(|r| r.starts_with("\r\n\r\n")) {
        return Err(bad());
    }
    Ok((guid, bom + HEADER_LINE.len() + 2 + 36 + 4))
}

pub fn event_node(e: &Event, ix: &EventIdx, status: TxStatus, tx: (i64, u64)) -> Node {
    let a = |s: String| Node::atom(s);
    let mut items = vec![
        a(fmt_time(e.time)),
        a(status.letter().to_string()),
        Node::list(vec![a(format!("{:x}", tx.0)), a(format!("{:x}", tx.1))]),
        a(ix.user.to_string()),
        a(ix.computer.to_string()),
        a(ix.app.to_string()),
        a(e.connection.to_string()),
        a(ix.event.to_string()),
        a(e.level.letter().to_string()),
        Node::str(e.comment.clone()),
        a(ix.metadata.to_string()),
        e.data.clone(),
        Node::str(e.data_presentation.clone()),
        a(ix.server.to_string()),
        a(ix.main_port.to_string()),
        a(ix.aux_port.to_string()),
        a(e.session.to_string()),
        a(ix.meta_array.len().to_string()),
    ];
    items.extend(ix.meta_array.iter().map(|i| a(i.to_string())));
    items.push(ix.separators.clone());
    Node::list(items)
}

fn bad(what: &str) -> Error {
    Error::Malformed(format!("bad .lgp record: {what}"))
}

fn parse_time(s: &str) -> Option<BslDate> {
    if s.len() != 14 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let f = |a: usize, b: usize| s.get(a..b)?.parse::<u32>().ok();
    BslDate::from_parts(
        f(0, 4)?,
        f(4, 6)?,
        f(6, 8)?,
        f(8, 10)?,
        f(10, 12)?,
        f(12, 14)?,
    )
}

fn separators(n: &Node, d: &Dicts) -> Result<Vec<SeparatorValue>, Error> {
    let it = n.as_list().ok_or_else(|| bad("bad separators"))?;
    let count = it
        .first()
        .and_then(Node::as_u32)
        .and_then(|c| usize::try_from(c).ok())
        .ok_or_else(|| bad("bad separators"))?;
    if count.checked_mul(2).and_then(|c| c.checked_add(1)) != Some(it.len()) {
        return Err(bad("bad separators"));
    }
    let mut out = Vec::with_capacity(count);
    for p in it.get(1..).unwrap_or_default().chunks(2) {
        let (Some(s), Some(v)) = (
            p.first().and_then(Node::as_u32),
            p.get(1).and_then(Node::as_u32),
        ) else {
            return Err(bad("bad separators"));
        };
        let (Some(DictKey::Separator(uuid, name)), Some(value)) = (d.key(9, s), d.value(s, v))
        else {
            return Err(bad("separator index without a dictionary record"));
        };
        out.push(SeparatorValue {
            separator: SeparatorRef {
                uuid: *uuid,
                name: name.clone(),
            },
            value: value.clone(),
        });
    }
    Ok(out)
}

pub fn event_from_node(n: &Node, d: &Dicts) -> Result<(Event, (i64, u64)), Error> {
    let it = n.as_list().ok_or_else(|| bad("not a list"))?;
    let at = |i: usize| it.get(i).ok_or_else(|| bad("too short"));
    let num = |i: usize| -> Result<u32, Error> { at(i)?.as_u32().ok_or_else(|| bad("bad number")) };
    let text = |i: usize| -> Result<&str, Error> { at(i)?.text().ok_or_else(|| bad("bad text")) };
    let count = usize::try_from(num(17)?).map_err(|_| bad("bad count"))?;
    let sep_at = count.checked_add(18).ok_or_else(|| bad("bad count"))?;
    if it.len() != sep_at + 1 {
        return Err(bad("wrong field count"));
    }
    let time = parse_time(text(0)?).ok_or_else(|| bad("bad time"))?;
    let status = TxStatus::from_letter(text(1)?).ok_or_else(|| bad("bad status"))?;
    let level = Level::from_letter(text(8)?).ok_or_else(|| bad("bad level"))?;
    let tx = {
        let p = at(2)?.as_list().ok_or_else(|| bad("bad transaction"))?;
        let hex = |i: usize| {
            p.get(i)
                .and_then(Node::as_atom)
                .ok_or_else(|| bad("bad transaction"))
        };
        match p {
            [_, _] => (
                i64::from_str_radix(hex(0)?, 16).map_err(|_| bad("bad transaction"))?,
                u64::from_str_radix(hex(1)?, 16).map_err(|_| bad("bad transaction"))?,
            ),
            _ => return Err(bad("bad transaction")),
        }
    };
    let lookup = |kind: u8, i: usize| -> Result<Option<&DictKey>, Error> {
        match num(i)? {
            0 => Ok(None),
            ix => d
                .key(kind, ix)
                .map(Some)
                .ok_or_else(|| bad("index without a dictionary record")),
        }
    };
    let user = match lookup(1, 3)? {
        Some(DictKey::User(u, n)) => Some(UserRef {
            uuid: *u,
            name: n.clone(),
        }),
        _ => None,
    };
    let string = |kind: u8, i: usize| -> Result<String, Error> {
        Ok(match lookup(kind, i)? {
            Some(DictKey::Computer(s))
            | Some(DictKey::App(s))
            | Some(DictKey::Event(s))
            | Some(DictKey::Server(s)) => s.clone(),
            _ => String::new(),
        })
    };
    let port = |kind: u8, i: usize| -> Result<u32, Error> {
        Ok(match lookup(kind, i)? {
            Some(DictKey::MainPort(p)) | Some(DictKey::AuxPort(p)) => *p,
            _ => 0,
        })
    };
    let meta_ref = |idx: u32| -> Result<MetaRef, Error> {
        match d.key(5, idx) {
            Some(DictKey::Meta(u, n)) => Ok(MetaRef {
                uuid: *u,
                name: n.clone(),
            }),
            _ => Err(bad("metadata index without a dictionary record")),
        }
    };
    let metadata = match (num(10)?, count) {
        (0, 0) => Meta::None,
        (m, 0) => Meta::One(meta_ref(m)?),
        (0, _) => {
            let mut v = Vec::new();
            for i in 18..sep_at {
                v.push(meta_ref(num(i)?)?);
            }
            Meta::Many(v)
        }
        _ => return Err(bad("both metadata forms")),
    };
    let e = Event {
        time,
        level,
        status,
        txn: None,
        user,
        computer: string(2, 4)?,
        app: string(3, 5)?,
        connection: num(6)?,
        session: num(16)?,
        event: string(4, 7)?,
        comment: text(9)?.to_owned(),
        metadata,
        data: at(11)?.clone(),
        data_presentation: text(12)?.to_owned(),
        server: string(6, 13)?,
        main_port: port(7, 14)?,
        aux_port: port(8, 15)?,
        separators: separators(at(sep_at)?, d)?,
    };
    Ok((e, tx))
}
