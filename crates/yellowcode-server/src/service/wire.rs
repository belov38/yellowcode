use super::{ResponseData, RouteError, ServiceError};
use crate::process::ProcessError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireResponse {
    pub status: u16,
    pub reason: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not implemented: {0}")]
pub struct Unimplemented(pub &'static str);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Login {
    #[default]
    Anonymous,
    Credentials,
    Started,
    Reused,
    Unrecorded,
}
impl Login {
    pub fn of(headers: &[(String, String)]) -> Self {
        let find = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.trim())
        };
        let (auth, cookie) = (find("Authorization").is_some(), find("Cookie").is_some());
        match find("IBSession") {
            None if cookie => Self::Reused,
            None if auth => Self::Credentials,
            None => Self::Anonymous,
            Some(v) if auth && v.eq_ignore_ascii_case("start") => Self::Started,
            Some(v) if auth && cookie && v.eq_ignore_ascii_case("finish") => Self::Reused,
            Some(_) => Self::Unrecorded,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Call<'a> {
    pub head: bool,
    pub accept_language: Option<&'a str>,
    pub login: Login,
    pub reuse: Option<&'a str>,
}
impl<'a> Call<'a> {
    pub fn of(method: &str, headers: &'a [(String, String)]) -> Self {
        Self {
            head: method == "HEAD",
            accept_language: headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("Accept-Language"))
                .map(|(_, value)| value.as_str()),
            login: Login::of(headers),
            reuse: None,
        }
    }
    pub fn with_reuse(self, reuse: &'a str) -> Self {
        Self {
            reuse: Some(reuse),
            ..self
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Handler,
    Route,
    Platform,
}

fn predefined_cache(call: Call<'_>, answer: Answer, status: u16) -> Result<bool, Unimplemented> {
    match (call.login, answer) {
        (Login::Started, _) => match status {
            200 | 500 => Ok(true),
            _ => Err(Unimplemented(
                "vrs-predefinedCache with IBSession: start at a status other than 200 and 500 (server.session.autouse#3)",
            )),
        },
        (Login::Credentials, _) => match call.reuse {
            Some("DontUse") => Ok(true),
            Some(_) => Err(Unimplemented(
                "vrs-predefinedCache with credentials at a service that reuses sessions (server.session.autouse#1)",
            )),
            None => Err(Unimplemented(
                "vrs-predefinedCache with credentials without the ReuseSessions of the service",
            )),
        },
        (Login::Anonymous, Answer::Handler | Answer::Route) | (Login::Reused, Answer::Handler) => {
            Ok(false)
        }
        (Login::Anonymous, Answer::Platform) => Err(Unimplemented(
            "a 403 or a 500 text of the platform in an anonymous session",
        )),
        (Login::Reused, _) => Err(Unimplemented(
            "a 404, 405, 403 or 500 of the platform in a reused session",
        )),
        (Login::Unrecorded, _) => Err(Unimplemented(
            "vrs-predefinedCache for an unrecorded combination of Authorization, Cookie and IBSession",
        )),
    }
}

const TEXT: &str = "text/plain; charset=utf-8";
const BOM: &str = "\u{FEFF}";
const AMBIGUOUS: &str = "Не удалось однозначно определить шаблон для запрашиваемого URL";
const INVALID_TEMPLATE: &str = "Недопустимое значение аргумента функции\n[httpsrv - src\\httpsrv\\src\\HttpServiceInvokingServiceImpl.cpp (53)]";
const FORBIDDEN: &str = "Недостаточно прав для использования ресурса с данным HTTP методом";
const NO_SESSION: &str = "Текущему соединению с информационной базой не назначен сеанс";
const REALM: &str = "Basic realm=\"1C:Enterprise 8.5\"";

pub fn reason(status: u16) -> Result<&'static str, Unimplemented> {
    Ok(match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        415 => "Unsupported Media Type",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        101 | 299 | 600 | 999 => "Unknown",
        _ => return Err(Unimplemented("the reason phrase of this status")),
    })
}

pub fn handler(call: Call<'_>, response: &ResponseData) -> Result<WireResponse, Unimplemented> {
    let status = response.status;
    if matches!(status, 0 | 99 | 100 | 199) {
        return text(
            call,
            500,
            Answer::Handler,
            &format!("Недопустимый код состояния HTTP: {status}"),
        );
    }
    if status < 101 {
        return Err(Unimplemented("a handler status 1-98"));
    }
    if (102..=198).contains(&status) {
        return Err(Unimplemented("a handler status 102-198"));
    }
    if status == 205 {
        return Err(Unimplemented("a handler status 205"));
    }
    if status > 999 {
        return Err(Unimplemented("a handler status above 999"));
    }
    let mut connection = ("Connection".to_owned(), "Keep-Alive".to_owned());
    let mut own = Vec::with_capacity(response.headers.len());
    for (name, value) in &response.headers {
        if name.is_empty() || !name.bytes().all(token) {
            return Err(Unimplemented(
                "a handler header name that is not an HTTP token",
            ));
        }
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(Unimplemented(
                "a control character in a handler header value",
            ));
        }
        if value.chars().any(|c| u32::from(c) > 0xFFFF) {
            return Err(Unimplemented(
                "a character outside the BMP in a handler header value",
            ));
        }
        let value: String = value
            .chars()
            .map(|c| if c.is_ascii() { c } else { '?' })
            .collect();
        if name.eq_ignore_ascii_case("Connection") {
            connection = (name.clone(), value);
        } else if !name.eq_ignore_ascii_case("Content-Length")
            && !name.eq_ignore_ascii_case("Transfer-Encoding")
        {
            own.push((name.clone(), value));
        }
    }
    let mut headers = vec![
        connection,
        ("Content-Length".to_owned(), response.body.len().to_string()),
    ];
    if predefined_cache(call, Answer::Handler, status)? {
        headers.push(("vrs-predefinedCache".to_owned(), "1".to_owned()));
    }
    headers.extend(own);
    let bodyless = call.head || status < 200 || matches!(status, 204 | 304);
    Ok(WireResponse {
        status,
        reason: reason(status)?,
        headers,
        body: (!bodyless).then(|| response.body.clone()),
    })
}

pub fn failure(call: Call<'_>, error: &ServiceError) -> Result<WireResponse, Unimplemented> {
    match error {
        ServiceError::Route(RouteError::NotFound) => empty(call, 404, Some(Answer::Route)),
        ServiceError::Route(RouteError::Method) => empty(call, 405, Some(Answer::Route)),
        ServiceError::Route(RouteError::Ambiguous) => text(call, 500, Answer::Platform, AMBIGUOUS),
        ServiceError::Route(RouteError::InvalidTemplate) => {
            text(call, 500, Answer::Platform, INVALID_TEMPLATE)
        }
        ServiceError::Route(RouteError::UnknownRoot) => Err(Unimplemented(
            "the XML exception of an unknown service root (503)",
        )),
        ServiceError::Process(ProcessError::Unauthorized) => {
            let mut response = empty(call, 401, None)?;
            response
                .headers
                .insert(1, ("WWW-Authenticate".to_owned(), REALM.to_owned()));
            Ok(response)
        }
        ServiceError::Process(ProcessError::Forbidden) => {
            text(call, 403, Answer::Platform, FORBIDDEN)
        }
        ServiceError::Process(ProcessError::Call) => Err(Unimplemented(
            "the error text of a failed service call (500)",
        )),
        ServiceError::Process(ProcessError::InstructionBudget) => Err(Unimplemented(
            "the answer to an exhausted instruction budget",
        )),
        ServiceError::Process(ProcessError::Open) => Err(Unimplemented(
            "the answer to a failed runtime initialization",
        )),
        ServiceError::Process(ProcessError::Context) => {
            Err(Unimplemented("the answer to an invalid request context"))
        }
        ServiceError::Process(ProcessError::Gone) => {
            Err(Unimplemented("the answer to an unavailable session"))
        }
        ServiceError::Process(ProcessError::Busy) => {
            Err(Unimplemented("the answer to a busy session"))
        }
        ServiceError::Process(ProcessError::Stopped) => {
            Err(Unimplemented("the answer of a stopped process"))
        }
        ServiceError::Process(ProcessError::Internal) => {
            Err(Unimplemented("the answer to an internal replica error"))
        }
    }
}

pub fn no_session(call: Call<'_>) -> Result<WireResponse, Unimplemented> {
    let mut response = platform_text(call, 500, None, NO_SESSION)?;
    response.headers.push(("vrs-rc".to_owned(), "1".to_owned()));
    Ok(response)
}

pub fn unanswered_method(
    call: Call<'_>,
    method: &str,
) -> Option<Result<WireResponse, Unimplemented>> {
    if matches!(method, "GET" | "POST" | "PUT" | "DELETE" | "HEAD") {
        return None;
    }
    Some(text(
        call,
        500,
        Answer::Handler,
        &format!("Метод HTTP не поддерживается: {method}"),
    ))
}

fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}
fn russian(call: Call<'_>) -> Result<(), Unimplemented> {
    let Some(value) = call
        .accept_language
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(());
    };
    let first = value.split([',', ';']).next().unwrap_or_default().trim();
    let primary = first.split('-').next().unwrap_or_default();
    if primary.eq_ignore_ascii_case("ru") {
        Ok(())
    } else {
        Err(Unimplemented(
            "platform error texts in a language other than Russian",
        ))
    }
}
fn empty(
    call: Call<'_>,
    status: u16,
    answer: Option<Answer>,
) -> Result<WireResponse, Unimplemented> {
    envelope(call, status, answer, None, Vec::new())
}
fn text(
    call: Call<'_>,
    status: u16,
    answer: Answer,
    message: &str,
) -> Result<WireResponse, Unimplemented> {
    platform_text(call, status, Some(answer), message)
}
fn platform_text(
    call: Call<'_>,
    status: u16,
    answer: Option<Answer>,
    message: &str,
) -> Result<WireResponse, Unimplemented> {
    russian(call)?;
    let body = format!("{BOM}{message}").into_bytes();
    envelope(call, status, answer, Some(TEXT), body)
}
fn envelope(
    call: Call<'_>,
    status: u16,
    answer: Option<Answer>,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> Result<WireResponse, Unimplemented> {
    let mut headers = vec![
        ("Connection".to_owned(), "Keep-Alive".to_owned()),
        ("Content-Length".to_owned(), body.len().to_string()),
    ];
    if let Some(answer) = answer
        && predefined_cache(call, answer, status)?
    {
        headers.push(("vrs-predefinedCache".to_owned(), "1".to_owned()));
    }
    if let Some(content_type) = content_type {
        headers.push(("Content-Type".to_owned(), content_type.to_owned()));
    }
    Ok(WireResponse {
        status,
        reason: reason(status)?,
        headers,
        body: (!call.head).then_some(body),
    })
}
