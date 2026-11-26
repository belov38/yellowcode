pub use crate::answer::Unrecognized;
use crate::answer::{
    NS_APP, NS_CMI, NS_MODULES, Reader, attr, boolean, guid, is, read, signed, text_of, unsigned,
};
use crate::auth::AuthSettings;

pub type InfoError = crate::answer::AnswerError;

use crate::metadata::{MdKind, read_types};

pub const HIDDEN_LOGIN: &[&str] = &[
    "credAns6",
    "seanceLicenseInfoPrs",
    "userRunMode",
    "configRunMode",
    "compatibilityModeLive",
    "interfaceCompatibilityMode",
    "userSeparatorsEmpty",
    "training",
    "modalityUseMode",
    "rigthClientsStart",
    "syncCallsUseMode",
    "interactiveSecurity",
    "defaultPaperWidth",
    "defaultPaperHeight",
    "beginningOfCentury",
    "countryUA",
];

pub const HIDDEN_START_INFO: &[&str] = &[
    "navStartPoint",
    "desktop",
    "styleEntries",
    "extensionLicensingText",
    "panels",
    "choiceHistory",
    "externalResourceMode",
    "errorProcessingSettings",
    "configHash",
    "dataZoneString",
    "configChangeEnabled",
    "serverNeedFunctionalities",
    "urlExtDataSupport",
    "startupErrorProcessingSettings",
    "appVariant",
    "dataZoneStringExclCondOff",
    "confCaption",
    "confShortCaption",
];

pub const HIDDEN_ABOUT: &[&str] = &[
    "title",
    "user",
    "config",
    "compress",
    "configLicenseInfo",
    "developerInfo",
];

pub const HIDDEN_CMI: &[&str] = &[
    "defaultNPHandleInfo",
    "urls",
    "panels",
    "commandsWithShortCuts",
];

#[derive(Debug, Clone, Copy)]
pub struct Answers<'a> {
    pub login: &'a [u8],
    pub types: Option<&'a [u8]>,
    pub start_info: &'a [u8],
    pub defs: &'a [u8],
    pub members: &'a [(String, Vec<u8>)],
    pub about: &'a [u8],
    pub cmi: Option<&'a [u8]>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerInfo {
    pub server: ServerPart,
    pub infobase: InfobasePart,
    pub config: ConfigPart,
    pub user: UserPart,
    pub session: SessionPart,
    pub modules: ModulesPart,
    pub types: Option<TypesPart>,
    pub sections: Option<Vec<Section>>,
    pub auth_settings: Option<AuthSettings>,
    pub unrecognized: Vec<Unrecognized>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerPart {
    pub version: Option<String>,
    pub server_type: Option<String>,
    pub dbms: Option<String>,
    pub deployment_type: Option<String>,
    pub compress: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InfobasePart {
    pub alias: Option<String>,
    pub instance_id: Option<String>,
    pub file_mode: Option<bool>,
    pub locale: Option<String>,
    pub langs: Langs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Langs {
    pub current: Option<String>,
    pub default: Option<String>,
    pub list: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigPart {
    pub name: Option<String>,
    pub full_name: Option<String>,
    pub code: Option<String>,
    pub version: Option<String>,
    pub hash: String,
    pub assembly_version: Option<String>,
    pub compatibility_mode: Option<u32>,
    pub has_extensions: Option<bool>,
    pub license_check: Option<String>,
    pub presentation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserPart {
    pub name: Option<String>,
    pub full_name: Option<String>,
    pub id: Option<String>,
    pub roles_id: Option<String>,
    pub password: PasswordPolicy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PasswordPolicy {
    pub min_length: Option<u32>,
    pub expiration_ticks_left: Option<i64>,
    pub compliance_status: Option<i64>,
    pub cannot_change: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPart {
    pub seance: Option<String>,
    pub seance_no: Option<u32>,
    pub licenses: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModulesPart {
    pub main: Option<String>,
    pub server: Vec<ServerModule>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerModule {
    pub code: String,
    pub id: String,
    pub members: Option<Vec<Member>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub kind: MemberKind,
    pub name: String,
    pub params: u32,
    pub defaults: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Func,
    Proc,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypesPart {
    pub hash: Option<String>,
    pub type_sets: Vec<(String, String)>,
    pub objects: Vec<(MdKind, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub id: String,
    pub name: Option<String>,
    pub text: Option<String>,
}

impl ServerInfo {
    pub fn from_answers(a: &Answers) -> Result<ServerInfo, InfoError> {
        let mut info = ServerInfo::default();
        let mut unrecognized = Vec::new();
        let types_hash = read(
            "login",
            a.login,
            NS_APP,
            HIDDEN_LOGIN,
            &mut unrecognized,
            |r| login(r, &mut info),
        )?;
        if let Some(body) = a.types {
            let mut t = read("types", body, NS_APP, &[], &mut unrecognized, types)?;
            t.hash = types_hash;
            info.types = Some(t);
        }
        read(
            "startInfo",
            a.start_info,
            NS_APP,
            HIDDEN_START_INFO,
            &mut unrecognized,
            |r| start_info(r, &mut info),
        )?;
        info.modules = read(
            "modules/defs",
            a.defs,
            NS_MODULES,
            &[],
            &mut unrecognized,
            defs,
        )?;
        for (code, body) in a.members {
            let module = info
                .modules
                .server
                .iter_mut()
                .find(|m| &m.code == code)
                .ok_or_else(|| InfoError::Missing {
                    answer: "modules/defs",
                    field: format!("server {code}"),
                })?;
            module.members = Some(read(
                "modules/defs?id",
                body,
                NS_MODULES,
                &[],
                &mut unrecognized,
                members,
            )?);
        }
        read(
            "about",
            a.about,
            NS_APP,
            HIDDEN_ABOUT,
            &mut unrecognized,
            |r| about(r, &mut info),
        )?;
        if let Some(body) = a.cmi {
            info.sections = Some(read(
                "cmiStartData",
                body,
                NS_CMI,
                HIDDEN_CMI,
                &mut unrecognized,
                sections,
            )?);
        }
        info.unrecognized = unrecognized;
        Ok(info)
    }
}

fn login(r: &Reader, info: &mut ServerInfo) -> Result<Option<String>, InfoError> {
    let ib = &mut info.infobase;
    ib.instance_id = r.parsed(NS_APP, "infoBaseInstanceID", guid)?;
    ib.alias = r.text(NS_APP, "infoBaseAlias")?;
    ib.file_mode = r.parsed(NS_APP, "infoBaseInFileMode", boolean)?;
    ib.locale = r.one(NS_APP, "locale")?.and_then(|n| attr(n, "name"));
    if let Some(langs) = r.one(NS_APP, "langs")? {
        ib.langs.current = attr(langs, "current");
        ib.langs.default = attr(langs, "default");
        for lang in langs.children().filter(|n| is(*n, NS_APP, "lang")) {
            let id = attr(lang, "id").ok_or_else(|| r.missing("lang id"))?;
            ib.langs.list.push((id, text_of(lang).unwrap_or_default()));
        }
    }
    let c = &mut info.config;
    c.hash = r
        .text(NS_APP, "config")?
        .ok_or_else(|| r.missing("config"))?;
    c.assembly_version = r.text(NS_APP, "assemblyVersion")?;
    c.compatibility_mode = r.parsed(NS_APP, "compatibilityMode", unsigned)?;
    let u = &mut info.user;
    u.id = r.parsed(NS_APP, "user", guid)?;
    u.roles_id = r.text(NS_APP, "rolesID")?;
    let p = &mut u.password;
    p.min_length = r.parsed(NS_APP, "passwordMinLength", unsigned)?;
    p.expiration_ticks_left = r.parsed(NS_APP, "passwordExpirationTicksLeft", signed)?;
    p.compliance_status = r.parsed(NS_APP, "passwordRequirementsComplianceStatus", signed)?;
    p.cannot_change = r.parsed(NS_APP, "cannotChangePassword", boolean)?;
    let s = &mut info.session;
    s.seance = r.parsed(NS_APP, "seance", guid)?;
    s.seance_no = r.parsed(NS_APP, "seanceNo", unsigned)?;
    r.text(NS_APP, "types")
}

fn types(r: &Reader) -> Result<TypesPart, InfoError> {
    let mut t = TypesPart::default();
    for n in r.children(NS_APP, "typeSet") {
        let clsid = r.attr_parsed(n, "clsid", "typeSet clsid", guid)?;
        let clsid = clsid.ok_or_else(|| r.missing("typeSet clsid"))?;
        let name = attr(n, "name").ok_or_else(|| r.missing("typeSet name"))?;
        t.type_sets.push((name, clsid));
    }
    for o in read_types(r)? {
        match t.objects.iter_mut().find(|(k, _)| *k == o.kind) {
            Some((_, n)) => *n += 1,
            None => t.objects.push((o.kind, 1)),
        }
    }
    Ok(t)
}

fn start_info(r: &Reader, info: &mut ServerInfo) -> Result<(), InfoError> {
    let root = r.root;
    info.config.name = attr(root, "confName");
    info.config.has_extensions = r.attr_parsed(root, "hasConfigExt", "hasConfigExt", boolean)?;
    info.config.full_name = r.text(NS_APP, "fullConfName")?;
    info.config.code = r.text(NS_APP, "configCode")?;
    info.config.version = r.text(NS_APP, "configVersion")?;
    info.config.license_check = r.text_same(NS_APP, "configLicenseInfo")?;
    info.user.name = r.text(NS_APP, "userName")?;
    info.user.full_name = r.text(NS_APP, "fullUserName")?;
    let s = &mut info.server;
    s.version = r.text(NS_APP, "serverVersion")?;
    s.server_type = r.text(NS_APP, "serverType")?;
    s.dbms = r.text(NS_APP, "serverDBMS")?;
    s.deployment_type = r.text(NS_APP, "deploymentType")?;
    s.compress = r.text(NS_APP, "compress")?;
    Ok(())
}

fn defs(r: &Reader) -> Result<ModulesPart, InfoError> {
    let main = r.one(NS_MODULES, "main")?.and_then(|n| attr(n, "name"));
    let mut server = Vec::new();
    for n in r.children(NS_MODULES, "server") {
        server.push(ServerModule {
            code: attr(n, "code").ok_or_else(|| r.missing("server code"))?,
            id: attr(n, "id").ok_or_else(|| r.missing("server id"))?,
            members: None,
        });
    }
    Ok(ModulesPart { main, server })
}

fn members(r: &Reader) -> Result<Vec<Member>, InfoError> {
    let mut out = Vec::new();
    for n in r.children_of(NS_MODULES, &["func", "proc"]) {
        let (kind, what) = if n.tag_name().name() == "func" {
            (MemberKind::Func, "func name")
        } else {
            (MemberKind::Proc, "proc name")
        };
        let name = attr(n, "name").ok_or_else(|| r.missing(what))?;
        let params = r.attr_parsed(n, "params", "params", unsigned)?.unwrap_or(0);
        let mut defaults = Vec::new();
        for p in n.children().filter(|p| is(*p, NS_MODULES, "param")) {
            let pos = r
                .attr_parsed(p, "pos", "param pos", unsigned)?
                .ok_or_else(|| r.missing("param pos"))?;
            if r.attr_parsed(p, "def", "param def", boolean)? == Some(true) {
                defaults.push(pos.saturating_add(1));
            }
        }
        out.push(Member {
            kind,
            name,
            params,
            defaults,
        });
    }
    Ok(out)
}

fn about(r: &Reader, info: &mut ServerInfo) -> Result<(), InfoError> {
    info.config.presentation = r.text(NS_APP, "presentation")?;
    info.session.licenses = r
        .text(NS_APP, "runMode")?
        .map(|t| {
            t.lines()
                .map(|l| l.trim_end_matches('\r'))
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(())
}

fn sections(r: &Reader) -> Result<Vec<Section>, InfoError> {
    let mut out = Vec::new();
    let Some(top) = r.one(NS_CMI, "toplevelSubsystems")? else {
        return Ok(out);
    };
    for n in top.children().filter(|n| is(*n, NS_CMI, "section")) {
        let id = r
            .attr_parsed(n, "id", "section id", guid)?
            .ok_or_else(|| r.missing("section id"))?;
        let text = n
            .children()
            .find(|c| is(*c, NS_CMI, "text"))
            .and_then(text_of);
        out.push(Section {
            id,
            name: attr(n, "name"),
            text,
        });
    }
    Ok(out)
}
