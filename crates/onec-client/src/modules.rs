use roxmltree::Node;

use crate::answer::{self, AnswerError, NS_MODULES, NS_WS, Unrecognized};
use crate::info::MemberKind;
use crate::value::Uuid;

const DEFS: &str = "modules/defs";
const FORM: &str = "logForm";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModulesError {
    #[error(transparent)]
    Answer(#[from] AnswerError),
    #[error("{answer}: unknown element <{name}>")]
    UnknownElement { answer: &'static str, name: String },
    #[error("{answer}: unknown attribute {attr:?} of <{element}>")]
    UnknownAttribute {
        answer: &'static str,
        element: &'static str,
        attr: String,
    },
    #[error("{answer}: unexpected text in <{element}>")]
    UnexpectedText {
        answer: &'static str,
        element: &'static str,
    },
    #[error("module id of an unknown shape: {0:?}")]
    BadId(String),
    #[error("modules/defs: module id {0} is listed twice")]
    DuplicateId(String),
    #[error("modules/src: the answer is {0:?}, not text/plain")]
    NotText(String),
    #[error("modules/src: the answer has no Content-Type, so it is not known to be text")]
    NoContentType,
    #[error("logForm: {context} and {without_context} are not the two parts of one form module")]
    FormParts {
        context: String,
        without_context: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    Main,
    Client,
    Server,
}

impl ModuleKind {
    pub fn tag(self) -> &'static str {
        match self {
            ModuleKind::Main => "main",
            ModuleKind::Client => "client",
            ModuleKind::Server => "server",
        }
    }

    pub fn is_client(self) -> bool {
        matches!(self, ModuleKind::Main | ModuleKind::Client)
    }

    fn naming(self) -> &'static [&'static str] {
        match self {
            ModuleKind::Main => &["name"],
            ModuleKind::Client => &["code", "name"],
            ModuleKind::Server => &["code"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleId {
    raw: String,
    object: (usize, usize),
    property: (usize, usize),
    ext: Option<(usize, usize)>,
    version: (usize, usize),
}

const ID_PREFIX: &str = "urn:module:md:";
const ID_PROPERTY: &str = "@property='";
const ID_EXT: &str = ";ext='";
const ID_VERSION: &str = ";version='";

impl ModuleId {
    pub fn parse(raw: &str) -> Result<ModuleId, ModulesError> {
        let bad = || ModulesError::BadId(raw.to_string());
        let rest = raw.strip_prefix(ID_PREFIX).ok_or_else(bad)?;
        let (object, rest) = rest.split_once(ID_PROPERTY).ok_or_else(bad)?;
        let (property, rest) = rest.split_once('\'').ok_or_else(bad)?;
        let (ext, rest) = match rest.strip_prefix(ID_EXT) {
            Some(r) => {
                let (e, r) = r.split_once('\'').ok_or_else(bad)?;
                (Some(e), r)
            }
            None => (None, rest),
        };
        let version = rest
            .strip_prefix(ID_VERSION)
            .and_then(|v| v.strip_suffix('\''))
            .ok_or_else(bad)?;
        let digits = |t: &str, ok: fn(&u8) -> bool| !t.is_empty() && t.bytes().all(|b| ok(&b));
        if Uuid::parse(object).is_err()
            || Uuid::parse(property).is_err()
            || ext.is_some_and(|e| !digits(e, u8::is_ascii_digit))
            || !digits(version, u8::is_ascii_hexdigit)
        {
            return Err(bad());
        }
        let span = |part: &str| {
            let start = part.as_ptr() as usize - raw.as_ptr() as usize;
            (start, start + part.len())
        };
        Ok(ModuleId {
            raw: raw.to_string(),
            object: span(object),
            property: span(property),
            ext: ext.map(span),
            version: span(version),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    fn part(&self, (from, to): (usize, usize)) -> &str {
        self.raw.get(from..to).unwrap_or_default()
    }

    pub fn object(&self) -> &str {
        self.part(self.object)
    }

    pub fn property(&self) -> &str {
        self.part(self.property)
    }

    pub fn ext(&self) -> Option<&str> {
        self.ext.map(|r| self.part(r))
    }

    pub fn version(&self) -> &str {
        self.part(self.version)
    }

    pub fn module_key(&self) -> &str {
        self.part((0, self.version.0 - ID_VERSION.len()))
    }
}

impl std::fmt::Display for ModuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpCache {
    pub etag: Option<String>,
    pub cache_control: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDef {
    pub kind: ModuleKind,
    pub code: Option<String>,
    pub name: Option<String>,
    pub id: ModuleId,
    pub cached: Option<bool>,
    pub security_info: Option<String>,
}

impl ModuleDef {
    pub fn title(&self) -> &str {
        self.name
            .as_deref()
            .or(self.code.as_deref())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleDefs {
    pub modules: Vec<ModuleDef>,
    pub cache: HttpCache,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSource {
    pub id: ModuleId,
    pub text: Vec<u8>,
    pub content_type: String,
    pub cache: HttpCache,
}

impl ModuleSource {
    pub fn charset_warning(&self) -> Option<String> {
        match charset(&self.content_type) {
            Some(c) if c.eq_ignore_ascii_case("utf-8") => None,
            Some(c) => Some(format!(
                "the answer is in charset {c}, not UTF-8; the bytes are kept as received"
            )),
            None => Some("the answer names no charset; the bytes are kept as received".into()),
        }
    }
}

fn charset(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches('"'))
    })
}

pub fn parse_defs(body: &[u8]) -> Result<Vec<ModuleDef>, ModulesError> {
    let mut unrecognized = Vec::new();
    let parsed = answer::read(DEFS, body, NS_WS, &[], &mut unrecognized, |r| {
        let nodes = r.children_of(NS_MODULES, &["main", "client", "server"]);
        let modules = elements(r.root, DEFS, "response").and_then(|_| {
            nodes
                .into_iter()
                .map(module_def)
                .collect::<Result<Vec<_>, _>>()
        });
        Ok(modules)
    })?;
    if let Some(Unrecognized { name, .. }) = unrecognized.into_iter().next() {
        return Err(ModulesError::UnknownElement { answer: DEFS, name });
    }
    let modules = parsed?;
    for (i, m) in modules.iter().enumerate() {
        if modules.iter().take(i).any(|p| p.id == m.id) {
            return Err(ModulesError::DuplicateId(m.id.to_string()));
        }
    }
    if modules
        .iter()
        .filter(|m| m.kind == ModuleKind::Main)
        .count()
        > 1
    {
        return Err(AnswerError::Duplicate {
            answer: DEFS,
            field: "main".into(),
        }
        .into());
    }
    Ok(modules)
}

fn module_def(n: Node) -> Result<ModuleDef, ModulesError> {
    let kind = match n.tag_name().name() {
        "main" => ModuleKind::Main,
        "client" => ModuleKind::Client,
        _ => ModuleKind::Server,
    };
    let tag = kind.tag();
    let missing = |field: &str| AnswerError::Missing {
        answer: DEFS,
        field: format!("{tag} {field}"),
    };
    for a in n.attributes() {
        let known = a.name() == "id" || a.name() == "cached" || kind.naming().contains(&a.name());
        if a.namespace().is_some() || !known {
            return Err(ModulesError::UnknownAttribute {
                answer: DEFS,
                element: tag,
                attr: a.name().to_string(),
            });
        }
    }
    let naming = |attr: &str| -> Result<Option<String>, AnswerError> {
        if !kind.naming().contains(&attr) {
            return Ok(None);
        }
        answer::attr(n, attr).map(Some).ok_or_else(|| missing(attr))
    };
    let code = naming("code")?;
    let name = naming("name")?;
    let id = ModuleId::parse(&answer::attr(n, "id").ok_or_else(|| missing("id"))?)?;
    let cached = match n.attribute("cached") {
        None => None,
        Some("0") => Some(false),
        Some("1") => Some(true),
        Some(other) => {
            return Err(AnswerError::Bad {
                answer: DEFS,
                field: format!("{tag} cached"),
                value: other.to_string(),
            }
            .into());
        }
    };
    let mut security_info = None;
    for c in elements(n, DEFS, tag)? {
        if answer::is(c, NS_MODULES, "securityInfo") {
            let value = leaf(c, DEFS, "securityInfo")?;
            set_once(&mut security_info, value, DEFS, tag, "securityInfo")?;
        } else {
            return Err(unknown(DEFS, tag, c));
        }
    }
    Ok(ModuleDef {
        kind,
        code,
        name,
        id,
        cached,
        security_info,
    })
}

fn elements<'a, 'i>(
    n: Node<'a, 'i>,
    answer: &'static str,
    element: &'static str,
) -> Result<Vec<Node<'a, 'i>>, ModulesError> {
    let mut out = Vec::new();
    for c in n.children() {
        if c.is_element() {
            out.push(c);
        } else if c.is_text() && c.text().is_some_and(|t| !t.trim().is_empty()) {
            return Err(ModulesError::UnexpectedText { answer, element });
        }
    }
    Ok(out)
}

fn unknown(answer: &'static str, parent: &str, c: Node) -> ModulesError {
    ModulesError::UnknownElement {
        answer,
        name: format!("{parent}/{}", c.tag_name().name()),
    }
}

fn leaf(n: Node, answer: &'static str, element: &'static str) -> Result<String, ModulesError> {
    no_attributes(n, answer, element)?;
    let mut out = String::new();
    for c in n.children() {
        if c.is_element() {
            return Err(unknown(answer, element, c));
        }
        if c.is_text() {
            out.push_str(c.text().unwrap_or_default());
        }
    }
    Ok(out)
}

fn no_attributes(n: Node, answer: &'static str, element: &'static str) -> Result<(), ModulesError> {
    match n.attributes().next() {
        Some(a) => Err(ModulesError::UnknownAttribute {
            answer,
            element,
            attr: a.name().to_string(),
        }),
        None => Ok(()),
    }
}

fn set_once<T>(
    slot: &mut Option<T>,
    value: T,
    answer: &'static str,
    parent: &str,
    field: &str,
) -> Result<(), ModulesError> {
    if slot.is_some() {
        return Err(AnswerError::Duplicate {
            answer,
            field: format!("{parent} {field}"),
        }
        .into());
    }
    *slot = Some(value);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormMethod {
    pub kind: MemberKind,
    pub name: String,
    pub params: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormModuleIds {
    pub module_name: String,
    pub context: ModuleId,
    pub without_context: Option<ModuleId>,
    pub security_info: Option<String>,
    pub assembly_version: Option<String>,
    pub server_methods: Vec<FormMethod>,
}

pub fn parse_form_module_ids(body: &[u8]) -> Result<FormModuleIds, ModulesError> {
    let mut unrecognized = Vec::new();
    answer::read(FORM, body, NS_WS, &[], &mut unrecognized, |r| {
        Ok(r.one(NS_WS, "moduleIds")?
            .ok_or_else(|| r.missing("moduleIds"))
            .map(module_ids))
    })??
}

fn module_ids(n: Node) -> Result<FormModuleIds, ModulesError> {
    const TAG: &str = "moduleIds";
    no_attributes(n, FORM, TAG)?;
    let (mut name, mut context, mut wo, mut security, mut assembly, mut methods) =
        (None, None, None, None, None, None);
    for c in elements(n, FORM, TAG)? {
        if c.tag_name().namespace() != Some(NS_WS) {
            return Err(unknown(FORM, TAG, c));
        }
        match c.tag_name().name() {
            "moduleName" => {
                let v = leaf(c, FORM, "moduleName")?;
                set_once(&mut name, v, FORM, TAG, "moduleName")?
            }
            "contextModuleId" => {
                let v = ModuleId::parse(&leaf(c, FORM, "contextModuleId")?)?;
                set_once(&mut context, v, FORM, TAG, "contextModuleId")?
            }
            "woContextModuleId" => {
                let v = ModuleId::parse(&leaf(c, FORM, "woContextModuleId")?)?;
                set_once(&mut wo, v, FORM, TAG, "woContextModuleId")?
            }
            "securityInfo" => {
                let v = leaf(c, FORM, "securityInfo")?;
                set_once(&mut security, v, FORM, TAG, "securityInfo")?
            }
            "assemblyVersion" => {
                let v = leaf(c, FORM, "assemblyVersion")?;
                set_once(&mut assembly, v, FORM, TAG, "assemblyVersion")?
            }
            "methods" => set_once(&mut methods, form_methods(c)?, FORM, TAG, "methods")?,
            _ => return Err(unknown(FORM, TAG, c)),
        }
    }
    let missing = |field: &str| AnswerError::Missing {
        answer: FORM,
        field: format!("{TAG} {field}"),
    };
    let module_name = name
        .filter(|t| !t.is_empty())
        .ok_or_else(|| missing("moduleName"))?;
    let context = context.ok_or_else(|| missing("contextModuleId"))?;
    check_part(&context, "3", "contextModuleId")?;
    if let Some(wo) = &wo {
        check_part(wo, "4", "woContextModuleId")?;
        if wo.object() != context.object()
            || wo.property() != context.property()
            || wo.version() != context.version()
        {
            return Err(ModulesError::FormParts {
                context: context.to_string(),
                without_context: wo.to_string(),
            });
        }
    }
    Ok(FormModuleIds {
        module_name,
        context,
        without_context: wo,
        security_info: security,
        assembly_version: assembly.filter(|t| !t.is_empty()),
        server_methods: methods.unwrap_or_default(),
    })
}

fn check_part(id: &ModuleId, ext: &str, field: &str) -> Result<(), ModulesError> {
    if id.ext() == Some(ext) {
        return Ok(());
    }
    Err(AnswerError::Bad {
        answer: FORM,
        field: format!("moduleIds {field} (ext '{ext}')"),
        value: id.to_string(),
    }
    .into())
}

fn form_methods(n: Node) -> Result<Vec<FormMethod>, ModulesError> {
    no_attributes(n, FORM, "methods")?;
    let mut srv = None;
    for c in elements(n, FORM, "methods")? {
        if !answer::is(c, NS_WS, "srvMtd") {
            return Err(unknown(FORM, "methods", c));
        }
        no_attributes(c, FORM, "srvMtd")?;
        let mut list = Vec::new();
        for m in elements(c, FORM, "srvMtd")? {
            let kind = match (m.tag_name().namespace(), m.tag_name().name()) {
                (Some(NS_MODULES), "func") => MemberKind::Func,
                (Some(NS_MODULES), "proc") => MemberKind::Proc,
                _ => return Err(unknown(FORM, "srvMtd", m)),
            };
            list.push(form_method(m, kind)?);
        }
        set_once(&mut srv, list, FORM, "methods", "srvMtd")?;
    }
    Ok(srv.unwrap_or_default())
}

fn form_method(m: Node, kind: MemberKind) -> Result<FormMethod, ModulesError> {
    let element = if kind == MemberKind::Func {
        "func"
    } else {
        "proc"
    };
    for a in m.attributes() {
        if a.namespace().is_some() || !matches!(a.name(), "name" | "params") {
            return Err(ModulesError::UnknownAttribute {
                answer: FORM,
                element,
                attr: a.name().to_string(),
            });
        }
    }
    if let Some(c) = elements(m, FORM, element)?.first() {
        return Err(unknown(FORM, element, *c));
    }
    let name = answer::attr(m, "name").ok_or_else(|| AnswerError::Missing {
        answer: FORM,
        field: format!("{element} name"),
    })?;
    let params = match answer::attr(m, "params") {
        None => 0,
        Some(t) => answer::unsigned(&t).ok_or_else(|| AnswerError::Bad {
            answer: FORM,
            field: format!("{element} params"),
            value: t.clone(),
        })?,
    };
    Ok(FormMethod { kind, name, params })
}

pub fn check_source_type(content_type: Option<&str>) -> Result<String, ModulesError> {
    let ct = content_type.unwrap_or_default();
    if ct.trim().is_empty() {
        return Err(ModulesError::NoContentType);
    }
    let media = ct.split(';').next().unwrap_or_default().trim();
    if !media.eq_ignore_ascii_case("text/plain") {
        return Err(ModulesError::NotText(ct.to_string()));
    }
    Ok(ct.to_string())
}
