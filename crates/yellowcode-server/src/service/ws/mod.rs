pub mod soap;
pub mod values;
use super::{RouteError, ServiceError, wire::Unimplemented};
use appobj::model::Uuid16;
use md_model::{Child, ChildTag, Configuration, MdKind, Value};

pub const XS: &str = "http://www.w3.org/2001/XMLSchema";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XdtoType {
    String,
    Decimal,
    Boolean,
    DateTime,
    ValueStorage,
    SubscriberId,
    Other(String),
}
impl XdtoType {
    pub fn of(value: Option<&Value>) -> Self {
        let text = match value {
            Some(Value::Xml(e)) if e.attrs.is_empty() && e.children.is_empty() => {
                e.text.clone().unwrap_or_default()
            }
            Some(Value::Xml(e)) => {
                let text = e.text.clone().unwrap_or_default();
                return if e.children.is_empty() && declared(e, &text) == Some((EXT, SUBSCRIBER_ID))
                {
                    Self::SubscriberId
                } else {
                    Self::Other(text)
                };
            }
            Some(Value::Text(t)) => return Self::Other(t.clone()),
            _ => return Self::Other(String::new()),
        };
        match text.as_str() {
            "xs:string" => Self::String,
            "xs:decimal" => Self::Decimal,
            "xs:boolean" => Self::Boolean,
            "xs:dateTime" => Self::DateTime,
            "v8:ValueStorage" => Self::ValueStorage,
            _ => Self::Other(text),
        }
    }
}

const EXT: &str = "http://v8.1c.ru/8.3/data/ext";
const SUBSCRIBER_ID: &str = "DeliverableNotificationSubscriberID";

fn declared<'a>(e: &'a md_model::XmlElem, text: &'a str) -> Option<(&'a str, &'a str)> {
    let (prefix, local) = text.split_once(':')?;
    let [(name, ns)] = e.attrs.as_slice() else {
        return None;
    };
    (name.strip_prefix("xmlns:")? == prefix).then_some((ns.as_str(), local))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsParameter {
    pub id: Uuid16,
    pub name: String,
    pub xdto: XdtoType,
    pub nillable: bool,
    pub direction: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsOperation {
    pub id: Uuid16,
    pub name: String,
    pub procedure: String,
    pub returns: XdtoType,
    pub nillable: bool,
    pub transactioned: bool,
    pub parameters: Vec<WsParameter>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsService {
    pub id: Uuid16,
    pub name: String,
    pub namespace: String,
    pub descriptor: String,
    pub reuse: String,
    pub operations: Vec<WsOperation>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WsTable {
    pub services: Vec<WsService>,
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}
fn flag(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn parameter(p: &Child) -> WsParameter {
    WsParameter {
        id: p.id,
        name: p.name.clone(),
        xdto: XdtoType::of(p.prop("XDTOValueType")),
        nillable: flag(p.prop("Nillable")).unwrap_or(false),
        direction: text(p.prop("TransferDirection")),
    }
}

fn operation(o: &Child) -> WsOperation {
    WsOperation {
        id: o.id,
        name: o.name.clone(),
        procedure: text(o.prop("ProcedureName")),
        returns: XdtoType::of(o.prop("XDTOReturningValueType")),
        nillable: flag(o.prop("Nillable")).unwrap_or(false),
        transactioned: flag(o.prop("Transactioned")).unwrap_or(true),
        parameters: o
            .children
            .iter()
            .filter(|c| c.tag == ChildTag::Parameter)
            .map(parameter)
            .collect(),
    }
}

impl WsTable {
    pub fn from_config(config: &Configuration) -> Self {
        let services = config
            .of_kind(MdKind::WebService)
            .map(|s| WsService {
                id: s.id,
                name: s.name.clone(),
                namespace: text(s.prop("Namespace")),
                descriptor: text(s.prop("DescriptorFileName")),
                reuse: text(s.prop("ReuseSessions")),
                operations: s
                    .children
                    .iter()
                    .filter(|c| c.tag == ChildTag::Operation)
                    .map(operation)
                    .collect(),
            })
            .collect();
        Self { services }
    }

    pub fn service<'a>(&'a self, path: &str) -> Result<&'a WsService, WsError> {
        let name = path
            .strip_prefix("/ws/")
            .ok_or(WsError::Service(ServiceError::Route(
                RouteError::UnknownRoot,
            )))?;
        if name.contains('/') {
            return Err(WsError::Unimplemented(Unimplemented(
                "a web service path below /ws/<name>",
            )));
        }
        self.services
            .iter()
            .find(|s| s.name == name || (!s.descriptor.is_empty() && s.descriptor == name))
            .ok_or(WsError::Service(ServiceError::Route(
                RouteError::UnknownRoot,
            )))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsError {
    Service(ServiceError),
    Unimplemented(Unimplemented),
}
impl From<ServiceError> for WsError {
    fn from(error: ServiceError) -> Self {
        Self::Service(error)
    }
}
impl From<crate::process::ProcessError> for WsError {
    fn from(error: crate::process::ProcessError) -> Self {
        Self::Service(error.into())
    }
}
impl From<Unimplemented> for WsError {
    fn from(error: Unimplemented) -> Self {
        Self::Unimplemented(error)
    }
}

pub fn page(request: &super::RequestData) -> Result<super::ResponseData, Unimplemented> {
    if !request.query.is_empty() {
        return Err(Unimplemented(
            "?wsdl and other queries of a web service page",
        ));
    }
    let base = &request.base_url;
    if base.is_empty()
        || !base
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'<' | b'>' | b'&'))
    {
        return Err(Unimplemented(
            "a web service page for a base URL that needs escaping",
        ));
    }
    Ok(super::ResponseData {
        status: 200,
        headers: vec![(
            "Content-Type".to_owned(),
            "text/html; charset=utf-8".to_owned(),
        )],
        body: format!("\u{FEFF}<html><body><a href=\"{base}?wsdl\">{base}</a></body></html>")
            .into_bytes(),
    })
}

pub fn admit_operation(op: &WsOperation) -> Result<(), Unimplemented> {
    if op.transactioned {
        return Err(Unimplemented("a web service operation with Transactioned"));
    }
    if op.procedure.is_empty() {
        return Err(Unimplemented(
            "a web service operation without ProcedureName",
        ));
    }
    if matches!(op.returns, XdtoType::Other(_) | XdtoType::SubscriberId) {
        return Err(Unimplemented(
            "an XDTO return type other than xs:string, xs:decimal, xs:boolean, xs:dateTime and v8:ValueStorage",
        ));
    }
    for p in &op.parameters {
        if p.direction != "In" {
            return Err(Unimplemented(
                "a web service parameter with TransferDirection Out or InOut",
            ));
        }
    }
    Ok(())
}
