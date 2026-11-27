use brace::Guid;
use brace::binary::{Atom, AtomValue, BinNode, ReadLimits, Reader, Writer};

use super::{IFACE_VRS, NativeError, T_HTTP, T_TICKET};
use crate::transport::Response;

pub const HTTP_CHUNK: usize = 16 * 1024;
pub const COOKIE_HIGH: u64 = 0x0000_02B7_0000_0000;
const MAX_NODES: u64 = 1 << 20;
const FRAME_SLACK: u64 = 64 * 1024;

fn guid(s: &str) -> Guid {
    Guid::parse(s).unwrap_or_else(|| Guid::from_bytes([0; 16]))
}

fn raw_atom(tag: u8, payload: &[u8]) -> Result<Atom, NativeError> {
    let mut bytes = Vec::with_capacity(payload.len() + 1);
    bytes.push(tag);
    bytes.extend_from_slice(payload);
    match Reader::new(bytes.as_slice()).next_event() {
        Ok(Some(brace::binary::Event::Atom(a))) => Ok(a),
        other => Err(NativeError::Malformed(format!(
            "cannot build atom with tag {tag:#04x}: {other:?}"
        ))),
    }
}

fn seq_atom(seq: u32) -> Result<Atom, NativeError> {
    raw_atom(0x0D, &((seq & 0xFFFF) as u16).to_le_bytes())
}

fn chunk_atom(chunk: &[u8], first: bool) -> Result<Atom, NativeError> {
    let len =
        u16::try_from(chunk.len()).map_err(|_| NativeError::TooLarge("HTTP chunk", HTTP_CHUNK))?;
    match u8::try_from(len) {
        Ok(short) if first => raw_atom(0x1A, &[&[short][..], chunk].concat()),
        _ => raw_atom(0x1B, &[&len.to_le_bytes()[..], chunk].concat()),
    }
}

pub fn request(
    session: &Guid,
    seq: u32,
    first: bool,
    cookie: u64,
    http: &[u8],
) -> Result<Vec<u8>, NativeError> {
    let mut w = Writer::new(Vec::with_capacity(http.len() + 128));
    w.open()?;
    w.int(0)?;
    w.guid(session)?;
    w.atom(&seq_atom(seq)?)?;
    if !first {
        w.int(0)?;
    }
    w.int(4)?;
    w.guid(&guid(IFACE_VRS))?;
    for v in [0, 1, 3, 0] {
        w.int(v)?;
    }
    w.open()?;
    w.str("#")?;
    w.guid(&guid(T_HTTP))?;
    w.int(i128::from(cookie))?;
    for _ in 0..3 {
        w.int(0)?;
    }
    w.open()?;
    for (i, chunk) in http.chunks(HTTP_CHUNK).enumerate() {
        let atom = chunk_atom(chunk, i == 0)?;
        if i == 0 {
            w.atom(&atom)?;
        } else {
            w.atom_joined(&atom)?;
        }
    }
    w.close()?;
    w.close()?;
    w.int(2)?;
    w.open()?;
    w.str("#")?;
    w.guid(&guid(T_TICKET))?;
    w.open()?;
    w.int(0)?;
    w.atom_joined(&Atom::int(0))?;
    w.close()?;
    w.close()?;
    w.int(2)?;
    w.open()?;
    w.str("B")?;
    w.int(0)?;
    w.close()?;
    w.close()?;
    Ok(w.finish()?)
}

pub fn close(session: &Guid, seq: u32, after_frames: bool) -> Result<Vec<u8>, NativeError> {
    let mut items = vec![
        BinNode::Atom(Atom::int(0)),
        BinNode::Atom(Atom::guid(*session)),
        BinNode::Atom(seq_atom(seq)?),
    ];
    if after_frames {
        items.push(BinNode::Atom(Atom::int(0)));
    }
    items.push(BinNode::Atom(Atom::int(0)));
    Ok(brace::binary::encode_all(&[BinNode::List(items)]))
}

fn read_answer(frame: &[u8], max_bytes: usize) -> Result<Vec<BinNode>, NativeError> {
    if frame.is_empty() {
        return Err(NativeError::Malformed(
            "empty answer to a frame (the server got no key exchange on this connection?)".into(),
        ));
    }
    let mut r = Reader::new(frame);
    r.begin_budget(ReadLimits {
        atom_bytes: max_bytes as u64,
        retained_bytes: max_bytes as u64 + FRAME_SLACK,
        nodes: MAX_NODES,
    });
    let mut out = Vec::new();
    loop {
        match r.read_value() {
            Ok(Some(v)) => out.push(v),
            Ok(None) => return Ok(out),
            Err(e) if matches!(e.kind, brace::binary::ErrorKind::Limit("nodes")) => {
                return Err(NativeError::TooLarge(
                    "answer frame nodes",
                    MAX_NODES as usize,
                ));
            }
            Err(e) if matches!(e.kind, brace::binary::ErrorKind::Limit(_)) => {
                return Err(NativeError::TooLarge("answer frame", max_bytes));
            }
            Err(e) => {
                return Err(NativeError::Malformed(format!(
                    "answer frame at byte {}: {:?}",
                    e.offset, e.kind
                )));
            }
        }
    }
}

fn atom_int(n: &BinNode) -> Option<i128> {
    match n {
        BinNode::Atom(a) => a.as_int(),
        BinNode::List(_) => None,
    }
}

fn atom_text(n: &BinNode) -> Option<String> {
    match n {
        BinNode::Atom(a) => a.as_str(),
        BinNode::List(_) => None,
    }
}

fn long_text(n: &BinNode) -> Option<String> {
    match n {
        BinNode::Atom(a) => match a.value() {
            AtomValue::Narrow(_) | AtomValue::Wide(_) => a.as_str().filter(|s| !s.is_empty()),
            _ => None,
        },
        BinNode::List(_) => None,
    }
}

fn answer_list(nodes: &[BinNode]) -> Result<&[BinNode], NativeError> {
    let Some(BinNode::List(top)) = nodes.first() else {
        return Err(NativeError::Malformed("answer frame is not a list".into()));
    };
    match top.first().and_then(atom_int) {
        Some(1) => Ok(top),
        Some(2) => Err(NativeError::Server(
            first_text(&top[1..]).unwrap_or_else(|| "exception without a text".into()),
        )),
        _ => Err(NativeError::Malformed(
            "answer frame does not start with 1 or 2".into(),
        )),
    }
}

fn first_text(nodes: &[BinNode]) -> Option<String> {
    nodes.iter().find_map(|n| match n {
        BinNode::List(items) => first_text(items),
        a => long_text(a),
    })
}

pub fn response_http(frame: &[u8], max_bytes: usize) -> Result<Vec<u8>, NativeError> {
    let nodes = read_answer(frame, max_bytes)?;
    let top = answer_list(&nodes)?;
    let http_type = guid(T_HTTP);
    let mut stack: Vec<&BinNode> = top.iter().rev().collect();
    while let Some(n) = stack.pop() {
        let BinNode::List(items) = n else { continue };
        let typed = matches!(items.first().and_then(atom_text), Some(t) if t == "#")
            && matches!(items.get(1), Some(BinNode::Atom(a)) if a.as_guid() == Some(http_type));
        if !typed {
            stack.extend(items.iter().rev());
            continue;
        }
        let Some(BinNode::List(chunks)) = items.last() else {
            return Err(NativeError::Malformed("HTTP value without chunks".into()));
        };
        let mut http = Vec::new();
        for c in chunks {
            let BinNode::Atom(a) = c else {
                return Err(NativeError::Malformed("HTTP chunk is a list".into()));
            };
            let bytes = a
                .as_bytes()
                .ok_or_else(|| NativeError::Malformed("HTTP chunk is not bytes".into()))?;
            http.extend_from_slice(&bytes);
            if http.len() > max_bytes {
                return Err(NativeError::TooLarge("HTTP in a frame", max_bytes));
            }
        }
        return Ok(http);
    }
    Err(NativeError::Malformed(
        "answer frame carries no HTTP".into(),
    ))
}

pub fn check_close_answer(frame: &[u8]) -> Result<(), NativeError> {
    answer_list(&read_answer(frame, 1024)?).map(|_| ())
}

pub fn http_request(
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Vec<u8> {
    let mut out = format!("{method} {path} HTTP/1.1\r\n").into_bytes();
    for (k, v) in headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    if !body.is_empty() {
        out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

pub fn parse_http(raw: &[u8]) -> Result<Response, NativeError> {
    let (head, body) = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => (&raw[..i], raw[i + 4..].to_vec()),
        None => {
            let mut end = raw.len();
            while end > 0 && matches!(raw[end - 1], b'\r' | b'\n') {
                end -= 1;
            }
            (&raw[..end], Vec::new())
        }
    };
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .strip_prefix("HTTP/1.1 ")
        .and_then(|r| r.split_whitespace().next())
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| {
            NativeError::Malformed(format!(
                "bad HTTP status line in a frame: {:?}",
                status_line.chars().take(80).collect::<String>()
            ))
        })?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    if get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        return Err(NativeError::Malformed(
            "chunked HTTP inside a frame is not supported".into(),
        ));
    }
    if let Some(len) = get("content-length")
        && len.parse::<usize>().ok() != Some(body.len())
    {
        return Err(NativeError::Malformed(format!(
            "HTTP in a frame: Content-Length {len:?}, body {} bytes",
            body.len()
        )));
    }
    Ok(Response {
        status,
        headers,
        body,
    })
}
