use std::io::Read;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQL support check I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid SQL support check output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported SQL system data: {0}")]
    Unsupported(&'static str),
    #[error("{}", .0.join("; "))]
    Found(Vec<String>),
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Flags {
    users: u8,
    legacy_users: u8,
    policies: u8,
    compromised: u8,
    auth_keys: u8,
    storage_content: u8,
    storage_version: u8,
    external_stores: u8,
    external_blobs: u8,
    config_cas: u8,
    config_cas_save: u8,
    schema_unstable: u8,
    version_unverified: u8,
    #[serde(default)]
    ib_version_rows: Option<i64>,
    #[serde(default)]
    ib_version: Option<i64>,
    #[serde(default)]
    platform_version_req: Option<i64>,
}

pub const VERIFIED_IB_VERSION: (i64, i64) = (7, 80313);

fn version_text(flags: &Flags) -> String {
    let Some(rows) = flags.ib_version_rows else {
        return "infobase version is not verified".to_owned();
    };
    let value = |v: Option<i64>| v.map_or_else(|| "NULL".to_owned(), |v| v.to_string());
    let found = match rows {
        0 => "таблица IBVersion базы пуста".to_owned(),
        1 => format!(
            "в таблице IBVersion базы IBVersion = {}, PlatformVersionReq = {}",
            value(flags.ib_version),
            value(flags.platform_version_req)
        ),
        rows => format!(
            "в таблице IBVersion базы строк: {rows} (первая: IBVersion = {}, PlatformVersionReq = {})",
            value(flags.ib_version),
            value(flags.platform_version_req)
        ),
    };
    format!(
        "{found}, а поддерживается только одна строка \
с IBVersion = {} и PlatformVersionReq = {}: так у всех проверенных копий, и все они записаны платформой 8.5.1. \
По названиям полей это версия формата хранения базы и наименьшая версия платформы, которая её откроет \
(80313 = 8.3.13); что значат другие значения, не установлено. Копии с платформы 8.3.x пока не проверены, \
с другими значениями IBVersion они не поддерживаются",
        VERIFIED_IB_VERSION.0, VERIFIED_IB_VERSION.1
    )
}

pub fn read_support(input: impl Read) -> Result<(), Error> {
    let flags = read_support_for_separated_users(input)?;
    if flags.users_present {
        return Err(Error::Unsupported("users"));
    }
    if flags.legacy_users_present {
        return Err(Error::Unsupported("legacy users.usr"));
    }
    Ok(())
}

pub struct UsersPresence {
    pub users_present: bool,
    pub legacy_users_present: bool,
}

pub fn read_support_findings(input: impl Read) -> Result<Result<UsersPresence, Error>, Error> {
    found_as_value(read_support_for_separated_users(input))
}

pub fn read_email_findings(input: impl Read) -> Result<Result<(), Error>, Error> {
    found_as_value(read_empty_user_email(input))
}

fn found_as_value<T>(result: Result<T, Error>) -> Result<Result<T, Error>, Error> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(found @ Error::Found(_)) => Ok(Err(found)),
        Err(other) => Err(other),
    }
}

pub fn read_empty_user_email(input: impl Read) -> Result<(), Error> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Email {
        present: u8,
    }
    let mut bytes = Vec::new();
    input.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(Error::Unsupported("email check output limit exceeded"));
    }
    bytes.retain(|&b| b != b'\r' && b != b'\n');
    let email: Email = serde_json::from_slice(&bytes)?;
    if email.present > 1 {
        return Err(Error::Unsupported("invalid email presence flag"));
    }
    if email.present != 0 {
        return Err(Error::Found(vec![
            "у пользователя информационной базы задан адрес электронной почты (V8USERS.Email); DT с адресами пользователей пока не поддерживается"
                .to_owned(),
        ]));
    }
    Ok(())
}

pub fn read_support_for_separated_users(input: impl Read) -> Result<UsersPresence, Error> {
    let mut bytes = Vec::new();
    input.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(Error::Unsupported("support output limit exceeded"));
    }
    bytes.retain(|&b| b != b'\r' && b != b'\n');
    let flags: Flags = serde_json::from_slice(&bytes)?;
    if flags.users > 1 || flags.legacy_users > 1 {
        return Err(Error::Unsupported("invalid user presence flags"));
    }
    let mut found = Vec::new();
    for (text, flag) in [
        (
            "в базе заданы политики паролей (таблица V8USERPWDPLCS не пуста)",
            flags.policies,
        ),
        (
            "в базе есть список скомпрометированных паролей (таблица V8CMSDPWDS не пуста)",
            flags.compromised,
        ),
        (
            "в базе есть ключи сопоставления пользователей (таблица V8USERSMATKEYS не пуста)",
            flags.auth_keys,
        ),
        (
            "в базе есть данные хранилища двоичных данных (таблица BinaryDataStorageContent не пуста)",
            flags.storage_content,
        ),
        (
            "в базе есть версии хранилища двоичных данных (таблица BinaryDataStorageVersion не пуста)",
            flags.storage_version,
        ),
        (
            "в базе описаны внешние хранилища двоичных данных (таблица ExternalBinDataStrgsList не пуста)",
            flags.external_stores,
        ),
        (
            "в базе есть объекты внешних хранилищ двоичных данных (таблица ExternalBinDataStrgsBList не пуста)",
            flags.external_blobs,
        ),
    ] {
        if flag != 0 {
            found.push(text.to_owned());
        }
    }
    match (flags.config_cas != 0, flags.config_cas_save != 0) {
        (false, false) => {}
        (cas, save) => found.push(format!(
            "в базе есть расширения конфигурации (не пусты таблицы: {})",
            match (cas, save) {
                (true, true) => "ConfigCAS, ConfigCASSave",
                (true, false) => "ConfigCAS",
                _ => "ConfigCASSave",
            }
        )),
    }
    if flags.schema_unstable != 0 {
        found.push(
            "служебная схема базы не в том состоянии, что у проверенных копий (там в SchemaStorage одна строка \
с SchemaID 0 и Status 100, её схема равна единственной строке DBSchema, списки нового поколения пусты)"
                .to_owned(),
        );
    }
    if flags.version_unverified != 0 {
        found.push(version_text(&flags));
    }
    if let Some(last) = found.last_mut() {
        last.push_str("; DT для такой базы пока не поддерживается");
        return Err(Error::Found(found));
    }
    Ok(UsersPresence {
        users_present: flags.users != 0,
        legacy_users_present: flags.legacy_users != 0,
    })
}
