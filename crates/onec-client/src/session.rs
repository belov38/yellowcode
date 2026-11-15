use base64::Engine;

use crate::answer::{NS_APP, read};
use crate::auth::{AuthSettings, AuthSettingsError};
use crate::body::{self, ACCEPT_ENCODING};
use crate::cred;
use crate::enums::{self, EnumPresentations, EnumType};
use crate::info::{Answers, InfoError, ServerInfo};
use crate::metadata::{MdAnswers, MdKind, MdType, Metadata, read_types};
use crate::modules::{self, HttpCache, ModuleDefs, ModuleId, ModuleSource, ModulesError};
use crate::refs::{self, Presentations};
use crate::transport::{HttpTransport, Response, TransportError};
use crate::value::{EnumRef, Ref, Uuid, Value};
use crate::xml::{self, LoginInfo, XmlError};

pub const VERSION: &str = "8.5.1.1529";

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
    #[error("random source failed: {0}")]
    Random(String),
    #[error("{step}: HTTP {status} {message}")]
    Step {
        step: &'static str,
        status: u16,
        message: String,
    },
    #[error("password rejected (round 2 returned empty cred)")]
    PasswordRejected,
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
}

#[derive(Debug)]
pub struct MetadataView {
    pub metadata: Metadata,
    pub enum_synonyms_error: Option<SessionError>,
}

pub struct Session<R: RandomSource = OsRandom> {
    tr: HttpTransport,
    rng: R,
    vrs: Option<String>,
    sync: u32,
    info: Option<LoginInfo>,
    login_body: Vec<u8>,
    types: Option<Vec<u8>>,
    enum_texts: std::collections::HashMap<Uuid, Vec<String>>,
    steps: Vec<Step>,
}

struct Prologue {
    conf: String,
    types: Option<Vec<u8>>,
    start_info: Vec<u8>,
    defs: Vec<u8>,
    defs_cache: HttpCache,
}

impl Session<OsRandom> {
    pub fn new(tr: HttpTransport) -> Self {
        Self::with_rng(tr, OsRandom)
    }
}

impl<R: RandomSource> Session<R> {
    pub fn with_rng(tr: HttpTransport, rng: R) -> Self {
        Self {
            tr,
            rng,
            vrs: None,
            sync: 0,
            info: None,
            login_body: Vec::new(),
            types: None,
            enum_texts: Default::default(),
            steps: Vec::new(),
        }
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

    fn headers(&mut self, accept: &str, state: bool, xml_body: bool) -> Vec<(String, String)> {
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

    fn step(
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
                return Err(e.into());
            }
        };
        let ok = accept.contains(&resp.status);
        self.steps.push(Step {
            name,
            status: resp.status,
            ok,
        });
        if !ok {
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

    fn decoded(resp: &Response) -> Result<Vec<u8>, SessionError> {
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
        self.forget_types();
        let noise: [u8; 16] = self.rand()?;
        let pad1: [u8; 16] = self.rand()?;
        let blob = cred::cred_blob(&cred::build_cred_req1(c.user, &noise, &pad1));
        let (h, b) = self.cred_json(&blob);
        let resp = self.step(
            "login/cred1",
            "POST",
            &format!("/e1cib/login?{query}"),
            &h,
            &b,
            &[200],
        )?;
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
        self.forget_types();
        let mut h = self.headers("*/*", false, false);
        h.push((
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        let resp = self.step(
            "login",
            "POST",
            &format!("/e1cib/login?{query}"),
            &h,
            &[],
            &[200],
        )?;
        self.vrs = resp.header("vrs-session").map(|s| s.to_string());
        self.set_login(Self::decoded(&resp)?)
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

    fn prologue(&mut self, with_types: bool) -> Result<Prologue, SessionError> {
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

        let h = self.headers("*/*", false, false);
        self.step(
            "splash",
            "GET",
            &format!("/e1cib/metadata/splash?sysver={VERSION}&confver={conf}&scale=100&interfaceVar=16&operatingSystem=1&theme=1&extract=true"),
            &h,
            &[],
            &[200, 404],
        )?;

        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "modules/defs",
            "GET",
            &format!("/e1cib/modules/defs?sysver={VERSION}&confver={conf}&lm=2"),
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
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "types",
            "GET",
            &format!("/e1cib/types?sysver={VERSION}&types={key}"),
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

    fn session_types(&mut self, what: &str) -> Result<Vec<MdType>, SessionError> {
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
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "types/view/values",
            "GET",
            &enums::values_path(t.clsid, VERSION, key.as_deref()),
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
        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "modules/defs?id",
            "GET",
            &format!("/e1cib/modules/defs?sysver={VERSION}&confver={conf}&lm=2&id={qid}"),
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

        self.about()?;

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
            let h = self.headers("application/xml", false, false);
            let resp = self.step(
                "cmiStartData",
                "GET",
                &format!("/e1cib/cmi/cmiStartData?sysver={VERSION}&confver={conf}"),
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

        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "types/view",
            "GET",
            &format!("/e1cib/types/view?sysver={VERSION}&confver={conf}"),
            &h,
            &[],
            &ANY_STATUS,
        )?;
        let view = if resp.status == 200 {
            Some(Self::decoded(&resp)?).filter(|b| !b.is_empty())
        } else {
            None
        };

        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "cmiStartData",
            "GET",
            &format!(
                "/e1cib/cmi/cmiStartData?sysver={VERSION}&confver={conf}&fover={FOVER}&taxi=1&rolesId={roles}\
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
            },
            None,
        )?;
        let Some(root) = tree.sections.first().map(|s| s.id.clone()) else {
            return Ok(tree);
        };

        let h = self.headers("application/xml", false, false);
        let resp = self.step(
            "cmi/commands",
            "GET",
            &format!(
                "/e1cib/cmi/commands?subtree=true&fover={FOVER}&categories=1&user=26&fo=true&fragment={root}\
                 &sysver={VERSION}&confver={conf}&navVer={ZERO_VER}&rolesId={roles}&clang={CLANG}&taxi=1"
            ),
            &h,
            &[],
            &[200, 204],
        )?;
        let commands = Some(Self::decoded(&resp)?).filter(|b| !b.is_empty());
        Ok(Metadata::from_answers(
            &MdAnswers {
                types: &types,
                view: view.as_deref(),
                cmi: &cmi,
                commands: commands.as_deref(),
            },
            Some(&root),
        )?)
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
        let h = self.headers("text/plain", false, false);
        let resp = self.step(
            "modules/src",
            "GET",
            &format!("/e1cib/modules/src?sysver={VERSION}&confver={conf}&lm=2&id={qid}"),
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
