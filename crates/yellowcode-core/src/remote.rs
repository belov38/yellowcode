use std::io::{BufRead, Write};
use std::path::PathBuf;

mod info;
mod list;
mod metadata;
mod modules;
mod object;
mod report;
mod values;

pub use object::{ObjectRef, ObjectRequest};
pub use report::ReportFormat;

pub use onec_client::Uuid;
use onec_client::auth::printable;
pub use onec_client::session::is_build;
use onec_client::session::{Session, client_nm, login_query, new_cln_id};
use onec_client::{
    Array, CallResult, Credentials, Date, Decimal, EnumPresentation, EnumPresentations, EnumRef,
    HttpTransport, InfoOptions, Map, MetadataOptions, ModuleId, Presentation, Presentations,
    ReportOptions, SessionError, Structure, Value,
};

use crate::Result;
use crate::output::Out;

pub use list::{DEFAULT_LIMIT, DEFAULT_PAGE_SIZE, ListRequest};
pub use onec_client::dlist::{Filter, ListError, Order};

#[derive(Clone, PartialEq, Eq)]
pub enum Password {
    Env(String),
    Value(String),
    Stdin,
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Password::Env(var) => f.debug_tuple("Env").field(var).finish(),
            Password::Value(_) => f.write_str("Value(<hidden>)"),
            Password::Stdin => f.write_str("Stdin"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Login {
    pub url: String,
    pub user: Option<String>,
    pub password: Password,
    pub strong: bool,
    pub server_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param(Value);

impl Param {
    pub fn parse(arg: &str) -> std::result::Result<Param, String> {
        parse_param(arg).map(Param)
    }

    pub fn parse_json(arg: &str) -> std::result::Result<Param, String> {
        parse_param_json(arg).map(Param)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleUrn(ModuleId);

impl ModuleUrn {
    pub fn parse(arg: &str) -> std::result::Result<ModuleUrn, String> {
        ModuleId::parse(arg)
            .map(ModuleUrn)
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct CallRequest {
    pub login: Login,
    pub call: String,
    pub params: Vec<Param>,
    pub cold: bool,
    pub json: bool,
    pub presentations: bool,
}

#[derive(Debug, Clone)]
pub struct InfoRequest {
    pub login: Login,
    pub members: bool,
    pub sections: bool,
    pub auth_settings: bool,
    pub auth_settings_only: bool,
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct MetadataRequest {
    pub login: Login,
    pub enum_synonyms: bool,
    pub attributes: bool,
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct ModulesRequest {
    pub login: Login,
    pub out: Option<PathBuf>,
    pub ids: Vec<ModuleUrn>,
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct ReportRequest {
    pub login: Login,
    pub form: String,
    pub call: Option<String>,
    pub params: Vec<Param>,
    pub attribute: Option<String>,
    pub format: ReportFormat,
}

#[derive(Debug, Clone)]
pub enum Request {
    Call(CallRequest),
    Info(InfoRequest),
    Metadata(MetadataRequest),
    Modules(ModulesRequest),
    Object(ObjectRequest),
    List(ListRequest),
    Report(ReportRequest),
}

pub fn run(
    req: Request,
    out: &mut Out,
    err: &mut dyn Write,
    input: &mut dyn BufRead,
) -> Result<i32> {
    match req {
        Request::Call(r) => run_call(r, out, err, input),
        Request::Info(r) => run_info(r, out, err, input),
        Request::Metadata(r) => run_metadata(r, out, err, input),
        Request::Modules(r) => modules::run(r, out, input),
        Request::Object(r) => object::run(r, out, err, input),
        Request::List(r) => list::run(r, out, err, input),
        Request::Report(r) => run_report(r, out, err, input),
    }
}

fn run_report(
    r: ReportRequest,
    out: &mut Out,
    err: &mut dyn Write,
    input: &mut dyn BufRead,
) -> Result<i32> {
    let (session, login) = open_session(&r.login, input)?;
    let params: Vec<Value> = r.params.into_iter().map(|Param(v)| v).collect();
    let (form, call, attribute, format) = (r.form, r.call, r.attribute, r.format);
    let (session, outcome) = on_value_stack(move || {
        let mut session = session;
        let outcome = login.and_then(|()| {
            session.run_report(ReportOptions {
                form: &form,
                call: call.as_deref().map(|c| (c, params.as_slice())),
                limits: onec_client::spreadsheet::DEFAULT_LIMITS,
            })
        });
        let _ = session.logout();
        (session, outcome)
    })?;
    match outcome {
        Ok(view) => {
            let lines = report::report_lines(&view, attribute.as_deref(), format, session.steps());
            match lines {
                Ok(lines) => {
                    for line in lines {
                        writeln!(out, "{line}")?;
                    }
                    if format == ReportFormat::Text {
                        writeln!(out, "RESULT OK {}", steps_line(&session))?;
                    }
                    Ok(0)
                }
                Err(e) => {
                    result_fail(format, &session, out, err)?;
                    Err(e.into())
                }
            }
        }
        Err(e) => {
            if format == ReportFormat::Text {
                users_hint(&r.login.user, &e, out)?;
            } else {
                let mut buf = Vec::new();
                users_hint(
                    &r.login.user,
                    &e,
                    &mut Out {
                        w: &mut buf,
                        closed: false,
                    },
                )?;
                err.write_all(&buf)?;
            }
            result_fail(format, &session, out, err)?;
            Err(printable(&e.to_string()).into())
        }
    }
}

fn result_fail(
    format: ReportFormat,
    session: &Session,
    out: &mut Out,
    err: &mut dyn Write,
) -> Result<()> {
    let line = format!("RESULT FAIL {}", steps_line(session));
    if format == ReportFormat::Text {
        writeln!(out, "{line}")?;
    } else {
        writeln!(err, "{line}")?;
    }
    Ok(())
}

fn password_of(
    login: &Login,
    get_env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
    input: &mut dyn BufRead,
) -> Result<Option<String>> {
    if login.user.is_none() {
        return Ok(None);
    }
    match &login.password {
        Password::Env(var) => match get_env(var) {
            Ok(v) => Ok(Some(v)),
            Err(std::env::VarError::NotPresent) => {
                Err(format!("environment variable {var} is not set").into())
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("environment variable {var} is not valid Unicode").into())
            }
        },
        Password::Value(v) => Ok(Some(v.clone())),
        Password::Stdin => {
            let mut line = String::new();
            let read = input
                .read_line(&mut line)
                .map_err(|e| format!("reading password from stdin: {e}"))?;
            if read == 0 {
                return Err(
                    "no password on stdin: the input ended before a line (an empty password is an \
                            empty line)"
                        .into(),
                );
            }
            Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
        }
    }
}

fn run_call(
    r: CallRequest,
    out: &mut Out,
    err: &mut dyn Write,
    input: &mut dyn BufRead,
) -> Result<i32> {
    let (session, login) = open_session(&r.login, input)?;
    let opts = CallOptions {
        cold: r.cold,
        json: r.json,
        presentations: r.presentations,
    };
    let params: Vec<Value> = r.params.into_iter().map(|Param(v)| v).collect();
    let call = r.call;
    let report = on_value_stack(move || call_report(session, login, &call, &params, opts))?;
    for line in report.lines {
        writeln!(out, "{line}")?;
    }
    if let Some(w) = report.warning {
        writeln!(err, "{w}")?;
    }
    report.outcome.map(|()| 0)
}

fn run_info(
    r: InfoRequest,
    out: &mut Out,
    err: &mut dyn Write,
    input: &mut dyn BufRead,
) -> Result<i32> {
    if r.auth_settings_only {
        return auth_settings_only_report(&r.login.url, r.json, out);
    }
    let (mut session, login) = open_session(&r.login, input)?;
    let opts = InfoOptions {
        members: r.members,
        sections: r.sections,
        auth_settings: r.auth_settings,
    };
    let outcome = login.and_then(|()| session.run_info(opts));
    let _ = session.logout();
    match outcome {
        Ok(view) => {
            let (lines, warning) =
                info_report(view, session.steps(), &steps_line(&session), r.json)?;
            for line in lines {
                writeln!(out, "{line}")?;
            }
            if let Some(w) = warning {
                writeln!(err, "{w}")?;
            }
            Ok(0)
        }
        Err(e) => {
            users_hint(&r.login.user, &e, out)?;
            writeln!(out, "RESULT FAIL {}", steps_line(&session))?;
            Err(e.into())
        }
    }
}

fn run_metadata(
    r: MetadataRequest,
    out: &mut Out,
    err: &mut dyn Write,
    input: &mut dyn BufRead,
) -> Result<i32> {
    let (mut session, login) = open_session(&r.login, input)?;
    let opts = MetadataOptions {
        enum_synonyms: r.enum_synonyms,
        attributes: r.attributes,
    };
    let outcome = login.and_then(|()| session.run_metadata_with(opts));
    let _ = session.logout();
    match outcome {
        Ok(view) => {
            let (lines, warning) =
                metadata_report(view, session.steps(), &steps_line(&session), r.json)?;
            for line in lines {
                writeln!(out, "{line}")?;
            }
            if let Some(w) = warning {
                writeln!(err, "{w}")?;
            }
            Ok(0)
        }
        Err(e) => {
            users_hint(&r.login.user, &e, out)?;
            writeln!(out, "RESULT FAIL {}", steps_line(&session))?;
            Err(e.into())
        }
    }
}

fn metadata_report(
    view: onec_client::MetadataView,
    steps: &[onec_client::Step],
    steps_line: &str,
    json: bool,
) -> Result<(Vec<String>, Option<String>)> {
    let error = view.enum_synonyms_error.map(|e| e.to_string());
    if json {
        let j = metadata::metadata_json(&view.metadata, steps, error.as_deref());
        return Ok((vec![serde_json::to_string_pretty(&j)?], None));
    }
    let mut lines = metadata::metadata_lines(&view.metadata);
    lines.push(format!("RESULT OK {steps_line}"));
    Ok((
        lines,
        error.map(|e| format!("warning: enum synonyms not shown: {e}")),
    ))
}

fn info_report(
    view: onec_client::InfoView,
    steps: &[onec_client::Step],
    steps_line: &str,
    json: bool,
) -> Result<(Vec<String>, Option<String>)> {
    let error = view.auth_settings_error.map(|e| e.to_string());
    if json {
        let j = info::info_json(&view.info, steps, error.as_deref());
        return Ok((vec![serde_json::to_string_pretty(&j)?], None));
    }
    let mut lines = info::info_lines(&view.info);
    lines.push(format!("RESULT OK {steps_line}"));
    Ok((
        lines,
        error.map(|e| format!("warning: auth settings not shown: {}", printable(&e))),
    ))
}

fn auth_settings_only_report(url: &str, json: bool, out: &mut Out) -> Result<i32> {
    let mut session = Session::new(HttpTransport::new(url)?);
    match session.auth_settings() {
        Ok(a) if json => {
            let j = serde_json::json!({
                "auth_settings": info::auth_json(&a),
                "steps": session.steps().iter().map(|s| serde_json::json!({"name": s.name, "status": s.status})).collect::<Vec<_>>(),
            });
            writeln!(out, "{}", serde_json::to_string_pretty(&j)?)?;
            Ok(0)
        }
        Ok(a) => {
            for line in info::auth_only_lines(&a) {
                writeln!(out, "{line}")?;
            }
            writeln!(out, "RESULT OK {}", steps_line(&session))?;
            Ok(0)
        }
        Err(e) => {
            writeln!(out, "RESULT FAIL {}", steps_line(&session))?;
            Err(printable(&e.to_string()).into())
        }
    }
}

fn users_hint(user: &Option<String>, e: &SessionError, out: &mut Out) -> Result<()> {
    if user.is_none()
        && matches!(
            e,
            SessionError::Step {
                step: "login",
                status: 402,
                ..
            }
        )
    {
        writeln!(
            out,
            "hint: the infobase has users; log in with --user (and --password-env)"
        )?;
    }
    Ok(())
}

fn open_session(
    login: &Login,
    input: &mut dyn BufRead,
) -> Result<(Session, std::result::Result<(), SessionError>)> {
    let password = password_of(login, |v| std::env::var(v), input)?;
    let tr = HttpTransport::new(&login.url)?;
    let mut session = Session::new(tr);
    if let Some(build) = &login.server_version {
        session.set_version(build);
    }
    let query = login_query(std::process::id(), &new_cln_id(), &client_nm());
    let outcome = match (&login.user, &password) {
        (Some(u), Some(p)) => session.cred_login(
            &query,
            &Credentials {
                user: u,
                password: p,
                strong: login.strong,
            },
        ),
        _ => session.login_plain(&query),
    };
    Ok((session, outcome))
}

fn parse_param(arg: &str) -> std::result::Result<Value, String> {
    let typed =
        |r: std::result::Result<Value, onec_client::ValueError>| r.map_err(|e| e.to_string());
    match arg.split_once(':') {
        Some(("n", t)) => typed(Decimal::parse(t).map(Value::Number)),
        Some(("b", "true")) => Ok(Value::Bool(true)),
        Some(("b", "false")) => Ok(Value::Bool(false)),
        Some(("b", t)) => Err(format!("not a boolean: {t:?} (true or false)")),
        Some(("d", t)) => typed(Date::parse(t).map(Value::Date)),
        Some(("u", t)) => typed(Uuid::parse(t).map(Value::Uuid)),
        Some(("s", t)) => Ok(Value::String(t.to_string())),
        _ if arg == "undef" => Ok(Value::Undefined),
        _ if arg == "null" => Ok(Value::Null),
        _ => Ok(Value::String(arg.to_string())),
    }
}

const VALUE_STACK: usize = 32 << 20;

fn on_value_stack<T: Send>(f: impl FnOnce() -> T + Send) -> std::io::Result<T> {
    on_stack(VALUE_STACK, f)
}

fn on_stack<T: Send>(size: usize, f: impl FnOnce() -> T + Send) -> std::io::Result<T> {
    std::thread::scope(|s| {
        let handle = std::thread::Builder::new()
            .name("onec-values".to_string())
            .stack_size(size)
            .spawn_scoped(s, f)?;
        match handle.join() {
            Ok(v) => Ok(v),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

#[derive(Clone, Copy)]
struct CallOptions {
    cold: bool,
    json: bool,
    presentations: bool,
}

struct Shown {
    refs: Presentations,
    enums: EnumPresentations,
}

impl Shown {
    fn ask(session: &mut Session, result: &CallResult) -> std::result::Result<Shown, SessionError> {
        let values = || result.ret.iter().chain(&result.params);
        let refs = session.presentations(&onec_client::refs::collect(values()))?;
        let enums = session.enum_presentations(&onec_client::enums::collect(values()))?;
        Ok(Shown { refs, enums })
    }
}

struct CallReport {
    lines: Vec<String>,
    warning: Option<String>,
    outcome: Result<()>,
}

fn call_report(
    mut session: Session,
    login: std::result::Result<(), SessionError>,
    call: &str,
    params: &[Value],
    opts: CallOptions,
) -> CallReport {
    let outcome = login.and_then(|()| session.run_call(call, params, opts.cold));
    let presentations = match &outcome {
        Ok(result) if opts.presentations => {
            Some(Shown::ask(&mut session, result).map_err(|e| e.to_string()))
        }
        _ => None,
    };
    let _ = session.logout();
    let fail = |session: &Session, e: crate::Error| CallReport {
        lines: vec![format!("RESULT FAIL {}", steps_line(session))],
        warning: None,
        outcome: Err(e),
    };
    let result = match outcome {
        Ok(result) => result,
        Err(e) => return fail(&session, e.into()),
    };
    if opts.json {
        let j = call_json(&session, &result, presentations.as_ref());
        if let Some(e) = too_deep_for_param_json(&j) {
            return fail(&session, e.into());
        }
        return match serde_json::to_string_pretty(&j) {
            Ok(text) => CallReport {
                lines: vec![text],
                warning: None,
                outcome: Ok(()),
            },
            Err(e) => CallReport {
                lines: Vec::new(),
                warning: None,
                outcome: Err(e.into()),
            },
        };
    }
    let (pres, warning) = match presentations {
        None => (None, None),
        Some(Ok(p)) => (Some(p), None),
        Some(Err(e)) => (None, Some(format!("warning: presentations not shown: {e}"))),
    };
    let mut lines = Vec::new();
    if let Some(info) = session.login_info() {
        lines.push(format!(
            "login ok: ib={} seance={} config={}",
            info.info_base_alias.as_deref().unwrap_or("?"),
            info.seance.as_deref().unwrap_or("?"),
            info.config.as_deref().unwrap_or("?"),
        ));
    }
    if !result.members.is_empty() {
        lines.push(format!("module functions: {}", result.members.join(", ")));
    }
    lines.extend(result_lines(&result.ret, &result.params, pres.as_ref()));
    lines.push(format!("RESULT OK {}", steps_line(&session)));
    CallReport {
        lines,
        warning,
        outcome: Ok(()),
    }
}

fn parse_param_json(arg: &str) -> std::result::Result<Value, String> {
    on_value_stack(|| read_param_json(arg))
        .map_err(|e| format!("cannot start the thread that reads values: {e}"))?
}

const JSON_MAX_NESTING: usize = 127;

fn read_param_json(arg: &str) -> std::result::Result<Value, String> {
    serde_json::from_str::<JsonValue>(arg)
        .map(|JsonValue(v)| v)
        .map_err(|e| with_nfc_hint(arg, e.to_string()))
}

fn with_nfc_hint(arg: &str, e: String) -> String {
    if arg.chars().any(|c| ('\u{300}'..='\u{36f}').contains(&c)) {
        format!("{e} (the text has combining marks; write names in NFC, e.g. «й» as one character)")
    } else {
        e
    }
}

struct JsonValue(Value);

impl<'de> serde::Deserialize<'de> for JsonValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        d.deserialize_any(JsonVisitor).map(JsonValue)
    }
}

struct JsonVisitor;

const TAGS: &[&str] = &[
    "n", "d", "u", "null", "ref", "enum", "fix", "st", "fixst", "map", "fixmap", "vt", "tree", "vl",
];

impl<'de> serde::de::Visitor<'de> for JsonVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(
            "a typed call value (a string, true/false, null, an array or a one-key tagged object)",
        )
    }

    fn visit_bool<E>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_str<E>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(v.to_string()))
    }

    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Undefined)
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Value, E> {
        Err(bare_number(v))
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Value, E> {
        Err(bare_number(v))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Value, E> {
        Err(bare_number(v))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        seq: A,
    ) -> std::result::Result<Value, A::Error> {
        Ok(Value::Array(Array::new(items(seq)?)))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Value, A::Error> {
        use serde::de::Error;
        let Some(tag) = map.next_key::<String>()? else {
            return Err(A::Error::custom(
                "an empty object is not a value; use {\"st\":{}} for a structure",
            ));
        };
        let text = |t: &str, r: std::result::Result<Value, onec_client::ValueError>| {
            r.map_err(|e| A::Error::custom(format!("{t}: {e}")))
        };
        let v = match tag.as_str() {
            "n" => {
                let t: String = map.next_value()?;
                text(&tag, Decimal::parse(&t).map(Value::Number))?
            }
            "d" => {
                let t: String = map.next_value()?;
                text(&tag, Date::parse(&t).map(Value::Date))?
            }
            "u" => {
                let t: String = map.next_value()?;
                text(&tag, Uuid::parse(&t).map(Value::Uuid))?
            }
            "null" => {
                map.next_value::<()>()?;
                Value::Null
            }
            "ref" => values::reference(map.next_value()?)
                .map_err(|e| A::Error::custom(format!("ref: {e}")))?,
            "enum" => values::enum_value(map.next_value()?)
                .map_err(|e| A::Error::custom(format!("enum: {e}")))?,
            "vt" => values::table(map.next_value()?)
                .map_err(|e| A::Error::custom(format!("vt: {e}")))?,
            "tree" => values::tree(map.next_value()?)
                .map_err(|e| A::Error::custom(format!("tree: {e}")))?,
            "vl" => {
                values::list(map.next_value()?).map_err(|e| A::Error::custom(format!("vl: {e}")))?
            }
            "fix" => Value::Array(Array::fixed(map.next_value::<JsonItems>()?.0)),
            "st" | "fixst" => {
                let mut st = map.next_value::<JsonStructure>()?.0;
                st.set_fixed(tag == "fixst");
                Value::Structure(st)
            }
            "map" | "fixmap" => {
                let pairs = map.next_value::<Vec<(JsonValue, JsonValue)>>()?;
                let mut m = if tag == "fixmap" {
                    Map::new_fixed()
                } else {
                    Map::new()
                };
                for (k, v) in pairs {
                    let shown = show(&k.0, true, None);
                    if m.insert(k.0, v.0)
                        .map_err(|e| A::Error::custom(e.to_string()))?
                        .is_some()
                    {
                        return Err(A::Error::custom(format!("repeated map key {shown}")));
                    }
                }
                Value::Map(m)
            }
            _ => return Err(A::Error::unknown_field(&tag, TAGS)),
        };
        if map.next_key::<serde::de::IgnoredAny>()?.is_some() {
            return Err(A::Error::custom(format!(
                "a tagged value has one key; {tag:?} has more"
            )));
        }
        Ok(v)
    }
}

fn bare_number<E: serde::de::Error>(v: impl std::fmt::Display) -> E {
    E::custom(format!(
        "a bare JSON number {v} may lose digits; write {{\"n\":\"{v}\"}}"
    ))
}

fn items<'de, A: serde::de::SeqAccess<'de>>(
    mut seq: A,
) -> std::result::Result<Vec<Value>, A::Error> {
    let mut out = Vec::new();
    while let Some(JsonValue(v)) = seq.next_element()? {
        out.push(v);
    }
    Ok(out)
}

struct JsonItems(Vec<Value>);

impl<'de> serde::Deserialize<'de> for JsonItems {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Vec<Value>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an array of values")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> std::result::Result<Vec<Value>, A::Error> {
                items(seq)
            }
        }
        d.deserialize_seq(V).map(JsonItems)
    }
}

struct JsonStructure(Structure);

impl<'de> serde::Deserialize<'de> for JsonStructure {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Structure;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object of structure properties")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Structure, A::Error> {
                use serde::de::Error;
                let mut st = Structure::new();
                while let Some(key) = map.next_key::<String>()? {
                    let JsonValue(v) = map.next_value()?;
                    if st
                        .insert(&key, v)
                        .map_err(|e| A::Error::custom(e.to_string()))?
                        .is_some()
                    {
                        return Err(A::Error::custom(format!("repeated structure key {key:?}")));
                    }
                }
                Ok(st)
            }
        }
        d.deserialize_map(V).map(JsonStructure)
    }
}

fn result_lines(ret: &Option<Value>, params: &[Value], pres: Option<&Shown>) -> Vec<String> {
    ret.iter()
        .map(|v| show(v, false, pres))
        .chain(
            params
                .iter()
                .enumerate()
                .map(|(i, p)| format!("param {}: {}", i + 1, show(p, false, pres))),
        )
        .collect()
}

fn quoted(s: &str) -> String {
    format!("{s:?}")
}

fn show_ref(r: &onec_client::Ref, pres: Option<&Presentations>) -> String {
    let base = format!("{}({})", r.type_name(), r.uuid());
    match pres.and_then(|p| p.get(r)) {
        None => base,
        Some(Presentation::Valid(t)) => format!("{base} {}", quoted(t)),
        Some(Presentation::NotFound(t)) => format!("{base} not found {}", quoted(t)),
        Some(Presentation::Blank) => format!("{base} (no text)"),
        Some(Presentation::Empty) => format!("{base} (empty)"),
        Some(Presentation::UnknownType) => format!("{base} (unknown type)"),
    }
}

fn show_enum(e: &EnumRef, pres: Option<&EnumPresentations>) -> String {
    let base = format!("{}({})", e.type_name(), e.name());
    match pres.and_then(|p| p.get(e)) {
        None => base,
        Some(EnumPresentation::Synonym(t)) => format!("{base} {}", quoted(t)),
        Some(EnumPresentation::Empty) => format!("{base} (empty)"),
        Some(EnumPresentation::UnknownType) => format!("{base} (unknown type)"),
        Some(EnumPresentation::UnknownValue) => format!("{base} (unknown value)"),
    }
}

fn show(v: &Value, inner: bool, pres: Option<&Shown>) -> String {
    let fix = |fixed: bool| if fixed { "fix" } else { "" };
    let list = |parts: Vec<String>| parts.join(", ");
    match v {
        Value::String(s) if inner => quoted(s),
        Value::String(s) => s.clone(),
        Value::Undefined => "undef".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => format!("b:{b}"),
        Value::Number(n) => format!("n:{n}"),
        Value::Date(d) => format!("d:{d}"),
        Value::Uuid(u) => format!("u:{u}"),
        Value::Array(a) => format!(
            "{}[{}]",
            fix(a.fixed),
            list(a.items.iter().map(|x| show(x, true, pres)).collect())
        ),
        Value::Structure(st) => format!(
            "{}{{{}}}",
            fix(st.is_fixed()),
            list(
                st.props()
                    .iter()
                    .map(|(k, x)| format!("{k}={}", show(x, true, pres)))
                    .collect()
            )
        ),
        Value::Map(m) => format!(
            "{}map{{{}}}",
            fix(m.is_fixed()),
            list(
                m.pairs()
                    .iter()
                    .map(|(k, x)| format!("{}=>{}", show(k, true, pres), show(x, true, pres)))
                    .collect()
            )
        ),
        Value::Ref(r) => show_ref(r, pres.map(|p| &p.refs)),
        Value::EnumRef(e) => show_enum(e, pres.map(|p| &p.enums)),
        Value::ValueTable(t) => values::show_table(t, &|x| show(x, true, pres)),
        Value::ValueTree(t) => values::show_tree(t, &|x| show(x, true, pres)),
        Value::ValueList(l) => values::show_list(l, &|x| show(x, true, pres)),
    }
}

fn too_deep_for_param_json(j: &serde_json::Value) -> Option<String> {
    let ret = j.get("ret").map(|v| ("ret".to_string(), v));
    let params = j
        .get("params")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, v)| (format!("param {}", i + 1), v));
    ret.into_iter().chain(params).find_map(|(name, v)| {
        let nesting = values::json_nesting(v);
        (nesting > JSON_MAX_NESTING).then(|| {
            format!(
                "{name} nests {nesting} levels of JSON, and --param-json reads at most {}: print it without --json",
                JSON_MAX_NESTING
            )
        })
    })
}

fn call_json(
    session: &Session,
    result: &CallResult,
    pres: Option<&std::result::Result<Shown, String>>,
) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    if let Some(info) = session.login_info() {
        o.insert(
            "login".into(),
            serde_json::json!({
                "ib": info.info_base_alias,
                "seance": info.seance,
                "config": info.config,
            }),
        );
    }
    o.insert("members".into(), serde_json::json!(result.members));
    if let Some(r) = &result.ret {
        o.insert("ret".into(), values::to_json(r));
    }
    o.insert(
        "params".into(),
        serde_json::Value::Array(result.params.iter().map(values::to_json).collect()),
    );
    match pres {
        None => {}
        Some(Ok(p)) => {
            o.insert(
                "presentations".into(),
                serde_json::Value::Array(
                    p.refs
                        .items()
                        .iter()
                        .map(|(r, p)| presentation_json(r, p))
                        .chain(
                            p.enums
                                .items()
                                .iter()
                                .map(|(e, p)| enum_presentation_json(e, p)),
                        )
                        .collect(),
                ),
            );
        }
        Some(Err(e)) => {
            o.insert("presentations_error".into(), serde_json::json!(e));
        }
    }
    o.insert(
        "steps".into(),
        serde_json::Value::Array(
            session
                .steps()
                .iter()
                .map(|s| serde_json::json!({"name": s.name, "status": s.status}))
                .collect(),
        ),
    );
    serde_json::Value::Object(o)
}

fn presentation_json(r: &onec_client::Ref, p: &Presentation) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    o.insert(
        "ref".into(),
        serde_json::json!([r.type_name().to_string(), r.uuid().to_string()]),
    );
    let (state, text) = match p {
        Presentation::Valid(t) => ("valid", Some(t)),
        Presentation::NotFound(t) => ("not_found", Some(t)),
        Presentation::Blank => ("no_text", None),
        Presentation::Empty => ("empty", None),
        Presentation::UnknownType => ("unknown_type", None),
    };
    o.insert("state".into(), serde_json::json!(state));
    if let Some(t) = text {
        o.insert("text".into(), serde_json::json!(t));
    }
    serde_json::Value::Object(o)
}

fn enum_presentation_json(e: &EnumRef, p: &EnumPresentation) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    o.insert(
        "enum".into(),
        serde_json::json!([e.type_name().to_string(), e.name()]),
    );
    let (state, text) = match p {
        EnumPresentation::Synonym(t) => ("valid", Some(t)),
        EnumPresentation::Empty => ("empty", None),
        EnumPresentation::UnknownType => ("unknown_type", None),
        EnumPresentation::UnknownValue => ("unknown_value", None),
    };
    o.insert("state".into(), serde_json::json!(state));
    if let Some(t) = text {
        o.insert("text".into(), serde_json::json!(t));
    }
    serde_json::Value::Object(o)
}

fn steps_line(session: &Session) -> String {
    let steps = session
        .steps()
        .iter()
        .map(|s| format!("{}:{}", s.name, s.status))
        .collect::<Vec<_>>()
        .join(",");
    format!("steps={steps}")
}
