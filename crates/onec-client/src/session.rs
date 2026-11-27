use base64::Engine;

use crate::answer::{NS_APP, read};
use crate::auth::{AuthSettings, AuthSettingsError};
use crate::body::{self, ACCEPT_ENCODING};
use crate::cred;
use crate::dcs;
use crate::enums::{self, EnumPresentations, EnumType};
use crate::formdescr::{FormData, FormKind};
use crate::info::{Answers, InfoError, ServerInfo};
use crate::metadata::{MdAnswers, MdAttributes, MdKind, MdType, Metadata, read_types};
use crate::modules::{self, HttpCache, ModuleDefs, ModuleId, ModuleSource, ModulesError};
use crate::refs::{self, Presentations};
use crate::report::{self, FormAttribute, OpenedForm, ReportError};
use crate::spreadsheet::{self, CellCount, Sheet, SheetChanges, SheetLimits};
use crate::transport::{Response, TransportError};
use crate::value::{EnumRef, Ref, Uuid, Value};
use crate::wire::Wire;
use crate::xml::{self, LoginInfo, XmlError};

pub const VERSION: &str = "8.5.1.1529";

const BUILD_MISMATCH: u16 = 409;

const FOVER: &str = "d98c1dd4-008f-04b2-e980-0998ecf8427e";
const ZERO_VER: &str = "00000000-0000-0000-0000-000000000000";
const CLANG: &str = "ru";

const ANY_STATUS: [u16; 500] = any_status();

const fn any_status() -> [u16; 500] {
    let mut a = [0u16; 500];
    let mut i = 0;
    while i < 500 {
        a[i] = 100 + i as u16;
        i += 1;
    }
    a
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Native(#[from] crate::native::NativeError),
    #[error(transparent)]
    Body(#[from] body::BodyError),
    #[error(transparent)]
    Xml(#[from] XmlError),
    #[error(transparent)]
    Cred(#[from] cred::CredError),
    #[error(transparent)]
    Info(#[from] InfoError),
    #[error(transparent)]
    Modules(#[from] ModulesError),
    #[error(transparent)]
    AuthSettings(#[from] AuthSettingsError),
    #[error(transparent)]
    Report(#[from] ReportError),
    #[error(transparent)]
    Dcs(#[from] crate::dcs::DcsError),
    #[error("random source failed: {0}")]
    Random(String),
    #[error("{step}: HTTP {status} {message}")]
    Step {
        step: &'static str,
        status: u16,
        message: String,
    },
    #[error(transparent)]
    List(#[from] crate::dlist::ListError),
    #[error("login refused: wrong user name or password")]
    PasswordRejected,
    #[error(transparent)]
    Object(#[from] crate::object::ObjectError),
    #[error(transparent)]
    Form(#[from] crate::formcall::FormError),
    #[error("{0}")]
    Protocol(String),
}

pub trait RandomSource {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), SessionError>;
}

pub struct OsRandom;

impl RandomSource for OsRandom {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), SessionError> {
        getrandom::fill(buf).map_err(|e| SessionError::Random(e.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct Step {
    pub name: &'static str,
    pub status: u16,
    pub ok: bool,
}

pub struct Credentials<'a> {
    pub user: &'a str,
    pub password: &'a str,
    pub strong: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CallResult {
    pub members: Vec<String>,
    pub ret: Option<Value>,
    pub params: Vec<Value>,
}

#[derive(Debug, Clone, Copy)]
pub struct InfoOptions {
    pub members: bool,
    pub sections: bool,
    pub auth_settings: bool,
}

#[derive(Debug)]
pub struct InfoView {
    pub info: ServerInfo,
    pub auth_settings_error: Option<SessionError>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MetadataOptions {
    pub enum_synonyms: bool,
    pub attributes: bool,
}

#[derive(Debug)]
pub struct MetadataView {
    pub metadata: Metadata,
    pub enum_synonyms_error: Option<SessionError>,
}

#[derive(Debug, Clone, Copy)]
pub struct ReportOptions<'a> {
    pub form: &'a str,
    pub open_params: &'a [(String, Value)],
    pub init: Option<(&'a str, &'a [Value])>,
    pub attributes: &'a [crate::write::Assign],
    pub before: &'a [String],
    pub call: Option<(&'a str, &'a [Value])>,
    pub poll: Option<Poll<'a>>,
    pub limits: SheetLimits,
    pub dcs: Option<DcsOptions<'a>>,
}

impl<'a> ReportOptions<'a> {
    pub fn form(form: &'a str, limits: SheetLimits) -> Self {
        Self {
            form,
            open_params: &[],
            init: None,
            attributes: &[],
            before: &[],
            call: None,
            poll: None,
            limits,
            dcs: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Poll<'a> {
    pub procedure: &'a str,
    pub running: Option<&'a str>,
    pub timeout: std::time::Duration,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DcsOptions<'a> {
    pub variant: Option<&'a str>,
    pub changes: &'a [crate::dcs::Change],
    pub generate: bool,
    pub details: bool,
}

#[derive(Debug, Clone)]
pub struct DcsView {
    pub form: crate::dcs::DcsForm,
    pub details: Vec<crate::dcs::DetailsItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSheet {
    pub position: usize,
    pub sheet: Sheet,
    pub rows_asked: bool,
}

#[derive(Debug, Clone)]
pub struct ReportView {
    pub form: OpenedForm,
    pub attributes: Vec<FormAttribute>,
    pub sheets: Vec<ReportSheet>,
    pub dcs: Option<DcsView>,
    pub init: Option<FormCall>,
    pub calls: Vec<FormCall>,
    pub messages: Vec<crate::write::UserMessage>,
    pub warnings: Vec<String>,
    pub states: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormCall {
    pub procedure: String,
    pub returned: Option<Value>,
}

#[derive(Default)]
struct ReportParts {
    attributes: Vec<FormAttribute>,
    sheets: Vec<ReportSheet>,
    dcs: Option<DcsView>,
    calls: Vec<FormCall>,
    messages: Vec<crate::write::UserMessage>,
    warnings: Vec<String>,
    states: Vec<String>,
}

const POLL_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
const POLL_FACTOR: f64 = 1.4;
const POLL_MAX: std::time::Duration = std::time::Duration::from_secs(15);

const ROWS_PER_REQUEST: usize = 1000;
const DETAILS_PER_REQUEST: u32 = 1000;

pub struct Session<R: RandomSource = OsRandom> {
    tr: Box<dyn Wire>,
    rng: R,
    vrs: Option<String>,
    sync: u32,
    info: Option<LoginInfo>,
    login_body: Vec<u8>,
    types: Option<Vec<u8>>,
    enum_texts: std::collections::HashMap<Uuid, Vec<String>>,
    steps: Vec<Step>,
    pub(crate) version: String,
    auto_version: bool,
    sleep: fn(std::time::Duration),
    relogin: Option<Relogin>,
    relogins: u32,
    logging_in: bool,
}

#[derive(Clone)]
enum Relogin {
    Plain {
        query: String,
    },
    Cred {
        query: String,
        user: String,
        password: String,
        strong: bool,
    },
}

const MAX_RELOGINS: u32 = 3;

pub const EXC_SESSION_ABSENT: &str = "60c686dc-798f-4d17-aadb-a90156a16eb8";

fn session_absent(resp: &Response) -> bool {
    let attr = format!("clsid=\"{EXC_SESSION_ABSENT}\"");
    matches!(resp.status, 404 | 500)
        && body::decode(&resp.body, resp.header("content-encoding")).is_ok_and(|b| {
            b.windows(attr.len())
                .any(|w| w.eq_ignore_ascii_case(attr.as_bytes()))
        })
}

fn with_vrs(headers: &[(String, String)], vrs: Option<&str>) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("vrs-session"))
        .cloned()
        .collect();
    if let Some(v) = vrs {
        h.push(("vrs-session".into(), v.to_string()));
    }
    h
}

pub(crate) struct Prologue {
    pub(crate) conf: String,
    types: Option<Vec<u8>>,
    start_info: Vec<u8>,
    defs: Vec<u8>,
    defs_cache: HttpCache,
}

impl Session<OsRandom> {
    pub fn new(tr: impl Wire + 'static) -> Self {
        Self::with_rng(tr, OsRandom)
    }
}

impl<R: RandomSource> Session<R> {
    pub fn with_rng(tr: impl Wire + 'static, rng: R) -> Self {
        Self {
            tr: Box::new(tr),
            rng,
            vrs: None,
            sync: 0,
            info: None,
            login_body: Vec::new(),
            types: None,
            enum_texts: Default::default(),
            steps: Vec::new(),
            version: VERSION.to_string(),
            auto_version: true,
            sleep: std::thread::sleep,
            relogin: None,
            relogins: 0,
            logging_in: false,
        }
    }

    pub fn relogins(&self) -> u32 {
        self.relogins
    }

    pub fn set_sleep(&mut self, sleep: fn(std::time::Duration)) {
        self.sleep = sleep;
    }

    pub fn set_version(&mut self, build: &str) {
        self.version = build.to_string();
        self.auto_version = false;
        self.tr.set_version(build);
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn login_info(&self) -> Option<&LoginInfo> {
        self.info.as_ref()
    }

    fn rand<const N: usize>(&mut self) -> Result<[u8; N], SessionError> {
        let mut b = [0u8; N];
        self.rng.fill(&mut b)?;
        Ok(b)
    }

    pub(crate) fn headers(
        &mut self,
        accept: &str,
        state: bool,
        xml_body: bool,
    ) -> Vec<(String, String)> {
        let mut h = vec![
            ("User-Agent".into(), "1CV8C".into()),
            ("1C-ApplicationName".into(), "1CV8C".into()),
            ("Accept-Charset".into(), "utf-8".into()),
            ("Accept-Encoding".into(), ACCEPT_ENCODING.into()),
            ("Accept-Language".into(), "ru".into()),
            ("Accept".into(), accept.into()),
        ];
        if let Some(v) = &self.vrs {
            h.push(("vrs-session".into(), v.clone()));
        }
        if state {
            self.sync += 1;
            h.push(("Pragma".into(), format!("sync={}", self.sync)));
        }
        if xml_body {
            h.push((
                "Content-Type".into(),
                "application/xml; charset=utf-8".into(),
            ));
            h.push(("Content-Encoding".into(), "zstd".into()));
        }
        h
    }

    pub(crate) fn step(
        &mut self,
        name: &'static str,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
        accept: &[u16],
    ) -> Result<Response, SessionError> {
        let resp = self.step_raw(name, method, path, headers, body, accept)?;
        Self::accepted(name, resp, accept)
    }

    fn step_raw(
        &mut self,
        name: &'static str,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
        accept: &[u16],
    ) -> Result<Response, SessionError> {
        let resp = self.exchange_step(name, method, path, headers, body, accept)?;
        let changes_state = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("pragma"));
        if name == "logout" || changes_state || !session_absent(&resp) || !self.can_relogin() {
            return Ok(resp);
        }
        self.relogin()?;
        let headers = with_vrs(headers, self.vrs.as_deref());
        self.exchange_step(name, method, path, &headers, body, accept)
    }

    fn can_relogin(&self) -> bool {
        self.relogin.is_some() && !self.logging_in && self.relogins < MAX_RELOGINS
    }

    fn relogin(&mut self) -> Result<(), SessionError> {
        let Some(how) = self.relogin.clone() else {
            return Err(SessionError::Protocol("no login to repeat".into()));
        };
        self.relogins += 1;
        let types = self.types.take();
        let enum_texts = std::mem::take(&mut self.enum_texts);
        let key = self.info.as_ref().and_then(|i| i.types.clone());
        let (vrs, info, login_body) = (
            self.vrs.take(),
            self.info.clone(),
            std::mem::take(&mut self.login_body),
        );
        let done = match &how {
            Relogin::Plain { query } => self.login_plain(query),
            Relogin::Cred {
                query,
                user,
                password,
                strong,
            } => self.cred_login(
                query,
                &Credentials {
                    user,
                    password,
                    strong: *strong,
                },
            ),
        };
        match done {
            Ok(()) => {
                if self.info.as_ref().and_then(|i| i.types.clone()) == key {
                    self.types = types;
                    self.enum_texts = enum_texts;
                }
                self.sync = 0;
                Ok(())
            }
            Err(e) => {
                (self.vrs, self.info, self.login_body) = (vrs, info, login_body);
                self.types = types;
                self.enum_texts = enum_texts;
                Err(e)
            }
        }
    }

    fn exchange_step(
        &mut self,
        name: &'static str,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
        accept: &[u16],
    ) -> Result<Response, SessionError> {
        let resp = match self.tr.exchange(method, path, headers, body) {
            Ok(r) => r,
            Err(e) => {
                self.steps.push(Step {
                    name,
                    status: 0,
                    ok: false,
                });
                return Err(e);
            }
        };
        let ok = accept.contains(&resp.status);
        self.steps.push(Step {
            name,
            status: resp.status,
            ok,
        });
        Ok(resp)
    }

    pub(crate) fn step_once(
        &mut self,
        name: &'static str,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
        accept: &[u16],
    ) -> Result<Response, SessionError> {
        let resp = match self.tr.exchange_once(method, path, headers, body) {
            Ok(r) => r,
            Err(e) => {
                self.steps.push(Step {
                    name,
                    status: 0,
                    ok: false,
                });
                return Err(e);
            }
        };
        self.steps.push(Step {
            name,
            status: resp.status,
            ok: accept.contains(&resp.status),
        });
        Ok(resp)
    }

    fn accepted(
        name: &'static str,
        resp: Response,
        accept: &[u16],
    ) -> Result<Response, SessionError> {
        if !accept.contains(&resp.status) {
            let decoded =
                body::decode(&resp.body, resp.header("content-encoding")).unwrap_or_default();
            let message = if decoded.is_empty() {
                String::new()
            } else {
                xml::parse_exception(&decoded)
            };
            return Err(SessionError::Step {
                step: name,
                status: resp.status,
                message,
            });
        }
        Ok(resp)
    }

    pub(crate) fn decoded(resp: &Response) -> Result<Vec<u8>, SessionError> {
        Ok(body::decode(&resp.body, resp.header("content-encoding"))?)
    }

    fn cred_json(&mut self, blob: &str) -> (Vec<(String, String)>, Vec<u8>) {
        let mut h = self.headers("*/*", false, false);
        h.push(("Content-Type".into(), "application/json".into()));
        h.push(("Content-Encoding".into(), "zstd".into()));
        let json = format!("{{\"cred\": \"{blob}\"}}");
        (h, body::zstd_compress(json.as_bytes()))
    }

    fn cred_of(resp_body: &[u8]) -> Vec<u8> {
        let text = String::from_utf8_lossy(resp_body);
        let Some(start) = text
            .find("\"cred\"")
            .and_then(|i| text[i..].find(':').map(|j| i + j + 1))
        else {
            return Vec::new();
        };
        let after = text[start..].trim_start();
        let after = after.strip_prefix('"').unwrap_or("");
        let Some(end) = after.find('"') else {
            return Vec::new();
        };
        base64::engine::general_purpose::STANDARD
            .decode(&after[..end])
            .unwrap_or_default()
    }

    pub fn cred_login(&mut self, query: &str, c: &Credentials) -> Result<(), SessionError> {
        let was = std::mem::replace(&mut self.logging_in, true);
        let done = self.cred_login_once(query, c);
        self.logging_in = was;
        if done.is_ok() {
            self.relogin = Some(Relogin::Cred {
                query: query.to_string(),
                user: c.user.to_string(),
                password: c.password.to_string(),
                strong: c.strong,
            });
        }
        done
    }

    fn cred_login_once(&mut self, query: &str, c: &Credentials) -> Result<(), SessionError> {
        self.forget_types();
        let (resp, query) = self.first_login("login/cred1", query, |s| {
            let noise: [u8; 16] = s.rand()?;
            let pad1: [u8; 16] = s.rand()?;
            let blob = cred::cred_blob(&cred::build_cred_req1(c.user, &noise, &pad1));
            Ok(s.cred_json(&blob))
        })?;
        let vrs = resp
            .header("vrs-session")
            .ok_or_else(|| SessionError::Protocol("round 1 answer has no vrs-session".into()))?
            .to_string();
        let sid = sid_from_vrs(&vrs)?;
        self.vrs = Some(vrs);

        let rc: [u8; 32] = self.rand()?;
        let pad3: [u8; 16] = self.rand()?;
        let key = cred::pwkey(c.password, c.strong);
        let blob = cred::cred_blob(&cred::build_cred_req3(&rc, &key, &pad3));
        let (h, b) = self.cred_json(&blob);
        let resp = self.step(
            "login/cred2",
            "POST",
            &format!("/e1cib/login?{query}&sid={sid}"),
            &h,
            &b,
            &[200],
        )?;
        let ans4 = Self::cred_of(&Self::decoded(&resp)?);
        if ans4.is_empty() {
            return Err(SessionError::PasswordRejected);
        }

        let rs = cred::recover_rs(&ans4, &rc)?;
        let session_key: [u8; 32] = self.rand()?;
        let pad5: [u8; 16] = self.rand()?;
        let blob = cred::cred_blob(&cred::build_cred_req5(&rs, &session_key, c.password, &pad5));
        let (h, b) = self.cred_json(&blob);
        let resp = self.step(
            "login/cred3",
            "POST",
            &format!("/e1cib/login?{query}&sid={sid}"),
            &h,
            &b,
            &[200],
        )?;
        if let Some(v) = resp.header("vrs-session") {
            self.vrs = Some(v.to_string());
        }
        self.set_login(Self::decoded(&resp)?)
    }

    pub fn login_plain(&mut self, query: &str) -> Result<(), SessionError> {
        let was = std::mem::replace(&mut self.logging_in, true);
        let done = self.login_plain_once(query);
        self.logging_in = was;
        if done.is_ok() {
            self.relogin = Some(Relogin::Plain {
                query: query.to_string(),
            });
        }
        done
    }

    fn login_plain_once(&mut self, query: &str) -> Result<(), SessionError> {
        self.forget_types();
        let (resp, _) = self.first_login("login", query, |s| {
            let mut h = s.headers("*/*", false, false);
            h.push((
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ));
            Ok((h, Vec::new()))
        })?;
        self.vrs = resp.header("vrs-session").map(|s| s.to_string());
        self.set_login(Self::decoded(&resp)?)
    }

    fn first_login(
        &mut self,
        name: &'static str,
        query: &str,
        mut request: impl FnMut(&mut Self) -> Result<(Vec<(String, String)>, Vec<u8>), SessionError>,
    ) -> Result<(Response, String), SessionError> {
        match self.tr.connect_build() {
            Ok(Some(build)) if self.auto_version => self.version = build,
            Ok(_) => {}
            Err(e) => {
                self.steps.push(Step {
                    name,
                    status: 0,
                    ok: false,
                });
                return Err(e);
            }
        }
        let q = with_version(query, &self.version);
        let (h, b) = request(self)?;
        let resp = self.step_raw(name, "POST", &format!("/e1cib/login?{q}"), &h, &b, &[200])?;
        if resp.status == BUILD_MISMATCH
            && self.auto_version
            && let Some(build) = server_build(&resp).filter(|b| *b != self.version)
        {
            self.version = build;
            self.tr.set_version(&self.version);
            let q = with_version(query, &self.version);
            let (h, b) = request(self)?;
            let resp = self.step(name, "POST", &format!("/e1cib/login?{q}"), &h, &b, &[200])?;
            return Ok((resp, q));
        }
        Ok((Self::accepted(name, resp, &[200])?, q))
    }

    fn forget_types(&mut self) {
        self.types = None;
        self.enum_texts.clear();
    }

    fn set_login(&mut self, body: Vec<u8>) -> Result<(), SessionError> {
        self.info = Some(xml::parse_login(&body)?);
        self.login_body = body;
        Ok(())
    }

    pub(crate) fn prologue(&mut self, with_types: bool) -> Result<Prologue, SessionError> {
        let info = self
            .info
            .clone()
            .ok_or_else(|| SessionError::Protocol("not logged in".into()))?;
        let conf = info
            .config
            .ok_or_else(|| SessionError::Protocol("login response had no config".into()))?;

        let types = if with_types {
            self.fetch_types(info.types.as_deref())?
        } else {
            None
        };

        let start_body = body::zstd_compress(START_INFO_BODY.as_bytes());
        let h = self.headers("application/xml", true, true);
        let resp = self.step(
            "startInfo",
            "POST",
            "/e1cib/misc?cmd=startInfo&clientTime=00000000000000&connSpeed=normal&clnOS=Windows&defaultScale=compact",
            &h,
            &start_body,
            &[200],
        )?;
        let start_info = Self::decoded(&resp)?;

        let sysver = self.version.clone();
        let h = self.headers("*/*", false, false);
        self.step(
            "splash",
            "GET",
            &format!("/e1cib/metadata/splash?sysver={sysver}&confver={conf}&scale=100&interfaceVar=16&operatingSystem=1&theme=1&extract=true"),
            &h,
            &[],
            &[200, 404],
        )?;

        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "modules/defs",
            "GET",
            &format!("/e1cib/modules/defs?sysver={sysver}&confver={conf}&lm=2"),
            &h,
            &[],
            &[200],
        )?;
        let defs = Self::decoded(&resp)?;
        Ok(Prologue {
            conf,
            types,
            start_info,
            defs,
            defs_cache: http_cache(&resp),
        })
    }

    fn fetch_types(&mut self, key: Option<&str>) -> Result<Option<Vec<u8>>, SessionError> {
        let Some(key) = key else {
            return Ok(None);
        };
        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "types",
            "GET",
            &format!("/e1cib/types?sysver={sysver}&types={key}"),
            &h,
            &[],
            &[200],
        )?;
        let body = Self::decoded(&resp)?;
        self.types = Some(body.clone());
        self.enum_texts.clear();
        Ok(Some(body))
    }

    pub fn presentations(&mut self, refs: &[Ref]) -> Result<Presentations, SessionError> {
        let ids = if refs.iter().any(|r| !r.is_empty()) {
            refs::type_ids(&self.session_types("references")?)?
        } else {
            Default::default()
        };
        let mut unrecognized = Vec::new();
        let plan = refs::plan(refs, &ids);
        let mut answers = std::collections::HashMap::new();
        for batch in refs::batches(&plan.groups, refs::BATCH) {
            let req = body::zstd_compress(&refs::build_request(&batch));
            let h = self.headers("application/xml", true, true);
            let resp = self.step("refs", "POST", "/e1cib/misc?cmd=refs", &h, &req, &[200])?;
            answers.extend(refs::parse_answer(
                &Self::decoded(&resp)?,
                &batch,
                &mut unrecognized,
            )?);
        }
        let mut items = Vec::with_capacity(plan.refs.len());
        for (r, state) in plan.refs {
            let p = match state {
                Err(p) => p,
                Ok(clsid) => answers.get(&(clsid, r.uuid())).cloned().ok_or_else(|| {
                    SessionError::Protocol(format!(
                        "refs: no answer for {}({})",
                        r.type_name(),
                        r.uuid()
                    ))
                })?,
            };
            items.push((r, p));
        }
        Ok(Presentations::new(items, unrecognized))
    }

    pub(crate) fn session_types(&mut self, what: &str) -> Result<Vec<MdType>, SessionError> {
        let body = match self.types.clone() {
            Some(b) => b,
            None => {
                if self.info.is_none() {
                    return Err(SessionError::Protocol("not logged in".into()));
                }
                let key = self.info.as_ref().and_then(|i| i.types.clone());
                self.fetch_types(key.as_deref())?.ok_or_else(|| {
                    SessionError::Protocol(format!(
                        "login response had no types: {what} cannot be presented"
                    ))
                })?
            }
        };
        Ok(read(
            "types",
            &body,
            NS_APP,
            &[],
            &mut Vec::new(),
            read_types,
        )?)
    }

    fn enum_texts(&mut self, t: &EnumType) -> Result<Vec<String>, SessionError> {
        if let Some(texts) = self.enum_texts.get(&t.clsid)
            && texts.len() == t.names.len()
        {
            return Ok(texts.clone());
        }
        let key = self.info.as_ref().and_then(|i| i.types.clone());
        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "types/view/values",
            "GET",
            &enums::values_path(t.clsid, &sysver, key.as_deref()),
            &h,
            &[],
            &[200],
        )?;
        let texts = enums::parse_values(&Self::decoded(&resp)?, &t.names)?;
        self.enum_texts.insert(t.clsid, texts.clone());
        Ok(texts)
    }

    pub fn enum_presentations(
        &mut self,
        enums: &[EnumRef],
    ) -> Result<EnumPresentations, SessionError> {
        let types = if enums.iter().any(|e| !e.is_empty()) {
            self.session_types("enumeration values")?
        } else {
            Vec::new()
        };
        let plan = enums::plan(enums, &enums::enum_index(&types))?;
        let mut texts = Vec::with_capacity(plan.types.len());
        for t in &plan.types {
            texts.push(self.enum_texts(t)?);
        }
        let mut items = Vec::with_capacity(plan.values.len());
        for (e, state) in plan.values {
            let p = match state {
                Err(p) => p,
                Ok(at) => {
                    let text = texts
                        .get(at.ty)
                        .and_then(|t| t.get(at.pos))
                        .ok_or_else(|| {
                            SessionError::Protocol(format!(
                                "types/view/values: no text for {}({})",
                                e.type_name(),
                                e.name()
                            ))
                        })?;
                    enums::EnumPresentation::Synonym(text.clone())
                }
            };
            items.push((e, p));
        }
        Ok(EnumPresentations::new(items))
    }

    fn module_defs(&mut self, conf: &str, module_id: &str) -> Result<Vec<u8>, SessionError> {
        let qid = percent_encode(module_id);
        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "modules/defs?id",
            "GET",
            &format!("/e1cib/modules/defs?sysver={sysver}&confver={conf}&lm=2&id={qid}"),
            &h,
            &[],
            &[200],
        )?;
        Self::decoded(&resp)
    }

    fn about(&mut self) -> Result<Vec<u8>, SessionError> {
        let h = self.headers("application/xml", false, false);
        let resp = self.step("about", "GET", "/e1cib/about", &h, &[], &[200])?;
        Self::decoded(&resp)
    }

    pub fn run_call(
        &mut self,
        call: &str,
        params: &[Value],
        cold: bool,
    ) -> Result<CallResult, SessionError> {
        let pro = self.prologue(cold)?;
        let result = self.module_call(&pro, call, params)?;
        self.about()?;
        Ok(result)
    }

    fn module_call(
        &mut self,
        pro: &Prologue,
        call: &str,
        params: &[Value],
    ) -> Result<CallResult, SessionError> {
        let conf = pro.conf.as_str();
        let modules = xml::parse_server_modules(&pro.defs)?;
        let (module, func) = match call.rsplit_once('.') {
            Some((m, f)) => (m.to_string(), f.to_string()),
            None => (String::new(), call.to_string()),
        };
        let module = if module.is_empty() {
            modules
                .first()
                .map(|(code, _)| code.clone())
                .ok_or_else(|| SessionError::Protocol("server listed no modules".into()))?
        } else {
            module
        };
        let module_id = modules
            .iter()
            .find(|(code, _)| code == &module)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| SessionError::Protocol(format!("no server module {module}")))?;
        let members = xml::parse_module_members(&self.module_defs(conf, &module_id)?)?;
        let qid = percent_encode(&module_id);

        let call_body = body::zstd_compress(&xml::build_call_request(&func, params)?);
        let h = self.headers("application/xml", true, true);
        let resp = self.step(
            "modules/call",
            "POST",
            &format!("/e1cib/modules/call?id={qid}"),
            &h,
            &call_body,
            &[200],
        )?;
        let values = xml::parse_call_result(&Self::decoded(&resp)?)?;
        let (ret, params) = (values.ret, values.params);
        Ok(CallResult {
            members,
            ret,
            params,
        })
    }

    pub fn auth_settings(&mut self) -> Result<AuthSettings, SessionError> {
        let mut h = self.headers("*/*", false, false);
        h.push((
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        let resp = self.step(
            "additionalAuthSettings",
            "POST",
            "/e1cib/additionalAuthSettings?version=1",
            &h,
            &[],
            &[200],
        )?;
        let body = body::decode_bounded(
            &resp.body,
            resp.header("content-encoding"),
            crate::auth::MAX_ANSWER as u64,
        )
        .map_err(|e| match e {
            body::BodyError::TooLarge => SessionError::AuthSettings(AuthSettingsError::TooLarge),
            e => e.into(),
        })?;
        Ok(AuthSettings::parse(&body)?)
    }

    pub fn run_info(&mut self, opts: InfoOptions) -> Result<InfoView, SessionError> {
        let pro = self.prologue(true)?;
        let conf = pro.conf.as_str();
        let mut members = Vec::new();
        if opts.members {
            for (code, id) in xml::parse_server_modules(&pro.defs)? {
                let body = self.module_defs(conf, &id)?;
                members.push((code, body));
            }
        }
        let about = self.about()?;
        let (auth_settings, auth_settings_error) = if opts.auth_settings {
            match self.auth_settings() {
                Ok(a) => (Some(a), None),
                Err(e) => (None, Some(e)),
            }
        } else {
            (None, None)
        };
        let mut cmi = None;
        if opts.sections {
            let sysver = self.version.clone();
            let h = self.headers("application/xml", false, false);
            let resp = self.step(
                "cmiStartData",
                "GET",
                &format!("/e1cib/cmi/cmiStartData?sysver={sysver}&confver={conf}"),
                &h,
                &[],
                &[200],
            )?;
            cmi = Some(Self::decoded(&resp)?);
        }
        let mut info = ServerInfo::from_answers(&Answers {
            login: &self.login_body,
            types: pro.types.as_deref(),
            start_info: &pro.start_info,
            defs: &pro.defs,
            members: &members,
            about: &about,
            cmi: cmi.as_deref(),
        })?;
        info.auth_settings = auth_settings;
        Ok(InfoView {
            info,
            auth_settings_error,
        })
    }

    pub fn run_metadata(&mut self) -> Result<Metadata, SessionError> {
        Ok(self.run_metadata_with(MetadataOptions::default())?.metadata)
    }

    pub fn run_metadata_with(
        &mut self,
        opts: MetadataOptions,
    ) -> Result<MetadataView, SessionError> {
        let mut metadata = self.metadata_answers()?;
        if opts.attributes {
            self.attributes_of(&mut metadata)?;
        }
        let mut enum_synonyms_error = None;
        if opts.enum_synonyms {
            match self.enum_synonyms_of(&metadata.types) {
                Ok(all) => {
                    for (i, texts) in all {
                        if let Some(t) = metadata.types.get_mut(i) {
                            for (v, text) in t.values.iter_mut().zip(texts) {
                                v.synonym = Some(text);
                            }
                        }
                    }
                }
                Err(e) => enum_synonyms_error = Some(e),
            }
        }
        Ok(MetadataView {
            metadata,
            enum_synonyms_error,
        })
    }

    fn attributes_of(&mut self, metadata: &mut Metadata) -> Result<(), SessionError> {
        let info = self
            .info
            .clone()
            .ok_or_else(|| SessionError::Protocol("not logged in".into()))?;
        let conf = info
            .config
            .ok_or_else(|| SessionError::Protocol("login response had no config".into()))?;
        let roles = info.roles_id.unwrap_or_else(|| "0".to_string());
        let uuids = metadata.md_uuids();
        let mut broken = false;
        for i in 0..metadata.types.len() {
            let Some(kind) = metadata.types[i].kind.form_kind() else {
                continue;
            };
            let found = uuids
                .iter()
                .find(|(j, _)| *j == i)
                .map_or(&[][..], |(_, u)| u.as_slice());
            let a = self.form_attributes(kind, found, &conf, &roles, &mut broken, metadata);
            metadata.types[i].attributes = Some(a);
        }
        for i in 0..metadata.objects.len() {
            let Some(kind) = metadata.objects[i].kind.object_form() else {
                continue;
            };
            let found = metadata.objects[i].uuids.clone();
            let a = self.form_attributes(kind, &found, &conf, &roles, &mut broken, metadata);
            metadata.objects[i].attributes = Some(a);
        }
        Ok(())
    }

    fn form_attributes(
        &mut self,
        kind: FormKind,
        uuids: &[String],
        conf: &str,
        roles: &str,
        broken: &mut bool,
        metadata: &mut Metadata,
    ) -> MdAttributes {
        let uuid = match uuids {
            [] => return MdAttributes::NoCommand,
            [one] => one,
            many => {
                return MdAttributes::Ambiguous {
                    uuids: many.to_vec(),
                };
            }
        };
        let form_id = kind.form_id(uuid);
        if *broken {
            return MdAttributes::Failed {
                form_id,
                error: "not asked: an earlier exchange failed".into(),
            };
        }
        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let (step, path) = match kind {
            FormKind::List => (
                "logForm/elements",
                format!(
                    "/e1cib/logForm?sysver={sysver}&cmd=elements&clang={CLANG}&formId={form_id}&confver={conf}\
                     &rolesId={roles}&iweb=16"
                ),
            ),
            _ => (
                "logForm/dataDescrWMethods",
                format!(
                    "/e1cib/logForm?sysver={sysver}&cmd=dataDescrWMethods&formId={form_id}&confver={conf}\
                     &rolesId={roles}"
                ),
            ),
        };
        let resp = match self.step(step, "GET", &path, &h, &[], &ANY_STATUS) {
            Ok(r) => r,
            Err(e) => {
                *broken = true;
                return MdAttributes::Failed {
                    form_id,
                    error: e.to_string(),
                };
            }
        };
        let status = resp.status;
        let read = Self::decoded(&resp)
            .map_err(|e| e.to_string())
            .and_then(|body| {
                let read = match kind {
                    FormKind::List => {
                        crate::metadata::list_columns(&body).map(|columns| MdAttributes::Columns {
                            form_id: form_id.clone(),
                            columns,
                        })
                    }
                    _ => {
                        let mut unrecognized = Vec::new();
                        FormData::from_answer(&body, &mut unrecognized).map(|data| {
                            if status == 200 {
                                metadata.unrecognized.extend(unrecognized);
                            }
                            MdAttributes::Read {
                                form_id: form_id.clone(),
                                data,
                            }
                        })
                    }
                };
                match read {
                    Ok(a) if status == 200 => Ok(a),
                    Ok(_) => Err(format!("HTTP {status}")),
                    Err(e) if status == 200 => Err(e.to_string()),
                    Err(e) => Err(format!("HTTP {status}: {e}")),
                }
            });
        read.unwrap_or_else(|error| MdAttributes::Failed { form_id, error })
    }

    fn enum_synonyms_of(
        &mut self,
        types: &[MdType],
    ) -> Result<Vec<(usize, Vec<String>)>, SessionError> {
        let mut out = Vec::new();
        for (i, t) in types.iter().enumerate() {
            if t.kind != MdKind::Enum || t.values.is_empty() {
                continue;
            }
            out.push((i, self.enum_texts(&enums::enum_type(t)?)?));
        }
        Ok(out)
    }

    fn metadata_answers(&mut self) -> Result<Metadata, SessionError> {
        let pro = self.prologue(true)?;
        let conf = pro.conf.as_str();
        let types = pro
            .types
            .ok_or_else(|| SessionError::Protocol("login response had no types".into()))?;
        let roles = self
            .info
            .as_ref()
            .and_then(|i| i.roles_id.clone())
            .unwrap_or_else(|| "0".to_string());

        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "types/view",
            "GET",
            &format!("/e1cib/types/view?sysver={sysver}&confver={conf}"),
            &h,
            &[],
            &ANY_STATUS,
        )?;
        let view = if resp.status == 200 {
            Some(Self::decoded(&resp)?).filter(|b| !b.is_empty())
        } else {
            None
        };

        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "cmiStartData",
            "GET",
            &format!(
                "/e1cib/cmi/cmiStartData?sysver={sysver}&confver={conf}&fover={FOVER}&taxi=1&rolesId={roles}\
                 &navVer={ZERO_VER}&subsystemVer={ZERO_VER}&actionVer={ZERO_VER}&clang={CLANG}"
            ),
            &h,
            &[],
            &[200],
        )?;
        let cmi = Self::decoded(&resp)?;
        let tree = Metadata::from_answers(
            &MdAnswers {
                types: &types,
                view: view.as_deref(),
                cmi: &cmi,
                commands: None,
                panel: None,
            },
            None,
        )?;
        let Some(root) = tree.sections.first().map(|s| s.id.clone()) else {
            return Ok(tree);
        };

        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "cmi/commands",
            "GET",
            &format!(
                "/e1cib/cmi/commands?subtree=true&fover={FOVER}&categories=1&user=26&fo=true&fragment={root}\
                 &sysver={sysver}&confver={conf}&navVer={ZERO_VER}&rolesId={roles}&clang={CLANG}&taxi=1"
            ),
            &h,
            &[],
            &[200, 204],
        )?;
        let commands = Some(Self::decoded(&resp)?).filter(|b| !b.is_empty());
        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "cmi/commands/panel",
            "GET",
            &format!(
                "/e1cib/cmi/commands?subtree=true&fover={FOVER}&categories=4&user=26&fo=true&fragment={root}\
                 &sysver={sysver}&confver={conf}&navVer={ZERO_VER}&rolesId={roles}&clang={CLANG}&taxi=1"
            ),
            &h,
            &[],
            &ANY_STATUS,
        )?;
        let (panel, panel_status) = match resp.status {
            200 | 204 => (Some(Self::decoded(&resp)?).filter(|b| !b.is_empty()), None),
            status => (None, Some(status)),
        };
        let mut md = Metadata::from_answers(
            &MdAnswers {
                types: &types,
                view: view.as_deref(),
                cmi: &cmi,
                commands: commands.as_deref(),
                panel: panel.as_deref(),
            },
            Some(&root),
        )?;
        if let Some(status) = panel_status {
            md.unrecognized.push(crate::info::Unrecognized {
                answer: "cmi/commands/panel",
                name: format!("HTTP {status}"),
            });
        }
        Ok(md)
    }

    pub fn run_modules(&mut self) -> Result<ModuleDefs, SessionError> {
        let pro = self.prologue(true)?;
        Ok(ModuleDefs {
            modules: modules::parse_defs(&pro.defs)?,
            cache: pro.defs_cache,
        })
    }

    pub fn module_source(&mut self, id: &ModuleId) -> Result<ModuleSource, SessionError> {
        let conf = self
            .info
            .as_ref()
            .ok_or_else(|| SessionError::Protocol("not logged in".into()))?
            .config
            .clone()
            .ok_or_else(|| SessionError::Protocol("login response had no config".into()))?;
        let qid = percent_encode(id.as_str());
        let sysver = self.version.clone();
        let h = self.headers("text/plain", false, false);
        let resp = self.step(
            "modules/src",
            "GET",
            &format!("/e1cib/modules/src?sysver={sysver}&confver={conf}&lm=2&id={qid}"),
            &h,
            &[],
            &[200],
        )?;
        let content_type = modules::check_source_type(resp.header("content-type"))?;
        Ok(ModuleSource {
            id: id.clone(),
            text: Self::decoded(&resp)?,
            content_type,
            cache: http_cache(&resp),
        })
    }

    pub fn run_report(&mut self, opts: ReportOptions) -> Result<ReportView, SessionError> {
        let pro = self.prologue(true)?;
        let init = match opts.init {
            Some((call, params)) => Some(FormCall {
                procedure: call.to_string(),
                returned: self.module_call(&pro, call, params)?.ret,
            }),
            None => None,
        };
        let roles = self
            .info
            .as_ref()
            .and_then(|i| i.roles_id.clone())
            .unwrap_or_else(|| "0".to_string());
        let (mut form, mut body) = if opts.open_params.is_empty() {
            self.open_report_form(opts.form, &[])?
        } else {
            let request = report::query_request_values(opts.form, opts.open_params)?;
            self.open_form_with(&request)?
        };
        if let Some(name) = opts.dcs.and_then(|d| d.variant) {
            let wanted = dcs::read_variants(&body).and_then(|(current, list)| {
                let v = dcs::find_variant(&list, name)?;
                Ok((current.is_some_and(|c| c.key == v.key), v.key.clone()))
            });
            let key = match wanted {
                Ok((true, _)) => None,
                Ok((false, key)) => Some(key),
                Err(e) => {
                    let _ = self.close_form(&form.fid);
                    return Err(e.into());
                }
            };
            if let Some(key) = key {
                self.close_form(&form.fid)?;
                (form, body) = self.open_report_form(opts.form, &[(dcs::VARIANT_PARAM, &key)])?;
                let opened = dcs::read_variants(&body).and_then(|(current, _)| match current {
                    Some(c) if c.key == key => Ok(()),
                    c => Err(dcs::DcsError::VariantNotOpened {
                        asked: key.clone(),
                        got: c.map(|c| c.key).unwrap_or_default(),
                    }),
                });
                if let Err(e) = opened {
                    let _ = self.close_form(&form.fid);
                    return Err(e.into());
                }
            }
        }
        let outcome = self.report_of(&form, &body, &pro.conf, &roles, opts);
        let closed = self.close_form(&form.fid);
        let parts = outcome?;
        closed?;
        Ok(ReportView {
            form,
            attributes: parts.attributes,
            sheets: parts.sheets,
            dcs: parts.dcs,
            init,
            calls: parts.calls,
            messages: parts.messages,
            warnings: parts.warnings,
            states: parts.states,
        })
    }

    fn open_report_form(
        &mut self,
        key: &str,
        params: &[(&str, &str)],
    ) -> Result<(OpenedForm, Vec<u8>), SessionError> {
        self.open_form_with(&report::query_request_with(key, params))
    }

    fn open_form_with(&mut self, request: &[u8]) -> Result<(OpenedForm, Vec<u8>), SessionError> {
        let body = body::zstd_compress(request);
        let h = self.headers("application/xml", true, true);
        let resp = self.step(
            "logForm/query",
            "POST",
            "/e1cib/logForm?cmd=query",
            &h,
            &body,
            &[200],
        )?;
        let body = Self::decoded(&resp)?;
        match report::read_opened(&body) {
            Ok(f) => Ok((f, body)),
            Err(e) => {
                if let Some(fid) = report::fid_of(&body) {
                    let _ = self.close_form(&fid);
                }
                Err(e.into())
            }
        }
    }

    fn report_of(
        &mut self,
        form: &OpenedForm,
        query: &[u8],
        conf: &str,
        roles: &str,
        opts: ReportOptions,
    ) -> Result<ReportParts, SessionError> {
        let sysver = self.version.clone();
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "logForm/dataDescrWMethods",
            "GET",
            &format!(
                "/e1cib/logForm?sysver={sysver}&cmd=dataDescrWMethods&formId={}&confver={conf}&rolesId={roles}",
                form.id
            ),
            &h,
            &[],
            &[200],
        )?;
        let descr = Self::decoded(&resp)?;
        let mut attributes = report::read_attributes(&descr)?;
        if let Some(d) = opts.dcs {
            let dcs_form = match dcs::read_form(query, &descr, &attributes) {
                Ok(f) => f,
                Err(dcs::DcsError::NoComposer) if !d.generate => {
                    return Ok(ReportParts {
                        attributes,
                        ..Default::default()
                    });
                }
                Err(e) => return Err(e.into()),
            };
            if !d.generate {
                return Ok(ReportParts {
                    attributes,
                    dcs: Some(DcsView {
                        form: dcs_form,
                        details: Vec::new(),
                    }),
                    ..Default::default()
                });
            }
            let (sheets, details) = self.compose(form, &dcs_form, d, opts.limits)?;
            return Ok(ReportParts {
                attributes,
                sheets,
                dcs: Some(DcsView {
                    form: dcs_form,
                    details,
                }),
                ..Default::default()
            });
        }
        let mut warnings = Vec::new();
        let opened = self.opened_sheets(query, &mut attributes, opts.limits);
        let mut sheets = match (opened, opts.call.is_some()) {
            (Ok(s), _) => s,
            (Err(e), false) => return Err(e),
            (Err(e), true) => {
                warnings.push(format!(
                    "the documents of the opened form were not read: {e}"
                ));
                Vec::new()
            }
        };
        let Some((procedure, params)) = opts.call else {
            let messages = self.messages_or_warning(&mut warnings);
            return Ok(ReportParts {
                attributes,
                sheets,
                messages,
                warnings,
                ..Default::default()
            });
        };
        let edits = if opts.attributes.is_empty() {
            Vec::new()
        } else {
            let types = self.session_types("the values of the attributes of the form")?;
            report::attribute_edits(&attributes, opts.attributes, &types)?
        };
        let mut pchs = Some(report::attributes_pchs(&edits));
        let mut calls = Vec::new();
        let mut states = Vec::new();
        for b in opts.before {
            let p = pchs
                .take()
                .unwrap_or_else(|| report::EMPTY_PCHS.to_string());
            let (returned, _) =
                self.form_call(form, b, &[], &p, opts.limits, &mut sheets, &mut states)?;
            calls.push(FormCall {
                procedure: b.clone(),
                returned,
            });
        }
        let p = pchs
            .take()
            .unwrap_or_else(|| report::EMPTY_PCHS.to_string());
        let (returned, changed) = self.form_call(
            form,
            procedure,
            params,
            &p,
            opts.limits,
            &mut sheets,
            &mut states,
        )?;
        let started = !matches!(returned, Some(Value::Bool(false))) && !changed;
        calls.push(FormCall {
            procedure: procedure.to_string(),
            returned,
        });
        if let (Some(poll), true) = (opts.poll, started) {
            self.poll_form(
                form,
                poll,
                opts.limits,
                &mut sheets,
                &mut calls,
                &mut states,
            )?;
        }
        let messages = self.messages_or_warning(&mut warnings);
        Ok(ReportParts {
            attributes,
            sheets,
            dcs: None,
            calls,
            messages,
            warnings,
            states,
        })
    }

    fn opened_sheets(
        &mut self,
        query: &[u8],
        attributes: &mut Vec<FormAttribute>,
        limits: SheetLimits,
    ) -> Result<Vec<ReportSheet>, SessionError> {
        let added = report::read_added_attributes(query, attributes.len())?;
        attributes.extend(added);
        let mut count = CellCount::default();
        let opened = report::read_opened_sheets(query, attributes, limits, &mut count)?;
        self.fill_sheets(opened, limits, &mut count)
    }

    fn messages_or_warning(
        &mut self,
        warnings: &mut Vec<String>,
    ) -> Vec<crate::write::UserMessage> {
        match self.user_messages() {
            Ok(m) => m,
            Err(e) => {
                warnings.push(format!("the messages to the user were not read: {e}"));
                Vec::new()
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn form_call(
        &mut self,
        form: &OpenedForm,
        procedure: &str,
        params: &[Value],
        pchs: &str,
        limits: SheetLimits,
        sheets: &mut Vec<ReportSheet>,
        states: &mut Vec<String>,
    ) -> Result<(Option<Value>, bool), SessionError> {
        let body = body::zstd_compress(&report::call_request_with(form, procedure, params, pchs)?);
        let h = self.headers("application/xml", true, true);
        let resp = self.step(
            "logForm/call",
            "POST",
            "/e1cib/logForm?cmd=call",
            &h,
            &body,
            &[200],
        )?;
        let answer = Self::decoded(&resp)?;
        let returned = report::read_call_return(&answer)?;
        *states = report::read_states(&answer);
        let changed = self.sheets_of(&answer, limits)?;
        let any = !changed.is_empty();
        for sheet in changed {
            match sheets.iter_mut().find(|s| s.position == sheet.position) {
                Some(old) => *old = sheet,
                None => sheets.push(sheet),
            }
        }
        Ok((returned, any))
    }

    fn poll_form(
        &mut self,
        form: &OpenedForm,
        poll: Poll,
        limits: SheetLimits,
        sheets: &mut Vec<ReportSheet>,
        calls: &mut Vec<FormCall>,
        states: &mut Vec<String>,
    ) -> Result<(), SessionError> {
        let started = std::time::Instant::now();
        let mut waited = std::time::Duration::ZERO;
        let mut pause = POLL_FIRST;
        let mut polls = 0usize;
        loop {
            if waited.max(started.elapsed()) + pause > poll.timeout {
                return Err(ReportError::PollTimeout {
                    procedure: poll.procedure.to_string(),
                    seconds: poll.timeout.as_secs(),
                    calls: polls,
                }
                .into());
            }
            (self.sleep)(pause);
            waited += pause;
            pause = pause.mul_f64(POLL_FACTOR).min(POLL_MAX);
            polls += 1;
            let (returned, changed) = self.form_call(
                form,
                poll.procedure,
                &[],
                report::EMPTY_PCHS,
                limits,
                sheets,
                states,
            )?;
            let running =
                !changed && report::poll_running(poll.procedure, returned.as_ref(), poll.running)?;
            calls.push(FormCall {
                procedure: poll.procedure.to_string(),
                returned,
            });
            if !running {
                return Ok(());
            }
        }
    }

    fn compose(
        &mut self,
        form: &OpenedForm,
        dcs_form: &dcs::DcsForm,
        opts: DcsOptions,
        limits: SheetLimits,
    ) -> Result<(Vec<ReportSheet>, Vec<dcs::DetailsItem>), SessionError> {
        let types = if opts.changes.is_empty() {
            dcs::RefTypes::default()
        } else {
            dcs::RefTypes::new(&self.session_types("the values of the report settings")?)
        };
        let mut edits = dcs::resolve(dcs_form, opts.changes, &types)?;
        let mut failure = None;
        dcs::settle_searches(&mut edits, dcs_form, |clsid, text| {
            let body = body::zstd_compress(&dcs::search_request(clsid, text));
            let h = self.headers("application/xml", true, true);
            let resp = self.step(
                "logForm/querySearchResult",
                "POST",
                "/e1cib/logForm?cmd=querySearchResult",
                &h,
                &body,
                &[200],
            );
            match resp.and_then(|r| Self::decoded(&r)) {
                Ok(b) => dcs::read_search(&b),
                Err(e) => {
                    failure = Some(e);
                    Ok(Vec::new())
                }
            }
        })
        .map_err(|e| failure.take().unwrap_or(SessionError::Dcs(e)))?;
        let pchs = dcs_form.compose_pchs(&edits)?;
        let body = body::zstd_compress(&report::call_request_with(
            form,
            dcs::COMPOSE_RESULT,
            &dcs::compose_params(),
            &pchs,
        )?);
        let h = self.headers("application/xml", true, true);
        let resp = self.step(
            "logForm/call",
            "POST",
            "/e1cib/logForm?cmd=call",
            &h,
            &body,
            &[200],
        )?;
        let answer = Self::decoded(&resp)?;
        let sheets = self.sheets_of(&answer, limits)?;
        if sheets.is_empty() {
            return Err(dcs::DcsError::NoDocument.into());
        }
        let mut details = std::collections::BTreeMap::new();
        let ids: Vec<u32> = sheets
            .iter()
            .flat_map(|s| s.sheet.cells.iter().filter_map(|c| c.details_id))
            .collect();
        if let (true, Some(address)) = (
            opts.details && !ids.is_empty(),
            dcs::details_address(&answer)?,
        ) {
            for (from, to) in dcs::details_intervals(ids, DETAILS_PER_REQUEST) {
                let body = body::zstd_compress(&dcs::details_request(&address, from, to));
                let h = self.headers("application/xml", true, true);
                let resp = self.step(
                    "dcs/loadDetailsPart",
                    "POST",
                    "/e1cib/dcs?cmd=loadDetailsPart",
                    &h,
                    &body,
                    &[200],
                )?;
                for item in dcs::read_details(&Self::decoded(&resp)?)? {
                    details.entry(item.id).or_insert(item);
                }
                dcs::details_bound(details.len(), limits.cells)?;
            }
        }
        let details = details.into_values().collect();
        Ok((sheets, details))
    }

    fn sheets_of(
        &mut self,
        answer: &[u8],
        limits: SheetLimits,
    ) -> Result<Vec<ReportSheet>, SessionError> {
        let mut count = CellCount::default();
        let changed = report::read_call(answer, limits, &mut count)?;
        self.fill_sheets(changed, limits, &mut count)
    }

    fn fill_sheets(
        &mut self,
        changed: Vec<report::ChangedSheet>,
        limits: SheetLimits,
        count: &mut CellCount,
    ) -> Result<Vec<ReportSheet>, SessionError> {
        let mut sheets = Vec::with_capacity(changed.len());
        for c in changed {
            let (sheet, rows_asked) = match c.changes {
                SheetChanges::Full(sheet) => (sheet, false),
                SheetChanges::WithoutContent { key, mut sheet } => {
                    let rows: Vec<u32> = (0..sheet.height).collect();
                    for batch in rows.chunks(ROWS_PER_REQUEST) {
                        let body =
                            body::zstd_compress(&spreadsheet::row_contents_request(&key, batch));
                        let h = self.headers("application/xml", true, true);
                        let resp = self.step(
                            "moxel/getRowContents",
                            "POST",
                            "/e1cib/moxel?cmd=getRowContents",
                            &h,
                            &body,
                            &[200],
                        )?;
                        spreadsheet::read_row_contents(
                            &Self::decoded(&resp)?,
                            batch,
                            &mut sheet,
                            limits,
                            count,
                        )
                        .map_err(ReportError::from)?;
                    }
                    sheet.settle(limits).map_err(ReportError::from)?;
                    (sheet, true)
                }
            };
            sheets.push(ReportSheet {
                position: c.position,
                sheet,
                rows_asked,
            });
        }
        Ok(sheets)
    }

    pub fn logout(&mut self) -> Result<(), SessionError> {
        if self.vrs.is_none() {
            return Ok(());
        }
        let mut h = self.headers("*/*", true, false);
        h.push((
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        self.step("logout", "POST", "/e1cib/logout", &h, &[], &[200])?;
        Ok(())
    }
}

fn http_cache(resp: &Response) -> HttpCache {
    HttpCache {
        etag: resp.header("etag").map(str::to_string),
        cache_control: resp.header("cache-control").map(str::to_string),
    }
}

fn sid_from_vrs(vrs: &str) -> Result<String, SessionError> {
    let padded = {
        let mut s = vrs.to_string();
        while !s.len().is_multiple_of(4) {
            s.push('=');
        }
        s
    };
    let raw = base64::engine::general_purpose::URL_SAFE
        .decode(padded.as_bytes())
        .map_err(|e| SessionError::Protocol(format!("bad vrs-session: {e}")))?;
    if raw.len() < 36 {
        return Err(SessionError::Protocol(
            "vrs-session shorter than 36 bytes".into(),
        ));
    }
    String::from_utf8(raw[..36].to_vec())
        .map_err(|e| SessionError::Protocol(format!("bad sid: {e}")))
}

fn with_version(query: &str, build: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut set = false;
    for p in query.split('&').filter(|p| !p.is_empty()) {
        if p.starts_with("version=") {
            if !set {
                parts.push(format!("version={build}"));
                set = true;
            }
        } else {
            parts.push(p.to_string());
        }
    }
    if !set {
        parts.push(format!("version={build}"));
    }
    parts.join("&")
}

fn server_build(resp: &Response) -> Option<String> {
    let body = body::decode(&resp.body, resp.header("content-encoding")).unwrap_or_default();
    let text = String::from_utf8_lossy(&body);
    let from_body = text
        .find("<?xml-stylesheet")
        .map(|i| &text[i..])
        .and_then(|pi| pi.find("?>").map(|end| &pi[..end]))
        .and_then(|pi| pi.find("sysver=").map(|i| &pi[i + "sysver=".len()..]))
        .map(|v| v.split(['&', '"', '\'']).next().unwrap_or(""))
        .filter(|v| is_build(v));
    let from_header = || {
        resp.header("server")
            .and_then(|s| s.trim().strip_prefix("1C:Enterprise/"))
            .map(|v| v.split_whitespace().next().unwrap_or(""))
            .filter(|v| is_build(v))
    };
    from_body.or_else(from_header).map(str::to_string)
}

pub fn is_build(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (2..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn login_query(cln_pid: u32, cln_id: &str, nm: &str) -> String {
    format!(
        "vl=ru_RU&version={VERSION}&clnPid={cln_pid}&nm={nm}&dpps=Letter&ni=1980582150&dppx=600&dppy=600&dpph=2794&screenwidthcharunits=936&clnId={cln_id}"
    )
}

pub fn new_cln_id() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        b = *b"onecclientclnid0";
    }
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

pub fn client_nm() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

const START_INFO_BODY: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><request xmlns=\"http://v8.1c.ru/8.2/mngsrv/ws\" xmlns:d1p1=\"http://v8.1c.ru/8.1/data/core\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"d1p1:FixedArray\"><d1p1:Value xmlns:app=\"http://v8.1c.ru/8.2/managed-application/core\" xsi:type=\"app:ClientDisplayInformation\" x=\"1024\" y=\"768\" dpi=\"96\"/></request>";
