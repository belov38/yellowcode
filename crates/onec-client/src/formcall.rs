use roxmltree::{Document, Node};

use std::collections::HashMap;

use crate::answer::{NS_MODULES, NS_VRS, NS_WS, attr, boolean};
use crate::body;
use crate::dcs;
use crate::formdescr::{FormData, FormField, FormTable, NS_UO, Slot, collection_columns};
use crate::info::MemberKind;
use crate::metadata::MdType;
use crate::modules::{FormMethod, ModuleId, ModulesError, parse_form_module_ids};
use crate::object::{Cell, ObjectError, ObjectTable, cell, form_key, implied_type, uo_children};
use crate::report::{self, FormAttribute, OpenedForm};
use crate::session::{RandomSource, Session, SessionError};
use crate::tabular::ValueList;
use crate::value::{NS_CORE, Ref, TypeName, Value};
use crate::write::{
    Input, ObjectChange, UserMessage, field_value, named, same_name, settable, table_xml, value_xml,
};
use crate::xml::{self, escape_attr};

mod state;
use state::{FormState, Kind, Place};

const NS_LF: &str = "http://v8.1c.ru/8.2/managed-application/logform";

#[derive(Debug, Clone, PartialEq)]
pub enum FormTarget {
    Form(String),
    Object { reference: Ref, key: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormAction {
    Command(String),
    Call { name: String, params: Vec<Value> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormStep {
    pub change: ObjectChange,
    pub action: FormAction,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormRequest {
    pub target: FormTarget,
    pub steps: Vec<FormStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodPlace {
    Server,
    ServerWithoutContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerMethod {
    pub name: String,
    pub function: bool,
    pub params: u32,
    pub required: u32,
    pub place: MethodPlace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandRun {
    Server(String),
    ClientCallsServer {
        handler: String,
        method: String,
    },
    Client {
        handler: Option<String>,
        calls: Vec<String>,
        reason: String,
    },
}

impl CommandRun {
    pub fn runs(&self) -> bool {
        !matches!(self, CommandRun::Client { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormCommand {
    pub name: String,
    pub handlers: Vec<String>,
    pub run: CommandRun,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Period {
    pub variant: String,
    pub start: Option<String>,
    pub end: Option<String>,
}

impl std::fmt::Display for Period {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.start, &self.end) {
            (Some(s), Some(e)) => write!(f, "{s}..{e}"),
            _ => f.write_str(&self.variant),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormValue {
    Cell(Cell),
    Period(Period),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Cell(Cell),
    Period(Period),
    Object {
        fields: Vec<(FormField, Cell)>,
        tables: Vec<(FormField, Vec<FormField>, Vec<Vec<Cell>>)>,
    },
    Rows {
        columns: Vec<FormField>,
        rows: Vec<Vec<Cell>>,
    },
    Sheet,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormAttr {
    pub attribute: FormAttribute,
    pub main: bool,
    pub value: AttrValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Changed {
    Value {
        path: String,
        old: Option<FormValue>,
        new: FormValue,
    },
    Rows {
        path: String,
        count: usize,
        columns: Vec<FormField>,
        rows: Vec<(usize, Vec<Cell>)>,
    },
    Sheet {
        path: String,
    },
    Other {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetValue {
    pub path: String,
    pub old: Option<FormValue>,
    pub new: Option<FormValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CallOutcome {
    Done {
        ret: Option<Cell>,
        changed: Vec<Changed>,
    },
    Failed {
        reason: Option<String>,
        text: String,
        changed: Vec<Changed>,
    },
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Called {
    pub method: String,
    pub place: MethodPlace,
    pub command: Option<String>,
    pub set: Vec<SetValue>,
    pub outcome: CallOutcome,
    pub messages: Vec<UserMessage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormView {
    pub form_id: String,
    pub names: Vec<String>,
    pub title: Option<String>,
    pub read_only: bool,
    pub attributes: Vec<FormAttr>,
    pub methods: Vec<ServerMethod>,
    pub commands: Vec<FormCommand>,
    pub opening_messages: Vec<UserMessage>,
    pub calls: Vec<Called>,
    pub refused: Option<String>,
    pub not_run: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum FormError {
    #[error("refused before the call: {0}")]
    Refused(String),
    #[error("{answer}: {what}")]
    Shape { answer: &'static str, what: String },
}

fn shape(answer: &'static str, what: impl Into<String>) -> FormError {
    FormError::Shape {
        answer,
        what: what.into(),
    }
}

fn refused(what: impl Into<String>) -> FormError {
    FormError::Refused(what.into())
}

const QUERY: &str = "logForm/query";

fn ws_children<'a, 'i>(n: Node<'a, 'i>, local: &'a str) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(move |c| {
        c.is_element() && c.tag_name().namespace() == Some(NS_WS) && c.tag_name().name() == local
    })
}

pub fn read_commands(body: &[u8]) -> Result<Vec<(String, Vec<String>)>, FormError> {
    let text = xml::value_text(body);
    let doc = Document::parse(&text).map_err(|e| shape(QUERY, e.to_string()))?;
    let mut out = Vec::new();
    for c in ws_children(doc.root_element(), "command") {
        let name = attr(c, "name").ok_or_else(|| shape(QUERY, "a command without its name"))?;
        let handlers = c
            .descendants()
            .filter(|h| {
                h.is_element()
                    && h.tag_name().namespace() == Some(NS_LF)
                    && h.tag_name().name() == "handler"
            })
            .filter_map(|h| attr(h, "handlerName"))
            .collect();
        out.push((name, handlers));
    }
    Ok(out)
}

pub fn read_methods(body: &[u8]) -> Result<(Vec<ServerMethod>, Option<ModuleId>), FormError> {
    let ids = match parse_form_module_ids(body) {
        Ok(ids) => ids,
        Err(ModulesError::Answer(crate::answer::AnswerError::Missing { field, .. }))
            if field == "moduleIds" =>
        {
            return Ok((Vec::new(), None));
        }
        Err(e) => return Err(shape(QUERY, e.to_string())),
    };
    let method = |m: &FormMethod, place| ServerMethod {
        name: m.name.clone(),
        function: m.kind == MemberKind::Func,
        params: m.params,
        required: m.required(),
        place,
    };
    let mut methods: Vec<ServerMethod> = ids
        .server_methods
        .iter()
        .map(|m| method(m, MethodPlace::Server))
        .collect();
    methods.extend(
        ids.server_methods_without_context
            .iter()
            .map(|m| method(m, MethodPlace::ServerWithoutContext)),
    );
    Ok((methods, Some(ids.context)))
}

fn find_method<'a>(methods: &'a [ServerMethod], name: &str) -> Option<&'a ServerMethod> {
    methods.iter().find(|m| same_name(&m.name, name))
}

fn code_of(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                }
                '"' => {
                    in_string = false;
                    out.push('"');
                }
                '\n' => {
                    out.push('\n');
                    loop {
                        while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
                            chars.next();
                        }
                        let mut ahead = chars.clone();
                        if !(ahead.next() == Some('/') && ahead.next() == Some('/')) {
                            break;
                        }
                        for d in chars.by_ref() {
                            if d == '\n' {
                                out.push('\n');
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push('"');
            }
            '/' if chars.peek() == Some(&'/') => {
                for d in chars.by_ref() {
                    if d == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn words(code: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in code.char_indices() {
        match (is_ident_char(c), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s, &code[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, &code[s..]));
    }
    out
}

fn body_of<'c>(code: &'c str, name: &str) -> Option<&'c str> {
    let w = words(code);
    let starts = ["процедура", "procedure", "функция", "function"];
    let ends = [
        "конецпроцедуры",
        "endprocedure",
        "конецфункции",
        "endfunction",
    ];
    let mut found = None;
    for (i, (_, word)) in w.iter().enumerate() {
        if !starts.contains(&word.to_lowercase().as_str()) {
            continue;
        }
        let Some((at, n)) = w.get(i + 1) else {
            continue;
        };
        if !same_name(n, name) {
            continue;
        }
        let after = &code[at + n.len()..];
        let Some(open) = after.find('(') else {
            continue;
        };
        if !after[..open].trim().is_empty() {
            continue;
        }
        let Some(close) = after[open..].find(')').map(|c| open + c) else {
            continue;
        };
        let mut from = at + n.len() + close + 1;
        let rest = &code[from..];
        let trimmed = rest.trim_start();
        let lead = rest.len() - trimmed.len();
        let first: String = trimmed.chars().take_while(|c| is_ident_char(*c)).collect();
        if ["экспорт", "export"].contains(&first.to_lowercase().as_str()) {
            from += lead + first.len();
        }
        let Some(end) = w
            .iter()
            .skip(i + 2)
            .find(|(p, x)| *p >= from && ends.contains(&x.to_lowercase().as_str()))
        else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        found = Some(&code[from..end.0]);
    }
    found
}

fn command_run(handlers: &[String], methods: &[ServerMethod], client: Option<&str>) -> CommandRun {
    let [handler] = handlers else {
        return CommandRun::Client {
            handler: handlers.first().cloned(),
            calls: Vec::new(),
            reason: if handlers.is_empty() {
                "the command has no handler of the form (a standard command of the platform)"
                    .to_string()
            } else {
                format!(
                    "the command has {} handlers (an extension): not run",
                    handlers.len()
                )
            },
        };
    };
    if let Some(m) = find_method(methods, handler) {
        return CommandRun::Server(m.name.clone());
    }
    let client_only = |calls: Vec<String>, reason: &str| CommandRun::Client {
        handler: Some(handler.clone()),
        calls,
        reason: reason.to_string(),
    };
    let Some(code) = client.map(code_of) else {
        return client_only(
            Vec::new(),
            "the handler runs on the client and the client module was not read",
        );
    };
    let Some(body) = body_of(&code, handler) else {
        return client_only(
            Vec::new(),
            "the handler runs on the client (it is not in the client part of the module)",
        );
    };
    let mut calls: Vec<String> = Vec::new();
    for (at, w) in words(body) {
        let called = body[at + w.len()..].trim_start().starts_with('(');
        if let Some(m) = find_method(methods, w)
            .filter(|m| called && !calls.iter().any(|c| same_name(c, &m.name)))
        {
            calls.push(m.name.clone());
        }
    }
    let statements: Vec<&str> = body
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let alone = match statements.as_slice() {
        [one] => one
            .strip_suffix(')')
            .and_then(|s| s.trim_end().strip_suffix('('))
            .map(str::trim_end),
        _ => None,
    };
    if let Some(m) = alone.and_then(|name| {
        find_method(methods, name).filter(|m| m.required == 0 && name.chars().all(is_ident_char))
    }) {
        return CommandRun::ClientCallsServer {
            handler: handler.clone(),
            method: m.name.clone(),
        };
    }
    let reason = if calls.is_empty() {
        "the handler runs on the client and calls no server method of the form"
    } else {
        "the handler runs on the client and does more than call one server method without parameters"
    };
    client_only(calls, reason)
}

pub fn commands_of(
    query: &[u8],
    methods: &[ServerMethod],
    client: Option<&str>,
) -> Result<Vec<FormCommand>, FormError> {
    Ok(read_commands(query)?
        .into_iter()
        .map(|(name, handlers)| {
            let run = command_run(&handlers, methods, client);
            FormCommand {
                name,
                handlers,
                run,
            }
        })
        .collect())
}

struct Opened {
    fid: String,
    title: Option<String>,
    read_only: bool,
    properties: Vec<(String, bool)>,
}

fn read_opened(body: &[u8]) -> Result<Opened, FormError> {
    let text = xml::value_text(body);
    let doc = Document::parse(&text).map_err(|e| shape(QUERY, e.to_string()))?;
    let root = doc.root_element();
    let fid = ws_children(root, "fid")
        .next()
        .and_then(|n| n.text())
        .ok_or_else(|| shape(QUERY, "no fid"))?
        .to_string();
    let title = ws_children(root, "dd")
        .next()
        .and_then(|dd| {
            dd.descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "attl")
        })
        .and_then(|n| n.text().map(str::to_string));
    let read_only = ws_children(root, "elchs").any(|e| {
        e.descendants().any(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(NS_LF)
                && n.tag_name().name() == "main"
                && n.attribute("readOnly") == Some("true")
        })
    });
    let properties: Vec<(String, bool)> = ws_children(root, "property")
        .map(|p| {
            let id = attr(p, "id").ok_or_else(|| shape(QUERY, "a property without its id"))?;
            Ok((id, p.attribute("main").and_then(boolean) == Some(true)))
        })
        .collect::<Result<_, FormError>>()?;
    Ok(Opened {
        fid,
        title,
        read_only,
        properties,
    })
}

fn props_of<'a, 'i>(
    root: Node<'a, 'i>,
    properties: usize,
    what: &'static str,
) -> Result<Vec<Node<'a, 'i>>, FormError> {
    let values: Vec<Node> = match ws_children(root, "props").next() {
        Some(props) => uo_children(props).collect(),
        None => Vec::new(),
    };
    if values.len() != properties {
        return Err(shape(
            what,
            format!(
                "props has {} values for {} attributes (property)",
                values.len(),
                properties
            ),
        ));
    }
    Ok(values)
}

fn raw_of(n: Node) -> String {
    let doc = n.document();
    let text = doc.input_text();
    text[n.range()].to_string()
}

fn field_cell(n: Node, f: &FormField, types: &[MdType]) -> Cell {
    match cell(n, f, implied_type(&f.ty, types).as_ref()) {
        Ok(c) => c,
        Err(_) if n.children().any(|c| c.is_element()) => Cell::Raw(raw_of(n)),
        Err(_) => Cell::Raw(n.text().unwrap_or("").to_string()),
    }
}

fn shown(n: Node, f: &FormField, types: &[MdType]) -> Option<Cell> {
    cell(n, f, implied_type(&f.ty, types).as_ref()).ok()
}

fn rows_of(
    n: Node,
    columns: &[FormField],
    types: &[MdType],
) -> (Option<usize>, Vec<(usize, Vec<Cell>)>) {
    let count = n.attribute("count").and_then(|c| c.parse().ok());
    let mut rows = Vec::new();
    let items: Vec<Node> = uo_children(n)
        .flat_map(|c| match c.tag_name().name() {
            "item" => vec![c],
            "items" => uo_children(c)
                .filter(|i| i.tag_name().name() == "item")
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    for (k, item) in items.into_iter().enumerate() {
        let holder = uo_children(item)
            .find(|c| c.tag_name().name() == "structure")
            .unwrap_or(item);
        let values: Vec<Node> = uo_children(holder)
            .filter(|v| v.tag_name().name() == "v")
            .collect();
        let index = item
            .attribute("index")
            .and_then(|i| i.parse().ok())
            .unwrap_or(k);
        let row = values
            .iter()
            .zip(columns)
            .map(|(v, c)| field_cell(*v, c, types))
            .collect();
        rows.push((index, row));
    }
    (count, rows)
}

fn own_value(n: Node, a: &FormAttribute, types: &[MdType]) -> AttrValue {
    if a.is_spreadsheet() {
        return AttrValue::Sheet;
    }
    if a.object.is_some() {
        return AttrValue::Other(raw_of(n));
    }
    match shown(n, &a.field, types) {
        Some(c) => AttrValue::Cell(c),
        None => AttrValue::Other(raw_of(n)),
    }
}

fn main_value(n: Node, data: &FormData, types: &[MdType]) -> AttrValue {
    let values: Vec<Node> = uo_children(n)
        .filter(|v| v.tag_name().name() == "v")
        .collect();
    if values.len() != data.layout.len() {
        return AttrValue::Other(raw_of(n));
    }
    let mut fields = Vec::new();
    let mut tables = Vec::new();
    for (v, slot) in values.iter().zip(&data.layout) {
        match *slot {
            Slot::Field(i) => {
                let f = &data.fields[i];
                fields.push((f.clone(), field_cell(*v, f, types)));
            }
            Slot::Table(i) => {
                let t = &data.tables[i];
                let (_, rows) = rows_of(*v, &t.columns, types);
                tables.push((
                    t.field.clone(),
                    t.columns.clone(),
                    rows.into_iter().map(|(_, r)| r).collect(),
                ));
            }
            Slot::Other => {}
        }
    }
    AttrValue::Object { fields, tables }
}

struct Described {
    attributes: Vec<FormAttribute>,
    data: Option<FormData>,
    collections: HashMap<String, Vec<FormField>>,
}

fn described(descr: &[u8], main_id: Option<&str>) -> Result<Described, SessionError> {
    let attributes = report::read_attributes(descr)?;
    let data = match main_id {
        Some(id) => FormData::from_answer_main(descr, id).ok(),
        None => FormData::from_answer(descr, &mut Vec::new()).ok(),
    };
    let collections = collection_columns(descr).map_err(ObjectError::from)?;
    Ok(Described {
        attributes,
        data,
        collections,
    })
}

fn state_of(
    answer: &[u8],
    what: &'static str,
    properties: &[(String, bool)],
    d: &Described,
    types: &[MdType],
) -> Result<FormState, FormError> {
    let text = xml::value_text(answer);
    let doc = Document::parse(&text).map_err(|e| shape(what, e.to_string()))?;
    let root = doc.root_element();
    let (values, properties): (Vec<Node>, Vec<(String, bool)>) =
        if properties.is_empty() && !d.attributes.is_empty() {
            let values = props_of(root, d.attributes.len(), what)?;
            let main = d.data.as_ref().map(|data| data.object.clone());
            let properties = d
                .attributes
                .iter()
                .map(|a| {
                    (
                        a.field.id.clone(),
                        main.as_deref() == a.field.name.as_deref(),
                    )
                })
                .collect();
            (values, properties)
        } else {
            (props_of(root, properties.len(), what)?, properties.to_vec())
        };
    let mut attrs = Vec::new();
    let mut nodes = Vec::new();
    for a in &d.attributes {
        let at = properties.iter().position(|(id, _)| *id == a.field.id);
        attrs.push(FormAttr {
            attribute: a.clone(),
            main: at.is_some_and(|i| properties[i].1),
            value: AttrValue::Other(String::new()),
        });
        nodes.push(at.map(|i| values[i]));
    }
    Ok(FormState::from_values(attrs, &nodes, d, types))
}

type Target<'a> = (usize, Option<(usize, &'a FormField)>, &'a FormField);

fn target_of<'a>(
    name: &str,
    st: &'a FormState,
    data: Option<&'a FormData>,
) -> Result<Target<'a>, FormError> {
    let attrs = &st.attrs;
    let main = attrs.iter().position(|a| a.main);
    let in_main = |field: &str| -> Result<Target<'a>, FormError> {
        let (Some(at), Some(data)) = (main, data) else {
            return Err(refused(format!("{name}: no such attribute of the form")));
        };
        let place = data
            .layout
            .iter()
            .position(|s| matches!(s, Slot::Field(i) if named(&data.fields[*i], field)));
        match place {
            Some(p) => {
                let Slot::Field(i) = data.layout[p] else {
                    unreachable!("the place of a field")
                };
                Ok((at, Some((p, &data.fields[i])), &data.fields[i]))
            }
            None if data.tables.iter().any(|t| named(&t.field, field)) => Err(refused(format!(
                "{name}: a tabular section is changed by its rows ({name}[N].Поле=…, --delete-row, --clear-table)"
            ))),
            None => Err(refused(format!(
                "{name}: no such field of the main attribute"
            ))),
        }
    };
    if let Some((head, field)) = name.split_once('.') {
        if main.is_some_and(|m| named(&attrs[m].attribute.field, head)) {
            return in_main(field);
        }
        return Err(refused(format!(
            "{name}: only the fields of the main attribute are set by a path"
        )));
    }
    if let Some(i) = attrs.iter().position(|a| named(&a.attribute.field, name)) {
        let a = &attrs[i];
        if a.main {
            return Err(refused(format!(
                "{name}: the main attribute is set by its fields ({name}.Поле)"
            )));
        }
        return match st.kinds[i] {
            Kind::Rows(_) => Err(refused(format!(
                "{name}: a value table is changed by its rows ({name}[N].Колонка=…, --delete-row, --clear-table)"
            ))),
            Kind::Value | Kind::Period if a.attribute.object.is_none() => {
                Ok((i, None, &a.attribute.field))
            }
            _ => Err(refused(format!(
                "{name}: an attribute of this type is not set (not implemented)"
            ))),
        };
    }
    in_main(name)
}

fn is_period(f: &FormField) -> bool {
    matches!(f.ty.ids.as_slice(), [one] if one.eq_ignore_ascii_case(state::PERIOD))
}

fn form_value(
    input: &Input,
    field: &FormField,
    types: &[MdType],
) -> Result<(FormValue, String), String> {
    if is_period(field) {
        let Input::Text(t) = input else {
            return Err(
                "a standard period is set by its text: ГГГГ-ММ-ДД..ГГГГ-ММ-ДД or a variant (ThisYear…)"
                    .to_string(),
            );
        };
        let (variant, dates) = dcs::period_parts(t)?;
        let inner = dcs::period(t)?;
        let (start, end) = dates.unzip();
        return Ok((
            FormValue::Period(Period {
                variant: variant.to_string(),
                start,
                end,
            }),
            format!("<uo:v>{inner}</uo:v>"),
        ));
    }
    let implied = implied_type(&field.ty, types);
    let list = TypeName::new(NS_CORE, "ValueListType");
    if implied.as_ref() == Some(&list) {
        let v = match input {
            Input::Value(v @ Value::ValueList(_)) => v.clone(),
            Input::Text(t) if t.is_empty() => Value::ValueList(ValueList::default()),
            _ => {
                return Err(
                    "a value list is set by its typed JSON (--set-json Имя='{\"vl\":{\"items\":[…]}}'), an empty text clears it"
                        .to_string(),
                );
            }
        };
        let x = xml::value_element_as("uo:v", &v, Some(&list)).map_err(|e| e.to_string())?;
        return Ok((FormValue::Cell(Cell::Value(v)), x));
    }
    let v = field_value(input, field, types)?;
    let x = value_xml(&v, implied.is_none()).map_err(|e| e.to_string())?;
    Ok((FormValue::Cell(Cell::Value(v)), x))
}

fn value_now(st: &FormState, at: usize, f: Option<&FormField>) -> Option<FormValue> {
    match (&st.attrs[at].value, f) {
        (AttrValue::Cell(c), None) => Some(FormValue::Cell(c.clone())),
        (AttrValue::Period(p), None) => Some(FormValue::Period(p.clone())),
        (AttrValue::Object { fields, .. }, Some(f)) => fields
            .iter()
            .find(|(x, _)| x.id == f.id)
            .map(|(_, c)| FormValue::Cell(c.clone())),
        _ => None,
    }
}

fn table_of(
    name: &str,
    st: &FormState,
    data: Option<&FormData>,
) -> Result<(Place, FormTable, String), FormError> {
    let main = st.attrs.iter().position(|a| a.main);
    let in_main = |t: &str| -> Result<(Place, FormTable, String), FormError> {
        let (Some(at), Some(data)) = (main, data) else {
            return Err(refused(format!("{name}: no such table of the form")));
        };
        let ti = data
            .tables
            .iter()
            .position(|x| named(&x.field, t))
            .ok_or_else(|| {
                refused(format!(
                    "{name}: no such tabular section of the main attribute"
                ))
            })?;
        let path = format!(
            "{}.{}",
            st.attrs[at].attribute.field.display_name(),
            data.tables[ti].field.display_name()
        );
        Ok(((at, Some(ti)), data.tables[ti].clone(), path))
    };
    if let Some((head, t)) = name.split_once('.') {
        if main.is_some_and(|m| named(&st.attrs[m].attribute.field, head)) {
            return in_main(t);
        }
        return Err(refused(format!(
            "{name}: only the tabular sections of the main attribute are named by a path"
        )));
    }
    if let Some(i) = st
        .attrs
        .iter()
        .position(|a| named(&a.attribute.field, name))
    {
        return match &st.kinds[i] {
            Kind::Rows(columns) => Ok((
                (i, None),
                FormTable {
                    field: st.attrs[i].attribute.field.clone(),
                    columns: columns.clone(),
                },
                st.attrs[i].attribute.field.display_name().to_string(),
            )),
            _ => Err(refused(format!(
                "{name}: the attribute is not a value table"
            ))),
        };
    }
    in_main(name)
}

fn pchs_of(
    change: &ObjectChange,
    st: &FormState,
    d: &Described,
    types: &[MdType],
) -> Result<(String, Vec<SetValue>), FormError> {
    if change.set.is_empty() && change.tables.is_empty() {
        return Ok((report::EMPTY_PCHS.to_string(), Vec::new()));
    }
    let data = d.data.as_ref();
    let attrs = &st.attrs;
    let mut own: Vec<(usize, String)> = Vec::new();
    let mut main: Vec<(usize, String)> = Vec::new();
    let mut main_at = None;
    let mut applied = Vec::new();
    for a in &change.set {
        let (at, in_main, field) = target_of(&a.name, st, data)?;
        settable(field, &a.name).map_err(|e| match e {
            ObjectError::Change(m) => refused(m),
            e => refused(e.to_string()),
        })?;
        let (new, x) =
            form_value(&a.input, field, types).map_err(|e| refused(format!("{}: {e}", a.name)))?;
        let path = match in_main {
            Some((_, f)) => format!(
                "{}.{}",
                attrs[at].attribute.field.display_name(),
                f.display_name()
            ),
            None => field.display_name().to_string(),
        };
        let twice = match in_main {
            Some((p, _)) => {
                main_at = Some(at);
                main.iter().any(|(q, _)| *q == p)
            }
            None => own.iter().any(|(q, _)| *q == at),
        };
        if twice {
            return Err(refused(format!("{}: the attribute is set twice", a.name)));
        }
        match in_main {
            Some((p, _)) => main.push((p, x)),
            None => own.push((at, x)),
        }
        applied.push(SetValue {
            path,
            old: value_now(st, at, in_main.map(|(_, f)| f)),
            new: Some(new),
        });
    }
    let mut ops = 0usize;
    let mut seen: Vec<Place> = Vec::new();
    for t in &change.tables {
        let (place, table, path) = table_of(&t.name, st, data)?;
        if seen.contains(&place) {
            return Err(refused(format!("{}: the table is given twice", t.name)));
        }
        seen.push(place);
        settable(&table.field, &t.name).map_err(|e| match e {
            ObjectError::Change(m) => refused(m),
            e => refused(e.to_string()),
        })?;
        let rows = st.rows(place).ok_or_else(|| {
            refused(format!(
                "{}: the rows of the table are not known after an earlier answer",
                t.name
            ))
        })?;
        let read = ObjectTable {
            field: table.field.clone(),
            columns: table.columns.clone(),
            rows: rows.cells(),
        };
        let raw = rows.raw_table(&table.columns, types);
        let mut edit = t.clone();
        edit.name = table.field.display_name().to_string();
        let (x, n, changes) =
            table_xml(&raw, &table, Some(&read), Some(&edit), types).map_err(|e| match e {
                ObjectError::Change(m) => refused(m),
                e => refused(e.to_string()),
            })?;
        ops += n;
        let short = table.field.display_name();
        applied.extend(changes.into_iter().map(|c| SetValue {
            path: match c.path.strip_prefix(short) {
                Some(rest) => format!("{path}{rest}"),
                None => c.path,
            },
            old: c.old.map(FormValue::Cell),
            new: c.new.map(|v| FormValue::Cell(Cell::Value(v))),
        }));
        match place {
            (at, Some(ti)) => {
                main_at = Some(at);
                let slot = data
                    .and_then(|d| d.layout.iter().position(|s| *s == Slot::Table(ti)))
                    .ok_or_else(|| refused(format!("{}: no slot of the table", t.name)))?;
                main.push((slot, x));
            }
            (at, None) => own.push((at, x)),
        }
    }
    let mut values: Vec<(usize, String)> = own;
    if let Some(at) = main_at {
        main.sort_by_key(|(p, _)| *p);
        let places: Vec<String> = main.iter().map(|(p, _)| p.to_string()).collect();
        let body: String = main.into_iter().map(|(_, x)| x).collect();
        values.push((
            at,
            format!("<uo:v fields=\"{}\">{body}</uo:v>", places.join(" ")),
        ));
    }
    values.sort_by_key(|(p, _)| *p);
    let places: Vec<String> = values.iter().map(|(p, _)| p.to_string()).collect();
    let body: String = values.into_iter().map(|(_, x)| x).collect();
    let fields = if attrs.len() > 1 {
        format!(" fields=\"{}\"", places.join(" "))
    } else {
        String::new()
    };
    let (sin, seq) = if ops > 0 {
        (st.sin + 1, st.seq + ops as u64)
    } else {
        (st.sin, st.seq)
    };
    Ok((
        format!(
            "<pchs sinDesc=\"0\" sin=\"{sin}\" seq=\"{seq}\"><uo:structure{fields}>{body}</uo:structure></pchs>"
        ),
        applied,
    ))
}

pub fn call_without_context_request(
    form_id: &str,
    fid: &str,
    method: &str,
    params: &[Value],
) -> Result<Vec<u8>, FormError> {
    let params = xml::params_xml(params).map_err(|e| refused(e.to_string()))?;
    Ok(format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"{NS_WS}\" \
         xmlns:cfg=\"http://v8.1c.ru/8.1/data/enterprise/current-config\" xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         id=\"{}\" fid=\"{}\"><generic xmlns:m=\"{NS_MODULES}\" xmlns:uo=\"{NS_UO}\" name=\"{}\">{params}</generic>\
         </request>",
        escape_attr(form_id),
        escape_attr(fid),
        escape_attr(method)
    )
    .into_bytes())
}

enum Answer {
    Done {
        ret: Option<Cell>,
    },
    Failed {
        reason: Option<String>,
        text: String,
    },
}

const CALL: &str = "logForm/call";

fn ret_of<'a, 'i>(n: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
    n.children().find(|c| {
        c.is_element()
            && c.tag_name().namespace() == Some(NS_MODULES)
            && c.tag_name().name() == "ret"
    })
}

fn read_answer(status: u16, body: &[u8]) -> Result<Answer, FormError> {
    let text = xml::value_text(body);
    let doc = Document::parse(&text).map_err(|e| shape(CALL, e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().namespace() == Some(NS_VRS) && root.tag_name().name() == "exception" {
        let reason = attr(root, "reason");
        let text = xml::parse_exception(body);
        let text = match &reason {
            Some(r) => text
                .strip_prefix(&format!("{r}: "))
                .map(str::to_string)
                .unwrap_or(text),
            None => text,
        };
        return Ok(Answer::Failed { reason, text });
    }
    if status != 200 {
        return Err(shape(CALL, format!("HTTP {status} without an exception")));
    }
    if root.tag_name().namespace() != Some(NS_WS) {
        return Err(shape(
            CALL,
            format!("the root is <{}>", root.tag_name().name()),
        ));
    }
    let ret = ret_of(root).or_else(|| ws_children(root, "prm").next().and_then(ret_of));
    let ret = ret.map(|n| match xml::typed_value(n, None) {
        Ok(v) => Cell::Value(v),
        Err(_) => Cell::Raw(n.text().unwrap_or("").to_string()),
    });
    Ok(Answer::Done { ret })
}

fn changes_of(
    answer: &[u8],
    attrs: &[FormAttr],
    data: Option<&FormData>,
    types: &[MdType],
) -> Result<Vec<Changed>, FormError> {
    let text = xml::value_text(answer);
    let doc = Document::parse(&text).map_err(|e| shape(CALL, e.to_string()))?;
    let Some(structure) = ws_children(doc.root_element(), "pchs")
        .next()
        .and_then(|p| uo_children(p).find(|c| c.tag_name().name() == "structure"))
    else {
        return Ok(Vec::new());
    };
    let values: Vec<Node> = uo_children(structure)
        .filter(|v| v.tag_name().name() == "v")
        .collect();
    let places = positions(structure, values.len(), CALL)?;
    let mut out = Vec::new();
    for (place, v) in places.into_iter().zip(values) {
        let Some(a) = attrs.get(place) else {
            return Err(shape(
                CALL,
                format!("pchs names the attribute {place} of {}", attrs.len()),
            ));
        };
        let name = a.attribute.field.display_name().to_string();
        match (&a.value, data, a.main) {
            (AttrValue::Object { fields, .. }, Some(data), true) => {
                main_changes(v, &name, fields, data, types, &mut out)?;
            }
            (AttrValue::Rows { columns, .. }, _, _) => {
                if table_changed(v) {
                    let (count, rows) = rows_of(v, columns, types);
                    out.push(Changed::Rows {
                        path: name,
                        count: count.unwrap_or(rows.len()),
                        columns: columns.clone(),
                        rows,
                    });
                }
            }
            (AttrValue::Period(old), _, _) => {
                let new = state::period_of(v);
                match new {
                    Some(p) if p == *old => {}
                    Some(p) => out.push(Changed::Value {
                        path: name,
                        old: Some(FormValue::Period(old.clone())),
                        new: FormValue::Period(p),
                    }),
                    None => out.push(Changed::Other { path: name }),
                }
            }
            _ if a.attribute.is_spreadsheet() => {
                let changed = v
                    .children()
                    .filter(|c| c.is_element() && c.tag_name().name() == "changes")
                    .any(|c| {
                        c.children()
                            .any(|n| n.is_element() && n.tag_name().name() != "commonPart")
                    });
                if changed {
                    out.push(Changed::Sheet { path: name });
                }
            }
            (old, _, _) => match own_value(v, &a.attribute, types) {
                AttrValue::Other(_) if v.attribute("count").is_some() && !table_changed(v) => {}
                AttrValue::Other(new) if matches!(old, AttrValue::Other(o) if *o == new) => {}
                AttrValue::Cell(new) => {
                    let old = match old {
                        AttrValue::Cell(c) => Some(c.clone()),
                        _ => None,
                    };
                    if old.as_ref() != Some(&new) {
                        out.push(Changed::Value {
                            path: name,
                            old: old.map(FormValue::Cell),
                            new: FormValue::Cell(new),
                        });
                    }
                }
                _ => out.push(Changed::Other { path: name }),
            },
        }
    }
    Ok(out)
}

fn table_changed(n: Node) -> bool {
    n.attribute("fullChanged") == Some("true")
        || uo_children(n).any(|c| match c.tag_name().name() {
            "operations" | "items" => c.children().any(|x| x.is_element()),
            "item" => true,
            _ => false,
        })
}

fn diff(before: &[FormAttr], after: &[FormAttr]) -> Vec<Changed> {
    let mut out = Vec::new();
    let rows = |path: String, columns: &[FormField], rows: &[Vec<Cell>]| Changed::Rows {
        path,
        count: rows.len(),
        columns: columns.to_vec(),
        rows: rows.iter().cloned().enumerate().collect(),
    };
    for (b, a) in before.iter().zip(after) {
        let name = a.attribute.field.display_name().to_string();
        match (&b.value, &a.value) {
            (x, y) if x == y => {}
            (AttrValue::Cell(x), AttrValue::Cell(y)) => out.push(Changed::Value {
                path: name,
                old: Some(FormValue::Cell(x.clone())),
                new: FormValue::Cell(y.clone()),
            }),
            (AttrValue::Period(x), AttrValue::Period(y)) => out.push(Changed::Value {
                path: name,
                old: Some(FormValue::Period(x.clone())),
                new: FormValue::Period(y.clone()),
            }),
            (AttrValue::Rows { .. }, AttrValue::Rows { columns, rows: r }) => {
                out.push(rows(name, columns, r));
            }
            (
                AttrValue::Object {
                    fields: bf,
                    tables: bt,
                },
                AttrValue::Object { fields, tables },
            ) => {
                for (f, c) in fields {
                    let old = bf.iter().find(|(g, _)| g.id == f.id).map(|(_, c)| c);
                    if old != Some(c) {
                        out.push(Changed::Value {
                            path: format!("{name}.{}", f.display_name()),
                            old: old.cloned().map(FormValue::Cell),
                            new: FormValue::Cell(c.clone()),
                        });
                    }
                }
                for (t, columns, r) in tables {
                    let old = bt.iter().find(|(g, _, _)| g.id == t.id).map(|(_, _, r)| r);
                    if old != Some(r) {
                        out.push(rows(format!("{name}.{}", t.display_name()), columns, r));
                    }
                }
            }
            (_, AttrValue::Sheet) => {}
            _ => out.push(Changed::Other { path: name }),
        }
    }
    out
}

fn positions(n: Node, count: usize, answer: &'static str) -> Result<Vec<usize>, FormError> {
    let places: Vec<usize> = match attr(n, "fields") {
        Some(f) => f
            .split_whitespace()
            .map(|p| {
                p.parse()
                    .map_err(|_| shape(answer, format!("bad pchs fields {p:?}")))
            })
            .collect::<Result<_, _>>()?,
        None => (0..count).collect(),
    };
    if places.len() != count {
        return Err(shape(
            answer,
            format!("pchs: {} fields, {count} values", places.len()),
        ));
    }
    Ok(places)
}

fn main_changes(
    v: Node,
    name: &str,
    before: &[(FormField, Cell)],
    data: &FormData,
    types: &[MdType],
    out: &mut Vec<Changed>,
) -> Result<(), FormError> {
    let values: Vec<Node> = uo_children(v)
        .filter(|x| x.tag_name().name() == "v")
        .collect();
    let places = positions(v, values.len(), CALL)?;
    for (p, x) in places.into_iter().zip(values) {
        match data.layout.get(p) {
            Some(Slot::Field(i)) => {
                let f = &data.fields[*i];
                let new = field_cell(x, f, types);
                let old = before
                    .iter()
                    .find(|(b, _)| b.id == f.id)
                    .map(|(_, c)| c.clone());
                if old.as_ref() != Some(&new) {
                    out.push(Changed::Value {
                        path: format!("{name}.{}", f.display_name()),
                        old: old.map(FormValue::Cell),
                        new: FormValue::Cell(new),
                    });
                }
            }
            Some(Slot::Table(_)) if !table_changed(x) => {}
            Some(Slot::Table(i)) => {
                let t = &data.tables[*i];
                let (count, rows) = rows_of(x, &t.columns, types);
                out.push(Changed::Rows {
                    path: format!("{name}.{}", t.field.display_name()),
                    count: count.unwrap_or(rows.len()),
                    columns: t.columns.clone(),
                    rows,
                });
            }
            Some(Slot::Other) => {}
            None => {
                return Err(shape(
                    CALL,
                    format!(
                        "pchs names the field {p} of {} of {name}",
                        data.layout.len()
                    ),
                ));
            }
        }
    }
    Ok(())
}

type Planned = (ServerMethod, Vec<Value>, Option<String>);

fn method_of(
    action: &FormAction,
    methods: &[ServerMethod],
    commands: &[FormCommand],
    has_module: bool,
) -> Result<Planned, FormError> {
    match action {
        FormAction::Call { name, params } => {
            let m = match find_method(methods, name) {
                Some(m) => m.clone(),
                None if !has_module => ServerMethod {
                    name: name.clone(),
                    function: false,
                    params: params.len() as u32,
                    required: 0,
                    place: MethodPlace::Server,
                },
                None => {
                    let names: Vec<&str> = methods.iter().map(|m| m.name.as_str()).collect();
                    return Err(refused(format!(
                        "{name} is not a server method of the form (a client procedure is not called); server methods: {}",
                        if names.is_empty() {
                            "none".to_string()
                        } else {
                            names.join(", ")
                        }
                    )));
                }
            };
            if has_module
                && (params.len() > m.params as usize || params.len() < m.required as usize)
            {
                return Err(refused(format!(
                    "{}: {} parameters for a method of {} ({} required)",
                    m.name,
                    params.len(),
                    m.params,
                    m.required
                )));
            }
            Ok((m, params.clone(), None))
        }
        FormAction::Command(name) => {
            let c = commands
                .iter()
                .find(|c| same_name(&c.name, name))
                .ok_or_else(|| {
                    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
                    refused(format!(
                        "{name}: no such command of the form; commands: {}",
                        if names.is_empty() {
                            "none".to_string()
                        } else {
                            names.join(", ")
                        }
                    ))
                })?;
            let method = match &c.run {
                CommandRun::Server(m) | CommandRun::ClientCallsServer { method: m, .. } => m,
                CommandRun::Client { calls, reason, .. } => {
                    let hint = if calls.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; the server methods it calls: {} (remote form --call, with the parameters the handler passes)",
                            calls.join(", ")
                        )
                    };
                    return Err(refused(format!(
                        "the command {} runs on the client and is not run: {reason}{hint}",
                        c.name
                    )));
                }
            };
            let m = find_method(methods, method)
                .cloned()
                .ok_or_else(|| refused(format!("{method}: not a server method of the form")))?;
            Ok((m, Vec::new(), Some(c.name.clone())))
        }
    }
}

fn synch_request(key: &str) -> Vec<u8> {
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"{NS_WS}\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
         <key>{}</key></request>",
        xml::escape_text(key)
    )
    .into_bytes()
}

const SYNCH: &str = "logForm/synchException";

fn synch_counters(answer: &[u8]) -> Result<(u64, u64), FormError> {
    let text = xml::value_text(answer);
    let doc = Document::parse(&text).map_err(|e| shape(SYNCH, e.to_string()))?;
    let d = ws_children(doc.root_element(), "descriptions")
        .next()
        .ok_or_else(|| shape(SYNCH, "no descriptions"))?;
    let counter = |name: &str| {
        d.attribute(name)
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| shape(SYNCH, format!("descriptions without {name}")))
    };
    Ok((counter("sinUo")?, counter("seqUo")?))
}

fn step_text(a: &FormAction) -> String {
    match a {
        FormAction::Command(c) => format!("command {c}"),
        FormAction::Call { name, .. } => format!("call {name}"),
    }
}

impl<R: RandomSource> Session<R> {
    pub fn run_form(&mut self, req: &FormRequest) -> Result<FormView, SessionError> {
        let conf = self.prologue(true)?.conf;
        let types = self.session_types("the types of the values of the form")?;
        let (key, reference) = match &req.target {
            FormTarget::Form(k) => (k.clone(), None),
            FormTarget::Object { reference, key } => (
                match key {
                    Some(k) => k.clone(),
                    None => form_key(reference)?,
                },
                Some(reference),
            ),
        };
        let (fid, answer) = self.open_form(&key, reference)?;
        let work = self.form_in(&conf, &answer, &types, req, reference);
        match work {
            Ok(mut v) => {
                if let Err(e) = self.close_form(&fid) {
                    v.warnings.push(format!("the form was not closed: {e}"));
                }
                Ok(v)
            }
            Err(e) => self.closing(&fid, Err(e)),
        }
    }

    fn form_in(
        &mut self,
        conf: &str,
        answer: &[u8],
        types: &[MdType],
        req: &FormRequest,
        reference: Option<&Ref>,
    ) -> Result<FormView, SessionError> {
        let opened = read_opened(answer)?;
        let form = report::read_opened(answer)?;
        let descr = self.form_description(conf, answer)?;
        let main_id = opened
            .properties
            .iter()
            .find(|(_, m)| *m)
            .map(|(id, _)| id.as_str());
        let d = described(&descr, main_id)?;
        let mut st = state_of(answer, QUERY, &opened.properties, &d, types)?;
        let (methods, client_id) = read_methods(answer)?;
        let mut warnings = Vec::new();
        let names = read_commands(answer)?;
        let needs_client = names.iter().any(|(_, h)| match h.as_slice() {
            [one] => find_method(&methods, one).is_none(),
            _ => false,
        });
        let client = match (&client_id, needs_client) {
            (Some(id), true) => match self.module_source(id) {
                Ok(s) => Some(s.text),
                Err(e) => {
                    warnings.push(format!("the client module of the form was not read: {e}"));
                    None
                }
            },
            _ => None,
        };
        let client_text = client.as_ref().map(|b| xml::value_text(b));
        let commands = commands_of(answer, &methods, client_text.as_deref())?;
        if let Some((r, attr)) = reference.zip(st.attrs.iter().find(|a| a.main)) {
            check_reference(r, attr)?;
        }
        let planned = req
            .steps
            .iter()
            .map(|s| method_of(&s.action, &methods, &commands, client_id.is_some()))
            .collect::<Result<Vec<_>, _>>()?;
        let opening_messages = match self.user_messages() {
            Ok(m) => m,
            Err(e) => {
                warnings.push(format!("the messages of the opening were not read: {e}"));
                Vec::new()
            }
        };
        let mut v = FormView {
            form_id: form.id.clone(),
            names: form.names.clone(),
            title: opened.title.clone(),
            read_only: opened.read_only,
            attributes: st.attrs.clone(),
            methods,
            commands,
            opening_messages,
            calls: Vec::new(),
            refused: None,
            not_run: Vec::new(),
            warnings,
        };
        for (k, (step, (method, params, command))) in req.steps.iter().zip(planned).enumerate() {
            let later = || {
                req.steps[k + 1..]
                    .iter()
                    .map(|s| step_text(&s.action))
                    .collect()
            };
            let prepared = self.prepare_call(
                &form,
                &opened,
                &d,
                types,
                &mut st,
                step,
                &method,
                &params,
                &mut v.warnings,
            );
            let (body, set) = match prepared {
                Ok(p) => p,
                Err(e) if k == 0 => return Err(e),
                Err(e) => {
                    let text = match e {
                        SessionError::Form(FormError::Refused(m)) => m,
                        e => e.to_string(),
                    };
                    v.refused = Some(format!("{}: {text}", step_text(&step.action)));
                    v.not_run = req.steps[k..]
                        .iter()
                        .map(|s| step_text(&s.action))
                        .collect();
                    break;
                }
            };
            let outcome = self.send_call(&form, &d, types, &mut st, &method, body, &mut v.warnings);
            let messages = match self.user_messages() {
                Ok(m) => m,
                Err(e) => {
                    v.warnings
                        .push(format!("the messages to the user were not read: {e}"));
                    Vec::new()
                }
            };
            let stop = !matches!(outcome, CallOutcome::Done { .. });
            v.calls.push(Called {
                method: method.name,
                place: method.place,
                command,
                set,
                outcome,
                messages,
            });
            if stop {
                v.not_run = later();
                break;
            }
        }
        Ok(v)
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_call(
        &mut self,
        form: &OpenedForm,
        opened: &Opened,
        d: &Described,
        types: &[MdType],
        st: &mut FormState,
        step: &FormStep,
        method: &ServerMethod,
        params: &[Value],
        warnings: &mut Vec<String>,
    ) -> Result<(Vec<u8>, Vec<SetValue>), SessionError> {
        Ok(match method.place {
            MethodPlace::Server => {
                let (pchs, set) = pchs_of(&step.change, st, d, types)?;
                if pchs != report::EMPTY_PCHS {
                    st.apply(&state::request_doc(&pchs), d, types, warnings)?;
                }
                let body =
                    report::call_request_dd(form, &method.name, params, &pchs, st.dd.as_deref())?;
                (body, set)
            }
            MethodPlace::ServerWithoutContext => {
                if !(step.change.set.is_empty() && step.change.tables.is_empty()) {
                    return Err(refused(format!(
                        "{} is a server method without context: it does not see the attributes of the form, they are not set",
                        method.name
                    ))
                    .into());
                }
                (
                    call_without_context_request(&form.id, &opened.fid, &method.name, params)?,
                    Vec::new(),
                )
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn send_call(
        &mut self,
        form: &OpenedForm,
        d: &Described,
        types: &[MdType],
        st: &mut FormState,
        method: &ServerMethod,
        body: Vec<u8>,
        warnings: &mut Vec<String>,
    ) -> CallOutcome {
        let (step, path) = match method.place {
            MethodPlace::Server => ("logForm/call", "/e1cib/logForm?cmd=call"),
            MethodPlace::ServerWithoutContext => {
                ("logForm/callWOContext", "/e1cib/logForm?cmd=callWOContext")
            }
        };
        let req_body = body::zstd_compress(&body);
        let h = self.headers("application/xml", true, true);
        let resp = match self.step_once(step, "POST", path, &h, &req_body, &[200, 400, 500]) {
            Ok(r) => r,
            Err(e) => return CallOutcome::Unknown(e.to_string()),
        };
        let decoded = match Self::decoded(&resp) {
            Ok(b) => b,
            Err(e) => return CallOutcome::Unknown(e.to_string()),
        };
        match read_answer(resp.status, &decoded) {
            Err(e) => CallOutcome::Unknown(e.to_string()),
            Ok(Answer::Done { ret }) => {
                let changed = match changes_of(&decoded, &st.attrs, d.data.as_ref(), types) {
                    Ok(c) => c,
                    Err(e) => return CallOutcome::Unknown(e.to_string()),
                };
                if method.place == MethodPlace::Server
                    && let Err(e) = st.apply(&xml::value_text(&decoded), d, types, warnings)
                {
                    warnings.push(format!(
                        "the data of the form are not followed after the call: {e}"
                    ));
                }
                CallOutcome::Done { ret, changed }
            }
            Ok(Answer::Failed { reason, text }) => {
                let changed = if method.place == MethodPlace::Server {
                    match self.synch(&form.remote_key, d, types) {
                        Ok(mut after) => {
                            let changed = diff(&st.attrs, &after.attrs);
                            if after.dd.is_none() {
                                after.dd = st.dd.take();
                            }
                            *st = after;
                            changed
                        }
                        Err(e) => {
                            warnings.push(format!(
                                "the data of the form after the exception were not read: {e}"
                            ));
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                CallOutcome::Failed {
                    reason,
                    text,
                    changed,
                }
            }
        }
    }

    fn synch(
        &mut self,
        key: &str,
        d: &Described,
        types: &[MdType],
    ) -> Result<FormState, SessionError> {
        let req = body::zstd_compress(&synch_request(key));
        let h = self.headers("application/xml", true, true);
        let resp = self.step(
            SYNCH,
            "POST",
            "/e1cib/logForm?cmd=synchException",
            &h,
            &req,
            &[200],
        )?;
        let answer = Self::decoded(&resp)?;
        let mut st = state_of(&answer, SYNCH, &[], d, types)?;
        st.apply(&xml::value_text(&answer), d, types, &mut Vec::new())?;
        (st.sin, st.seq) = synch_counters(&answer)?;
        Ok(st)
    }
}

fn check_reference(r: &Ref, main: &FormAttr) -> Result<(), SessionError> {
    let AttrValue::Object { fields, .. } = &main.value else {
        return Ok(());
    };
    let shown = fields
        .iter()
        .find(|(f, _)| f.name.as_deref() == Some("Ref"))
        .map(|(_, c)| c);
    match shown {
        Some(Cell::Value(Value::Ref(got))) if got.uuid() == r.uuid() => Ok(()),
        Some(c) => Err(ObjectError::OtherObject {
            asked: format!("{}({})", r.type_name(), r.uuid()),
            got: match c {
                Cell::Value(Value::Ref(g)) => format!("{}({})", g.type_name(), g.uuid()),
                other => format!("{other:?}"),
            },
        }
        .into()),
        None => Ok(()),
    }
}
