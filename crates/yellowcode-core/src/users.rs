use mdconf::{Configuration, ConfigurationExt, LoadOptions, LoadProfile, MdKind};
use onecd::{BlobVal, Database, OpenOptions, Value};
use std::{collections::BTreeMap, io::Write, path::Path};
use unicode_normalization::UnicodeNormalization;
pub use v8users::card::{Authentication, Card, Role, Separation};

const MAX_USERS: usize = 10_000;
const MAX_TOTAL_DATA: usize = 64 * 1_048_576;
const MAX_OUTPUT_BYTES: usize = 32 * 1_048_576;
const MAX_OUTPUT_ITEMS: usize = 100_000;

struct OutputBudget {
    remaining: usize,
    items: usize,
}

impl OutputBudget {
    fn new() -> Self {
        Self {
            remaining: MAX_OUTPUT_BYTES - 2,
            items: 0,
        }
    }

    fn admit(&mut self, card: &Card) -> Result<(), Error> {
        self.items = self
            .items
            .checked_add(card.roles.len())
            .and_then(|n| n.checked_add(card.data_separation.len()))
            .filter(|n| *n <= MAX_OUTPUT_ITEMS)
            .ok_or(Error::Limit)?;
        serde_json::to_writer(&mut *self, card).map_err(|_| Error::Limit)?;
        self.write_all(b",").map_err(|_| Error::Limit)
    }
}

impl Write for OutputBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("users output limit"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no such infobase user")]
    NotFound,
    #[error("several users have this name; select the UUID")]
    Ambiguous,
    #[error("infobase user data exceeds the reading limit")]
    Limit,
    #[error("V8USERS DATA row {row}: {source}")]
    Data { row: usize, source: v8users::Error },
    #[error("V8USERS DATA column is missing or malformed")]
    Column,
    #[error(transparent)]
    Storage(#[from] onecd::Error),
    #[error("reading users configuration: {0}")]
    Configuration(String),
}

fn name_key(name: &str) -> Result<Vec<u8>, Error> {
    use onecd::collate::{Collate, IcuCollate, Strength};
    let text = name.trim_end_matches(' ').nfc().collect::<String>();
    Ok(IcuCollate.sort_key(
        "root",
        Strength::Secondary,
        &text.encode_utf16().collect::<Vec<_>>(),
    )?)
}

pub fn valid_id(id: &str) -> bool {
    brace::Guid::parse(id).is_some()
}

pub fn find<'a>(cards: &'a [Card], identity: &str) -> Result<&'a Card, Error> {
    if let Some(id) = brace::Guid::parse(identity) {
        return cards
            .iter()
            .find(|c| c.id == id.to_string())
            .ok_or(Error::NotFound);
    }
    let key = name_key(identity)?;
    let mut found = None;
    for card in cards {
        if name_key(&card.name)? == key {
            if found.is_some() {
                return Err(Error::Ambiguous);
            }
            found = Some(card);
        }
    }
    found.ok_or(Error::NotFound)
}

pub fn list(base: &Path) -> Result<Vec<Card>, Error> {
    let db = Database::open(
        &crate::viewer::source::resolve(base),
        OpenOptions::read_only(),
    )?;
    let txn = db.read();
    let mut cards = read_cards(&txn)?;
    if cards.iter().any(|c| !c.roles.is_empty()) {
        let src = infobase::config::ConfigTxnSource::read(&txn, "CONFIG")
            .map_err(|e| Error::Configuration(e.to_string()))?;
        if !src.is_empty() {
            let cfg = Configuration::load(
                &src,
                &LoadOptions {
                    keep_source: false,
                    profile: LoadProfile::Query,
                    ..Default::default()
                },
            )
            .map_err(|e| Error::Configuration(e.to_string()))?;
            resolve_roles(&mut cards, &cfg)?;
        }
    }
    Ok(cards)
}

pub fn list_with_configuration(base: &Path, cfg: &Configuration) -> Result<Vec<Card>, Error> {
    let db = Database::open(
        &crate::viewer::source::resolve(base),
        OpenOptions::read_only(),
    )?;
    let mut cards = read_cards(&db.read())?;
    resolve_roles(&mut cards, cfg)?;
    Ok(cards)
}

pub fn resolve_roles(cards: &mut [Card], cfg: &Configuration) -> Result<(), Error> {
    let roles: BTreeMap<_, _> = cfg
        .objects
        .iter()
        .filter(|o| o.kind == MdKind::Role)
        .map(|o| (o.id.to_string(), o.name.as_str()))
        .collect();
    let mut names = OutputBudget::new();
    for role in cards.iter().flat_map(|card| &card.roles) {
        if let Some(name) = roles.get(&role.id) {
            serde_json::to_writer(&mut names, name).map_err(|_| Error::Limit)?;
        }
    }
    for card in cards.iter_mut() {
        for role in &mut card.roles {
            role.name = roles.get(&role.id).map(|n| (*n).to_owned());
        }
    }
    let mut output = OutputBudget::new();
    for card in cards.iter() {
        output.admit(card)?;
    }
    Ok(())
}

fn read_cards(txn: &onecd::ReadTxn) -> Result<Vec<Card>, Error> {
    let table = txn.table("V8USERS")?;
    let column = table
        .descr()
        .field("DATA")
        .map(|(i, _)| i)
        .ok_or(Error::Column)?;
    let mut cards = Vec::new();
    let mut total = 0usize;
    let mut output = OutputBudget::new();
    for (row_index, row) in table.scan().enumerate() {
        if row_index >= MAX_USERS {
            return Err(Error::Limit);
        }
        let (_, row) = row?;
        let value = row.get(column).ok_or(Error::Column)?;
        let len = match value {
            Value::Image(BlobVal::Ref(r)) => r.len as usize,
            Value::Image(BlobVal::Data(b)) | Value::Binary(b) => b.len(),
            _ => return Err(Error::Column),
        };
        total = total
            .checked_add(len)
            .filter(|n| *n <= MAX_TOTAL_DATA)
            .ok_or(Error::Limit)?;
        if len > v8users::MAX_DATA_BYTES {
            return Err(Error::Limit);
        }
        let blob = match value {
            Value::Image(BlobVal::Ref(r)) => table.blob_bytes(*r)?,
            Value::Image(BlobVal::Data(b)) | Value::Binary(b) => b.clone(),
            _ => return Err(Error::Column),
        };
        let card = Card::from_blob(&blob).map_err(|source| Error::Data {
            row: row_index,
            source,
        })?;
        if !card.is_service() {
            output.admit(&card)?;
            cards.push(card);
        }
    }
    Ok(cards)
}

pub fn write_json(out: &mut dyn Write, value: &impl serde::Serialize) -> crate::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

fn cell(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

pub fn write_list(out: &mut dyn Write, cards: &[Card]) -> crate::Result<()> {
    writeln!(out, "Name\tFull name\tRoles\tUUID")?;
    for c in cards {
        let roles = c
            .roles
            .iter()
            .map(|r| cell(r.name.as_deref().unwrap_or(&r.id)))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            cell(&c.name),
            cell(&c.full_name),
            roles,
            c.id
        )?;
    }
    Ok(())
}

pub fn write_card(out: &mut dyn Write, card: &Card) -> crate::Result<()> {
    let value = serde_json::to_value(card)?;
    if let Some(fields) = value.as_object() {
        for (name, value) in fields {
            let text = value
                .as_str()
                .map(cell)
                .unwrap_or_else(|| value.to_string());
            writeln!(out, "{name}: {text}")?;
        }
    }
    Ok(())
}

mod write;
pub use write::*;

mod metadata;
mod mutation;
mod snapshot;

#[cfg(feature = "testing")]
fn qualification_pin(path: &Path, maximum: u64) -> Result<(Vec<u8>, String), UserWriteError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    for parent in path.ancestors() {
        let metadata =
            std::fs::symlink_metadata(parent).map_err(|_| UserWriteError::UnsupportedSource)?;
        if metadata.file_type().is_symlink() {
            return Err(UserWriteError::UnsupportedSource);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(UserWriteError::UnsupportedSource);
            }
        }
    }
    let mut file = std::fs::File::open(path).map_err(|_| UserWriteError::UnsupportedSource)?;
    let metadata = file
        .metadata()
        .map_err(|_| UserWriteError::UnsupportedSource)?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(UserWriteError::UnsupportedSource);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(UserWriteError::UnsupportedSource);
        }
    }
    #[cfg(windows)]
    {
        if !qualification_single_link(&file)? {
            return Err(UserWriteError::UnsupportedSource);
        }
    }
    let mut digest = Sha256::new();
    let mut retained = Vec::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| UserWriteError::UnsupportedSource)?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .filter(|n| *n <= maximum)
            .ok_or(UserWriteError::UnsupportedSource)?;
        let block = buffer.get(..n).ok_or(UserWriteError::UnsupportedSource)?;
        digest.update(block);
        if maximum <= 1_048_576 {
            retained.extend_from_slice(block);
        }
    }
    if count != metadata.len() {
        return Err(UserWriteError::UnsupportedSource);
    }
    Ok((retained, format!("{:x}", digest.finalize())))
}

#[cfg(all(feature = "testing", windows))]
#[allow(unsafe_code)]
fn qualification_single_link(file: &std::fs::File) -> Result<bool, UserWriteError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(UserWriteError::UnsupportedSource);
    }
    Ok(unsafe { info.assume_init() }.nNumberOfLinks == 1)
}

#[cfg(feature = "testing")]
#[doc(hidden)]
pub fn qualify_sealed_user_clear(
    root: &Path,
    request_sha256: &str,
    base_sha256: &str,
) -> Result<UserWriteOutcome, UserWriteError> {
    qualify_fixture_clear(root, request_sha256, base_sha256, false)
}

#[cfg(feature = "testing")]
#[doc(hidden)]
pub fn qualify_sealed_mirror_clear(
    root: &Path,
    request_sha256: &str,
    base_sha256: &str,
) -> Result<UserWriteOutcome, UserWriteError> {
    qualify_fixture_clear(root, request_sha256, base_sha256, true)
}

#[cfg(feature = "testing")]
fn qualify_fixture_clear(
    root: &Path,
    request_sha256: &str,
    base_sha256: &str,
    mirrored_admin: bool,
) -> Result<UserWriteOutcome, UserWriteError> {
    let refused = || UserWriteError::UnsupportedSource;
    let compiled_head = option_env!("DU2A_QUALIFICATION_HEAD").ok_or_else(refused)?;
    if !cfg!(windows)
        || compiled_head.len() != 40
        || !root.is_absolute()
        || root.parent() != Some(Path::new(r"C:\yc"))
        || !root
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("du2a-native-"))
        || request_sha256.len() != 64
        || base_sha256.len() != 64
    {
        return Err(refused());
    }
    let base = qualification_candidate_slot(
        root,
        compiled_head,
        request_sha256,
        base_sha256,
        mirrored_admin,
    )?;
    let snapshot = write_snapshot(&base)?;
    let mut names = snapshot
        .users
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    let expected = if mirrored_admin {
        vec!["Du2aAdmin", "Du2aEdited", "Du2aUnrelated"]
    } else {
        vec!["Du2aAdmin", "Du2aCreated", "Du2aEdited", "Du2aUnrelated"]
    };
    if names != expected {
        return Err(refused());
    }
    let target = snapshot
        .users
        .iter()
        .find(|c| {
            c.name
                == if mirrored_admin {
                    "Du2aAdmin"
                } else {
                    "Du2aEdited"
                }
        })
        .ok_or_else(refused)?;
    if !target.password_set || !target.data_separation.is_empty() {
        return Err(refused());
    }
    let mutation = UserWriteRequest {
        revision: snapshot.revision,
        confirmed: true,
        allow_last_user: false,
        action: UserAction::ClearPassword {
            identity: target.id.clone(),
        },
    };
    mutation::apply_fixture_clear(&base, &mutation)
}

#[cfg(feature = "testing")]
fn qualification_candidate_slot(
    root: &Path,
    compiled_head: &str,
    request_sha256: &str,
    base_sha256: &str,
    mirrored_admin: bool,
) -> Result<std::path::PathBuf, UserWriteError> {
    use serde_json::Value as Json;
    let refused = || UserWriteError::UnsupportedSource;
    let (raw, pin) = qualification_pin(&root.join("request.json"), 1_048_576)?;
    if pin != request_sha256 {
        return Err(refused());
    }
    let request: Json = serde_json::from_slice(&raw).map_err(|_| refused())?;
    if request.get("owner").and_then(Json::as_str) != Some("DU2a users minimal")
        || request.get("head").and_then(Json::as_str) != Some(compiled_head)
        || request.get("root").and_then(Json::as_str).map(Path::new) != Some(root)
        || request
            .get("build")
            .and_then(|v| v.get("head"))
            .and_then(Json::as_str)
            != Some(compiled_head)
    {
        return Err(refused());
    }
    let (raw, pin) = qualification_pin(&root.join("inputs").join("input-pins.json"), 1_048_576)?;
    if request
        .get("private_input_commitment")
        .and_then(|v| v.get("sha256"))
        .and_then(Json::as_str)
        != Some(pin.as_str())
        || request
            .get("private_input_commitment")
            .and_then(|v| v.get("bytes"))
            .and_then(Json::as_u64)
            != Some(raw.len() as u64)
    {
        return Err(refused());
    }
    let pins: std::collections::BTreeMap<String, Json> =
        serde_json::from_slice(&raw).map_err(|_| refused())?;
    if pins.is_empty()
        || pins.len() > 256
        || !pins.contains_key("packet.json")
        || !pins.contains_key("fixture-private.json")
    {
        return Err(refused());
    }
    for (name, expected) in pins {
        let relative = Path::new(&name);
        if relative.is_absolute()
            || name.contains('\\')
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(refused());
        }
        let (raw, hash) = qualification_pin(&root.join("inputs").join(relative), 1_048_576)?;
        if expected.get("bytes").and_then(Json::as_u64) != Some(raw.len() as u64)
            || expected.get("sha256").and_then(Json::as_str) != Some(hash.as_str())
        {
            return Err(refused());
        }
    }
    let (raw, _) = qualification_pin(&root.join("inputs").join("packet.json"), 1_048_576)?;
    let plan: Json = serde_json::from_slice(&raw).map_err(|_| refused())?;
    if plan.get("head") != request.get("head")
        || plan.get("root") != request.get("root")
        || plan.get("turn") != request.get("turn")
        || plan
            .get("candidate_clear_qualification")
            .and_then(Json::as_str)
            != Some("sealed-candidate-only-v1")
    {
        return Err(refused());
    }
    let cases = plan
        .get("cases")
        .and_then(Json::as_array)
        .ok_or_else(refused)?;
    let case_id = if mirrored_admin {
        "candidate-mirror-empty"
    } else {
        "candidate-permanent-empty"
    };
    let operation = if mirrored_admin {
        "candidate:clear-mirror"
    } else {
        "candidate:clear"
    };
    if cases
        .iter()
        .filter(|case| case.get("id").and_then(Json::as_str) == Some(case_id))
        .count()
        != 1
        || !cases.iter().any(|case| {
            case.get("id").and_then(Json::as_str) == Some(case_id)
                && case.get("lane").and_then(Json::as_str) == Some("candidate")
                && case.get("operation").and_then(Json::as_str) == Some(operation)
                && case.get("state").and_then(Json::as_str) == Some("READY")
        })
    {
        return Err(refused());
    }
    let base = root.join("candidate").join("1Cv8.1CD");
    let (_, pin) = qualification_pin(&base, 256 * 1_048_576)?;
    if pin != base_sha256 {
        return Err(refused());
    }
    Ok(base)
}

#[cfg(all(test, feature = "testing"))]
fn qualification_candidate(
    root: &Path,
    compiled_head: &str,
    request_sha256: &str,
    base_sha256: &str,
) -> Result<std::path::PathBuf, UserWriteError> {
    qualification_candidate_slot(root, compiled_head, request_sha256, base_sha256, false)
}

pub(crate) fn source_identity(
    base: &Path,
) -> Result<(std::path::PathBuf, (u64, u64)), UserWriteError> {
    let path = snapshot::path(base)?;
    let file = std::fs::File::open(&path).map_err(|_| UserWriteError::UnsupportedSource)?;
    Ok((path, snapshot::identity(&file)?))
}

#[cfg(all(test, feature = "testing"))]
mod qualification_tests {
    use super::*;

    #[test]
    fn qualification_refuses_a_foreign_base_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("1Cv8.1CD");
        std::fs::write(&base, b"foreign source").unwrap();
        assert!(matches!(
            qualify_sealed_user_clear(temp.path(), &"a".repeat(64), &"b".repeat(64)),
            Err(UserWriteError::UnsupportedSource)
        ));
        assert_eq!(std::fs::read(&base).unwrap(), b"foreign source");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn qualification_pin_is_bounded_and_refuses_shared_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let file = root.join("pin.json");
        std::fs::write(&file, b"{}").unwrap();
        assert!(qualification_pin(&file, 1).is_err());
        let (raw, hash) = qualification_pin(&file, 2).unwrap();
        assert_eq!(raw, b"{}");
        assert_eq!(
            hash,
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        std::fs::hard_link(&file, root.join("alias.json")).unwrap();
        assert!(qualification_pin(&file, 2).is_err());
    }

    #[test]
    fn qualification_requires_exact_sealed_candidate_inputs_and_database() {
        use serde_json::json;
        let temp = tempfile::tempdir().unwrap();
        let physical = std::fs::canonicalize(temp.path()).unwrap();
        let root = physical.as_path();
        let inputs = root.join("inputs");
        std::fs::create_dir(&inputs).unwrap();
        std::fs::create_dir(root.join("candidate")).unwrap();
        let base = root.join("candidate").join("1Cv8.1CD");
        std::fs::write(&base, b"synthetic candidate bytes").unwrap();
        let head = "a".repeat(40);
        let mut plan = json!({"head":head,"root":root,"turn":"du2a-native-test",
            "candidate_clear_qualification":"sealed-candidate-only-v1",
            "cases":[{"id":"candidate-permanent-empty","lane":"candidate",
                "operation":"candidate:clear","state":"READY"}]});
        let reseal = |plan: &serde_json::Value| {
            std::fs::write(
                inputs.join("packet.json"),
                serde_json::to_vec(plan).unwrap(),
            )
            .unwrap();
            std::fs::write(inputs.join("fixture-private.json"), b"{\"synthetic\":true}").unwrap();
            let mut pins = std::collections::BTreeMap::new();
            for name in ["packet.json", "fixture-private.json"] {
                let (raw, hash) = qualification_pin(&inputs.join(name), 1_048_576).unwrap();
                pins.insert(name, json!({"bytes":raw.len(),"sha256":hash}));
            }
            std::fs::write(
                inputs.join("input-pins.json"),
                serde_json::to_vec(&pins).unwrap(),
            )
            .unwrap();
            let (raw, hash) =
                qualification_pin(&inputs.join("input-pins.json"), 1_048_576).unwrap();
            let request = json!({"head":head,"build":{"head":head},"owner":"DU2a users minimal",
                "root":root,"turn":"du2a-native-test","private_input_commitment":{"bytes":raw.len(),"sha256":hash}});
            std::fs::write(
                root.join("request.json"),
                serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
            qualification_pin(&root.join("request.json"), 1_048_576)
                .unwrap()
                .1
        };
        let request = reseal(&plan);
        let database = qualification_pin(&base, 256 * 1_048_576).unwrap().1;
        assert_eq!(
            qualification_candidate(root, &head, &request, &database).unwrap(),
            base
        );
        assert!(qualification_candidate_slot(root, &head, &request, &database, true).is_err());
        let mut mirror_plan = plan.clone();
        mirror_plan["cases"][0]["id"] = json!("candidate-mirror-empty");
        mirror_plan["cases"][0]["operation"] = json!("candidate:clear-mirror");
        let mirror_request = reseal(&mirror_plan);
        assert_eq!(
            qualification_candidate_slot(root, &head, &mirror_request, &database, true).unwrap(),
            base
        );
        assert!(qualification_candidate(root, &head, &mirror_request, &database).is_err());
        let request = reseal(&plan);
        assert!(qualification_candidate(root, &"b".repeat(40), &request, &database).is_err());
        assert!(qualification_candidate(root, &head, &"b".repeat(64), &database).is_err());
        assert!(qualification_candidate(root, &head, &request, &"b".repeat(64)).is_err());
        std::fs::write(inputs.join("fixture-private.json"), b"changed input").unwrap();
        assert!(qualification_candidate(root, &head, &request, &database).is_err());
        let request = reseal(&plan);
        std::fs::write(&base, b"foreign database").unwrap();
        assert!(qualification_candidate(root, &head, &request, &database).is_err());
        plan["cases"][0]["lane"] = json!("native");
        let request = reseal(&plan);
        let foreign = qualification_pin(&base, 256 * 1_048_576).unwrap().1;
        assert!(qualification_candidate(root, &head, &request, &foreign).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn qualification_pin_refuses_symlink_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("pin.json"), b"{}").unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert!(qualification_pin(&alias.join("pin.json"), 2).is_err());
    }
}
