use crate::json::{self, Json};

const ANSWER: &str = "additionalAuthSettings";

pub const MAX_ANSWER: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthSettingsError {
    #[error("{ANSWER}: the answer exceeds {MAX_ANSWER} bytes")]
    TooLarge,
    #[error("{ANSWER}: not JSON: {message} at byte {offset}")]
    Json {
        offset: usize,
        message: &'static str,
    },
    #[error("{ANSWER}: {} is {found}, not {expected}", printable(field))]
    Type {
        field: String,
        expected: &'static str,
        found: &'static str,
    },
    #[error(
        "{ANSWER}: bad {}: {} (expected an integer)",
        printable(field),
        printable(value)
    )]
    Integer { field: String, value: String },
    #[error("{ANSWER}: {} is repeated", printable(field))]
    Duplicate { field: String },
}

pub fn printable(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    for c in t.chars() {
        if c.is_control()
            || matches!(
                c,
                '\u{61c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            s.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            s.push(c);
        }
    }
    s
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthSettings {
    pub password_recovery_method: Option<i64>,
    pub password_recovery_url: Option<String>,
    pub allow_save_for_reauthentication: Option<bool>,
    pub save_for_reauthentication_by_default: Option<bool>,
    pub saved_authentication_lifetime: Option<i64>,
    pub lcinfo: Option<LcInfo>,
    pub enable_password_recovery: Option<bool>,
    pub inactivity_timeout: Option<i64>,
    pub inactivity_notification: Option<i64>,
    pub unrecognized: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LcInfo {
    pub auth: Option<LcAuth>,
    pub auth_types: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LcAuth {
    pub allow_save_for_reauthentication: Option<bool>,
    pub saved_authentication_lifetime: Option<i64>,
    pub save_for_reauthentication_by_default: Option<bool>,
}

impl AuthSettings {
    pub fn parse(body: &[u8]) -> Result<AuthSettings, AuthSettingsError> {
        if body.len() > MAX_ANSWER {
            return Err(AuthSettingsError::TooLarge);
        }
        let root = json::parse(body).map_err(|e| AuthSettingsError::Json {
            offset: e.offset,
            message: e.message,
        })?;
        let mut a = AuthSettings::default();
        for (key, v) in members("", &root)? {
            let field = key.as_str();
            match field {
                "passwordRecoveryMethod" => a.password_recovery_method = Some(integer(field, v)?),
                "passwordRecoveryURL" => a.password_recovery_url = Some(string(field, v)?),
                "allowSaveForReauthentication" => {
                    a.allow_save_for_reauthentication = Some(boolean(field, v)?)
                }
                "saveForReauthenticationByDefault" => {
                    a.save_for_reauthentication_by_default = Some(boolean(field, v)?)
                }
                "savedAuthenticationParametersLifeTime" => {
                    a.saved_authentication_lifetime = Some(integer(field, v)?)
                }
                "lcinfo" => a.lcinfo = Some(lcinfo(v, &mut a.unrecognized)?),
                "enablePasswordRecovery" => a.enable_password_recovery = Some(boolean(field, v)?),
                "inactivityTimeForTerminateSession" => {
                    a.inactivity_timeout = Some(integer(field, v)?)
                }
                "inactivityTimeForTerminateSessionNotification" => {
                    a.inactivity_notification = Some(integer(field, v)?)
                }
                _ => a.unrecognized.push(key.clone()),
            }
        }
        Ok(a)
    }
}

fn lcinfo(v: &Json, unrecognized: &mut Vec<String>) -> Result<LcInfo, AuthSettingsError> {
    let mut l = LcInfo::default();
    for (key, v) in members("lcinfo", v)? {
        match key.as_str() {
            "auth" => l.auth = Some(lcauth(v, unrecognized)?),
            "authTypes" => {
                let Json::Array(items) = v else {
                    return Err(wrong("lcinfo.authTypes", "an array", v));
                };
                let mut types = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    types.push(string(&format!("lcinfo.authTypes[{i}]"), item)?);
                }
                l.auth_types = Some(types);
            }
            _ => unrecognized.push(format!("lcinfo.{key}")),
        }
    }
    Ok(l)
}

fn lcauth(v: &Json, unrecognized: &mut Vec<String>) -> Result<LcAuth, AuthSettingsError> {
    let mut a = LcAuth::default();
    for (key, v) in members("lcinfo.auth", v)? {
        let field = format!("lcinfo.auth.{key}");
        match key.as_str() {
            "allowSaveForReauthentication" => {
                a.allow_save_for_reauthentication = Some(boolean(&field, v)?)
            }
            "savedAuthenticationParametersLifeTime" => {
                a.saved_authentication_lifetime = Some(integer(&field, v)?)
            }
            "saveForReauthenticationByDefault" => {
                a.save_for_reauthentication_by_default = Some(boolean(&field, v)?)
            }
            _ => unrecognized.push(field),
        }
    }
    Ok(a)
}

fn members<'v>(path: &str, v: &'v Json) -> Result<&'v [(String, Json)], AuthSettingsError> {
    let Json::Object(all) = v else {
        let at = if path.is_empty() { "the answer" } else { path };
        return Err(wrong(at, "an object", v));
    };
    let mut seen = std::collections::HashSet::with_capacity(all.len());
    for (key, _) in all {
        if !seen.insert(key.as_str()) {
            let field = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            return Err(AuthSettingsError::Duplicate { field });
        }
    }
    Ok(all)
}

fn wrong(field: &str, expected: &'static str, v: &Json) -> AuthSettingsError {
    AuthSettingsError::Type {
        field: field.to_string(),
        expected,
        found: v.kind(),
    }
}

fn boolean(field: &str, v: &Json) -> Result<bool, AuthSettingsError> {
    match v {
        Json::Bool(b) => Ok(*b),
        _ => Err(wrong(field, "a boolean", v)),
    }
}

fn string(field: &str, v: &Json) -> Result<String, AuthSettingsError> {
    match v {
        Json::String(s) => Ok(s.clone()),
        _ => Err(wrong(field, "a string", v)),
    }
}

fn integer(field: &str, v: &Json) -> Result<i64, AuthSettingsError> {
    let Json::Number(text) = v else {
        return Err(wrong(field, "a number", v));
    };
    text.parse().map_err(|_| AuthSettingsError::Integer {
        field: field.to_string(),
        value: text.clone(),
    })
}
