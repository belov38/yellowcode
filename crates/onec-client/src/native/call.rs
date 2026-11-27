use base64::Engine as _;
use brace::Node;

use super::{
    BOM, CTX_IB, CTX_REGMNGR, IFACE_CREATOR, IFACE_REGMNGR, NativeError, OBJ_CREATOR, T_ADDR,
    T_GUIDREF, T_OBJREF, T_TICKET,
};

pub const MAX_TEXT_ANSWER: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub app: String,
    pub computer: String,
    pub version: String,
    pub lang: String,
    pub locale: String,
}

pub fn s(text: &str) -> String {
    format!("{{\"S\",\"{}\"}}", text.replace('"', "\"\""))
}

pub fn guidref(guid: &str) -> String {
    format!("{{\"#\",{T_GUIDREF},{{1,{guid}}}}}")
}

pub fn ticket(token: &[u8]) -> String {
    let b64 = if token.is_empty() {
        String::new()
    } else {
        base64::engine::general_purpose::STANDARD.encode(token)
    };
    format!("{{\"#\",{T_TICKET},{{{b64}}}}}")
}

pub fn bind(obj: &str, seq: u32, iface: &str) -> String {
    format!("{{0,{obj},{seq},1,{iface}}}")
}

pub fn release(obj: &str, seq: u32) -> String {
    format!("{{0,{obj},{seq},0}}")
}

fn method(obj: &str, seq: u32, iface: &str, number: u32, argc: u32, tail: &str) -> String {
    format!("{{0,{obj},{seq},4,{iface},0,{number},{argc},0,\r\n{tail}}}")
}

pub fn find_ib(obj: &str, seq: u32, ib: &str, app: &str, request_id: &str) -> String {
    let tail = [
        format!("{},0", s("")),
        format!("{},0", s("")),
        format!("{},0", s(ib)),
        format!("{},0", s(app)),
        format!("{},0", guidref(request_id)),
        format!("{},0", s("")),
        format!("{},1,1", s("")),
    ]
    .join(",\r\n");
    method(obj, seq, IFACE_REGMNGR, 3, 9, &tail)
}

pub fn create_instance(
    seq: u32,
    client: &ClientInfo,
    context: &str,
    context_id: &str,
    token: &[u8],
    last: &str,
) -> String {
    let tail = [
        format!("{},0", s(context)),
        format!("{},0", guidref(context_id)),
        format!("{},0", s(&client.app)),
        format!("{},2", s(&client.computer)),
        format!("{},1,0", ticket(token)),
        format!("{},0", s(&client.version)),
        format!("{},0", s(&client.lang)),
        format!("{},0", s(&client.locale)),
        format!("{},1", guidref(last)),
    ]
    .join(",\r\n");
    method(OBJ_CREATOR, seq, IFACE_CREATOR, 1, 11, &tail)
}

pub const MANAGER_CONTEXT: (&str, &str) = ("RegMngrCntxt:RegMngrCntxt", CTX_REGMNGR);

pub fn worker_context(ib: &str) -> (String, &'static str) {
    (format!("{ib}:{ib}"), CTX_IB)
}

pub fn message(text: &str) -> Vec<u8> {
    [BOM, text.as_bytes()].concat()
}

pub fn parse_answer(payload: &[u8]) -> Result<Vec<Node>, NativeError> {
    if payload.len() > MAX_TEXT_ANSWER {
        return Err(NativeError::TooLarge("text answer", MAX_TEXT_ANSWER));
    }
    let body = payload.strip_prefix(BOM).unwrap_or(payload);
    if body.is_empty() {
        return Err(NativeError::Malformed("empty answer".into()));
    }
    let text = std::str::from_utf8(body)
        .map_err(|e| NativeError::Malformed(format!("answer is not UTF-8: {e}")))?;
    let nodes = brace::parse_multi(text)
        .map_err(|e| NativeError::Malformed(format!("answer is not brace text: {e:?}")))?;
    let first = nodes
        .first()
        .and_then(Node::as_list)
        .ok_or_else(|| NativeError::Malformed(format!("answer is not a list: {}", head(text))))?;
    match first.first() {
        Some(Node::Atom(a)) if a == "1" => Ok(nodes),
        Some(Node::Atom(a)) if a == "2" => Err(NativeError::Server(
            first_string(&first[1..]).unwrap_or_else(|| head(text)),
        )),
        _ => Err(NativeError::Malformed(format!(
            "unexpected answer: {}",
            head(text)
        ))),
    }
}

fn head(text: &str) -> String {
    text.chars().take(200).collect()
}

fn first_string(nodes: &[Node]) -> Option<String> {
    nodes.iter().find_map(|n| match n {
        Node::Str(s) if s.chars().count() > 1 => Some(s.clone()),
        Node::List(items) => first_string(items),
        _ => None,
    })
}

pub fn find_typed<'a>(nodes: &'a [Node], type_guid: &str) -> Vec<&'a [Node]> {
    let mut out = Vec::new();
    let mut stack: Vec<&'a Node> = nodes.iter().rev().collect();
    while let Some(n) = stack.pop() {
        if let Node::List(items) = n {
            if let [Node::Str(tag), Node::Atom(t), ..] = items.as_slice()
                && tag == "#"
                && t.eq_ignore_ascii_case(type_guid)
            {
                out.push(items.as_slice());
            }
            stack.extend(items.iter().rev());
        }
    }
    out
}

pub fn created_object(nodes: &[Node]) -> Result<String, NativeError> {
    find_typed(nodes, T_OBJREF)
        .into_iter()
        .find_map(|t| match t.get(2) {
            Some(Node::List(v)) => match v.first() {
                Some(Node::Atom(g)) if brace::Guid::parse(g).is_some() => Some(g.to_lowercase()),
                _ => None,
            },
            _ => None,
        })
        .ok_or_else(|| {
            NativeError::Server(
                "createRemoteInstance returned no object (context or authentication refused)"
                    .into(),
            )
        })
}

pub fn worker_address(nodes: &[Node]) -> Result<(String, u16), NativeError> {
    let addr = string_value_with_port(nodes)
        .or_else(|| {
            find_typed(nodes, T_ADDR)
                .into_iter()
                .find_map(|t| match t.get(2) {
                    Some(Node::List(v)) => v.iter().find_map(|x| match x {
                        Node::Str(s) => Some(s.clone()),
                        _ => None,
                    }),
                    _ => None,
                })
        })
        .ok_or_else(|| NativeError::Malformed("the lookup returned no worker address".into()))?;
    let full = if addr.contains("://") {
        addr.clone()
    } else {
        format!("tcp://{addr}")
    };
    parse_tcp(&full).ok_or_else(|| NativeError::Malformed(format!("bad worker address {addr:?}")))
}

fn string_value_with_port(nodes: &[Node]) -> Option<String> {
    nodes.iter().find_map(|n| match n {
        Node::List(items) => match items.as_slice() {
            [Node::Str(tag), Node::Str(v)] if tag == "S" && v.contains(':') => Some(v.clone()),
            _ => string_value_with_port(items),
        },
        _ => None,
    })
}

pub fn parse_tcp(addr: &str) -> Option<(String, u16)> {
    let rest = addr.strip_prefix("tcp://")?;
    let (host, port) = rest.rsplit_once(':')?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port.parse().ok()?))
}
