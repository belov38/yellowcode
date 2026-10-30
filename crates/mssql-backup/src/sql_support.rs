use std::io::Read;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQL support check I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid SQL support check output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported SQL system data: {0}")]
    Unsupported(&'static str),
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
    if email.present != 0 {
        return Err(Error::Unsupported("non-NULL user email"));
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
    for (name, flag) in [
        ("password policies", flags.policies),
        ("compromised passwords", flags.compromised),
        ("authentication keys", flags.auth_keys),
        ("binary storage content", flags.storage_content),
        ("binary storage version", flags.storage_version),
        ("external storage definitions", flags.external_stores),
        ("external storage objects", flags.external_blobs),
        ("ConfigCAS", flags.config_cas),
        ("ConfigCASSave", flags.config_cas_save),
        ("schema storage state", flags.schema_unstable),
        ("infobase version", flags.version_unverified),
    ] {
        if flag != 0 {
            return Err(Error::Unsupported(name));
        }
    }
    Ok(UsersPresence {
        users_present: flags.users != 0,
        legacy_users_present: flags.legacy_users != 0,
    })
}
