use appobj::SessionUser;
use icu46_sys::{Collator, Strength};
use onecd::{BlobVal, Database, FieldType, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use v8users::user::UserData;

const MAX_USERS: usize = 10_000;
const MAX_TOTAL_DATA: usize = 64 * 1_048_576;
const MAX_CREDENTIAL_BYTES: usize = 1_048_576;

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("cannot read the V8USERS table")]
    Table,
    #[error("V8USERS exceeds the authentication directory limit")]
    Limit,
    #[error("V8USERS row {row}: invalid user data")]
    Data { row: usize },
    #[error("V8USERS row {row}: inconsistent user identity")]
    Identity { row: usize },
    #[error("V8USERS contains duplicate identities")]
    Duplicate,
    #[error("V8USERS row {row}: unsupported password algorithm")]
    PasswordAlgorithm { row: usize },
    #[error("cannot initialize user name collation")]
    Collation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid credentials")]
pub struct LoginError;

#[derive(Clone, Copy)]
pub struct Credentials<'a> {
    name: &'a str,
    password: &'a str,
}

impl<'a> Credentials<'a> {
    pub fn new(name: &'a str, password: &'a str) -> Self {
        Self { name, password }
    }
}

impl std::fmt::Debug for Credentials<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(<redacted>)")
    }
}

struct Entry {
    user: SessionUser,
    standard: bool,
    upper_hash: String,
}

pub struct UserDirectory {
    users: HashMap<Vec<u8>, Entry>,
    collator: Mutex<Collator>,
}

impl std::fmt::Debug for UserDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserDirectory")
            .field("user_count", &self.users.len())
            .finish_non_exhaustive()
    }
}

fn name_key(collator: &Collator, name: &str) -> Result<Vec<u8>, DirectoryError> {
    let key = collator.sort_key(
        &name
            .trim_end_matches(' ')
            .encode_utf16()
            .collect::<Vec<_>>(),
    );
    if key.is_empty() {
        return Err(DirectoryError::Collation);
    }
    Ok(key)
}

impl UserDirectory {
    pub fn load(db: &Database) -> Result<Self, DirectoryError> {
        let collator = Collator::new(&db.info().locale, Strength::Secondary)
            .map_err(|_| DirectoryError::Collation)?;
        let read = db.read();
        let table = read.table("V8USERS").map_err(|_| DirectoryError::Table)?;
        let column = |name| table.descr().field(name).ok_or(DirectoryError::Table);
        let ((id_col, id), (name_col, name), (data_col, data)) =
            (column("ID")?, column("NAME")?, column("DATA")?);
        if id.ty != FieldType::B
            || id.length != 16
            || !name.ty.is_string()
            || !matches!(data.ty, FieldType::I | FieldType::B | FieldType::VB)
        {
            return Err(DirectoryError::Table);
        }
        let mut users = HashMap::new();
        let mut identities = HashSet::new();
        let mut total = 0usize;
        for (row, item) in table.scan().enumerate() {
            if row >= MAX_USERS {
                return Err(DirectoryError::Limit);
            }
            let (_, values) = item.map_err(|_| DirectoryError::Table)?;
            let bad = || DirectoryError::Data { row };
            let inconsistent = || DirectoryError::Identity { row };
            let physical_id = match values.get(id_col) {
                Some(Value::Binary(bytes)) => ibschema::values::RefId(
                    bytes.as_slice().try_into().map_err(|_| inconsistent())?,
                )
                .to_uuid(),
                _ => return Err(inconsistent()),
            };
            let physical_name = match values.get(name_col) {
                Some(Value::Char(name) | Value::VarChar(name)) => name,
                _ => return Err(inconsistent()),
            };
            let value = values.get(data_col).ok_or_else(bad)?;
            let len = match value {
                Value::Image(BlobVal::Ref(r)) => r.len as usize,
                Value::Image(BlobVal::Data(bytes)) | Value::Binary(bytes) => bytes.len(),
                _ => return Err(bad()),
            };
            total = total
                .checked_add(len)
                .filter(|n| *n <= MAX_TOTAL_DATA)
                .ok_or(DirectoryError::Limit)?;
            if len > v8users::MAX_DATA_BYTES {
                return Err(DirectoryError::Limit);
            }
            let blob = match value {
                Value::Image(BlobVal::Ref(r)) => table.blob_bytes(*r).map_err(|_| bad())?,
                Value::Image(BlobVal::Data(bytes)) | Value::Binary(bytes) => bytes.clone(),
                _ => return Err(bad()),
            };
            let text = v8users::data::decode(&blob).map_err(|_| bad())?;
            let data = UserData::parse(&text.text).map_err(|_| bad())?;
            let card = data.card().map_err(|_| bad())?;
            if data.id() != Some(physical_id) || data.name() != Some(physical_name.as_str()) {
                return Err(inconsistent());
            }
            if !identities.insert(physical_id) {
                return Err(DirectoryError::Duplicate);
            }
            if card.password_hash_algorithm != 0 {
                return Err(DirectoryError::PasswordAlgorithm { row });
            }
            let hashes = data.hashes();
            if card.is_service() {
                if !physical_name.is_empty()
                    || !card.full_name.is_empty()
                    || !card.roles.is_empty()
                    || data.has_password_material().map_err(|_| bad())?
                {
                    return Err(inconsistent());
                }
                continue;
            }
            if physical_name.is_empty() || physical_id.is_nil() {
                return Err(inconsistent());
            }
            let key = name_key(&collator, physical_name)?;
            let entry = Entry {
                user: SessionUser {
                    uuid: physical_id,
                    name: physical_name.clone(),
                    full_name: card.full_name,
                    roles: data.roles(),
                    no_users: false,
                },
                standard: data.std_auth(),
                upper_hash: hashes.upper,
            };
            if users.insert(key, entry).is_some() {
                return Err(DirectoryError::Duplicate);
            }
        }
        Ok(Self {
            users,
            collator: Mutex::new(collator),
        })
    }

    pub fn authenticate(
        &self,
        credentials: Option<Credentials<'_>>,
    ) -> Result<SessionUser, LoginError> {
        let Some(credentials) = credentials else {
            return if self.users.is_empty() {
                Ok(SessionUser {
                    uuid: evlog::UserRef::nobody().uuid,
                    name: String::new(),
                    full_name: String::new(),
                    roles: Vec::new(),
                    no_users: true,
                })
            } else {
                Err(LoginError)
            };
        };
        if credentials.name.is_empty()
            || credentials
                .name
                .len()
                .saturating_add(credentials.password.len())
                > MAX_CREDENTIAL_BYTES
        {
            return Err(LoginError);
        }
        let key = name_key(
            &self.collator.lock().unwrap_or_else(|p| p.into_inner()),
            credentials.name,
        )
        .map_err(|_| LoginError)?;
        let user = self.users.get(&key).ok_or(LoginError)?;
        if user.standard && v8users::password::login_matches(&user.upper_hash, credentials.password)
        {
            Ok(user.user.clone())
        } else {
            Err(LoginError)
        }
    }
}
