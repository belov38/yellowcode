use super::{metadata::Facts, snapshot, *};
use onecd::{Numeric, RowId, TableDescr, WriteTxn};
#[cfg(any(not(unix), test))]
use std::sync::Arc;
use v8users::{
    BslDate, Uuid16,
    edit::PublicPatch,
    user::{NewUser, SERVICE_ROW_ID, UserData, UserRow},
};
type Result<T> = std::result::Result<T, UserWriteError>;
pub(super) fn storage(error: onecd::Error) -> UserWriteError {
    match error {
        onecd::Error::Locked { .. } => UserWriteError::SourceBusy,
        _ => UserWriteError::StorageBeforeCommit,
    }
}
fn layout(_: v8users::Error) -> UserWriteError {
    UserWriteError::UnsupportedLayout("user DATA projection")
}
struct Stored {
    slot: RowId,
    data: UserData,
    card: Card,
    changed: BslDate,
    size: usize,
}
struct Directory {
    rows: Vec<Stored>,
    service: Option<Stored>,
}
impl Directory {
    fn read(tx: &onecd::ReadTxn) -> Result<Self> {
        for name in ["V8USERS", "V8USERSMATKEYS"] {
            let table = tx.table(name).map_err(storage)?;
            let expected = infobase::seed::seed()
                .tables
                .iter()
                .find(|t| t.name == name)
                .ok_or(UserWriteError::UnsupportedLayout("user schema unavailable"))?;
            let actual = table.descr();
            if actual.fields != expected.fields
                || actual.indexes != expected.indexes
                || actual.recordlock != expected.recordlock
            {
                return Err(UserWriteError::UnsupportedLayout(
                    "unqualified user table schema",
                ));
            }
        }
        let table = tx.table("V8USERS").map_err(storage)?;
        let col = table
            .descr()
            .field("DATA")
            .ok_or(UserWriteError::UnsupportedLayout("DATA column"))?
            .0;
        let changed = table
            .descr()
            .field("CHANGED")
            .ok_or(UserWriteError::UnsupportedLayout("CHANGED column"))?
            .0;
        let mut rows = Vec::new();
        let mut service = None;
        let mut total = 0usize;
        let mut output = OutputBudget::new();
        for (i, row) in table.scan().enumerate() {
            if i >= MAX_USERS {
                return Err(UserWriteError::UnsupportedLayout("users limit"));
            }
            let (slot, row) = row.map_err(storage)?;
            let value = row
                .get(col)
                .ok_or(UserWriteError::UnsupportedLayout("DATA column"))?;
            let len = match value {
                Value::Image(BlobVal::Ref(r)) => r.len as usize,
                Value::Image(BlobVal::Data(b)) | Value::Binary(b) => b.len(),
                _ => return Err(UserWriteError::UnsupportedLayout("DATA type")),
            };
            total = total
                .checked_add(len)
                .filter(|n| *n <= MAX_TOTAL_DATA)
                .ok_or(UserWriteError::UnsupportedLayout("DATA budget"))?;
            if len > v8users::MAX_DATA_BYTES {
                return Err(UserWriteError::UnsupportedLayout("DATA limit"));
            }
            let bytes = match value {
                Value::Image(BlobVal::Ref(r)) => table.blob_bytes(*r).map_err(storage)?,
                Value::Image(BlobVal::Data(b)) | Value::Binary(b) => b.clone(),
                _ => return Err(UserWriteError::UnsupportedLayout("DATA type")),
            };
            let data = UserData::parse(&v8users::data::decode(&bytes).map_err(layout)?.text)
                .map_err(layout)?;
            let card = data.card().map_err(layout)?;
            let id_column = table
                .descr()
                .field("ID")
                .ok_or(UserWriteError::UnsupportedLayout("ID column"))?
                .0;
            let id = data
                .id()
                .ok_or(UserWriteError::UnsupportedLayout("DATA UUID"))?;
            let expected_id = ibschema::values::RefId::from_uuid(id).0;
            if !matches!(row.get(id_column),Some(Value::Binary(b)) if b.as_slice()==expected_id) {
                return Err(UserWriteError::UnsupportedLayout(
                    "physical and DATA UUID differ",
                ));
            }

            output
                .admit(&card)
                .map_err(|_| UserWriteError::UnsupportedLayout("public output limit"))?;
            let Some(Value::DateTime(date)) = row.get(changed) else {
                return Err(UserWriteError::UnsupportedLayout("CHANGED type"));
            };
            let changed = BslDate::from_ticks(ibschema::values::datetime_to_ticks(date) as i64)
                .ok_or(UserWriteError::UnsupportedLayout("CHANGED range"))?;
            let stored = Stored {
                slot,
                data,
                card,
                changed,
                size: len,
            };
            if stored.card.is_service() {
                if service.replace(stored).is_some() {
                    return Err(UserWriteError::UnsupportedLayout("duplicate service row"));
                }
            } else {
                if rows.iter().any(|r: &Stored| r.card.id == stored.card.id) {
                    return Err(UserWriteError::UnsupportedLayout("duplicate user UUID"));
                }
                rows.push(stored);
            }
        }
        Ok(Self { rows, service })
    }
}
pub(super) fn admit_source(tx: &onecd::ReadTxn) -> Result<()> {
    let directory = Directory::read(tx)?;
    legacy(tx, &directory).map(|_| ())
}

fn legacy_refusal() -> UserWriteError {
    UserWriteError::UnsupportedLayout("legacy users directory or partial migration")
}

struct Mirror {
    id: Uuid16,
    slot: RowId,
    row: onecd::Row,
    bytes: Vec<u8>,
}
fn legacy(tx: &onecd::ReadTxn, directory: &Directory) -> Result<Option<Mirror>> {
    let params = tx.table("PARAMS").map_err(storage)?;
    let column = |name| {
        params
            .descr()
            .field(name)
            .map(|(i, _)| i)
            .ok_or_else(legacy_refusal)
    };
    let filename = column("FILENAME")?;
    let part = column("PARTNO")?;
    let size = column("DATASIZE")?;
    let data = column("BINARYDATA")?;
    let mut mirror = None;
    for (i, row) in params.scan().enumerate() {
        if i >= 100_000 {
            return Err(legacy_refusal());
        }
        let (slot, mut row) = row.map_err(storage)?;
        let Some(Value::VarChar(name) | Value::Char(name)) = row.get(filename) else {
            return Err(legacy_refusal());
        };
        if name.len() > 4096 {
            return Err(legacy_refusal());
        }
        if !name.trim_end().eq_ignore_ascii_case("users.usr") {
            continue;
        }
        if mirror.is_some() || row.get(part) != Some(&Value::Numeric(Numeric::from_i64(0))) {
            return Err(legacy_refusal());
        }
        let value = row.get(data).ok_or_else(legacy_refusal)?;
        let len = match value {
            Value::Image(BlobVal::Ref(r)) => r.len as usize,
            Value::Image(BlobVal::Data(b)) | Value::Binary(b) => b.len(),
            _ => return Err(legacy_refusal()),
        };
        if len > v8users::MAX_DATA_BYTES
            || row.get(size) != Some(&Value::Numeric(Numeric::from_i64(len as i64)))
        {
            return Err(legacy_refusal());
        }
        let bytes = match value {
            Value::Image(BlobVal::Ref(r)) => params.blob_bytes(*r).map_err(storage)?,
            Value::Image(BlobVal::Data(b)) | Value::Binary(b) => b.clone(),
            _ => return Err(legacy_refusal()),
        };
        let decoded = v8users::data::decode(&bytes).map_err(|_| legacy_refusal())?;
        let mut depth = 0usize;
        let mut quoted = false;
        for byte in decoded.text.bytes() {
            match byte {
                b'"' => quoted = !quoted,
                b'{' if !quoted => {
                    depth += 1;
                    if depth > 32 {
                        return Err(legacy_refusal());
                    }
                }
                b'}' if !quoted => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        let tree = brace::parse(&decoded.text).map_err(|_| legacy_refusal())?;
        let fields = tree.as_list().ok_or_else(legacy_refusal)?;
        if fields.len() != 4
            || [0, 2, 3]
                .into_iter()
                .any(|i| fields.get(i).and_then(brace::Node::as_u32) != Some(1))
        {
            return Err(legacy_refusal());
        }
        let card = fields.get(1).ok_or_else(legacy_refusal)?;
        if card
            .as_list()
            .is_none_or(|fields| !matches!(fields.len(), 38 | 39))
        {
            return Err(legacy_refusal());
        }
        let modern = directory
            .rows
            .iter()
            .find(|row| &row.data.tree == card)
            .ok_or_else(legacy_refusal)?;
        *row.get_mut(data).ok_or_else(legacy_refusal)? = Value::Image(BlobVal::Data(bytes.clone()));
        mirror = Some(Mirror {
            id: modern.data.id().ok_or_else(legacy_refusal)?,
            slot,
            row,
            bytes,
        });
    }
    Ok(mirror)
}

fn preserve_mirror(
    mirror: Option<Uuid16>,
    target: Option<&Stored>,
    data: Option<&UserData>,
) -> Result<()> {
    if let Some(target) = target
        && mirror.is_some()
        && mirror == target.data.id()
        && data.is_none()
    {
        return Err(UserWriteError::UnsupportedLayout(
            "mirrored users.usr card removal requires native qualification",
        ));
    }
    Ok(())
}

fn encode_mirror(modern: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let decoded = v8users::data::decode(modern).map_err(layout)?;
    let size = decoded
        .text
        .len()
        .checked_add(10)
        .and_then(|n| n.checked_add(4 + key.len()))
        .filter(|n| *n <= v8users::MAX_DATA_BYTES)
        .ok_or(UserWriteError::InvalidInput("users.usr result budget"))?;
    let mut text = String::with_capacity(size - 4 - key.len());
    text.push_str("{1,\r\n");
    text.push_str(&decoded.text);
    text.push_str(",1,1}");
    v8users::data::encode(&text, key).map_err(layout)
}

impl Mirror {
    fn prepare(&mut self, tx: &onecd::ReadTxn, modern: &UserRow) -> Result<()> {
        let key = v8users::data::decode(&self.bytes).map_err(layout)?.key;
        self.bytes = encode_mirror(&modern.data, &key)?;
        let table = tx.table("PARAMS").map_err(storage)?;
        for (name, value) in [
            (
                "DATASIZE",
                Value::Numeric(Numeric::from_i64(self.bytes.len() as i64)),
            ),
            (
                "BINARYDATA",
                Value::Image(BlobVal::Data(self.bytes.clone())),
            ),
        ] {
            let col = table.descr().field(name).ok_or_else(legacy_refusal)?.0;
            *self.row.get_mut(col).ok_or_else(legacy_refusal)? = value;
        }
        Ok(())
    }
    fn verify(&self, tx: &onecd::ReadTxn) -> Result<()> {
        let actual =
            legacy(tx, &Directory::read(tx)?)?.ok_or(UserWriteError::CommittedUnverified)?;
        if actual.id != self.id
            || actual.slot != self.slot
            || actual.row != self.row
            || actual.bytes != self.bytes
        {
            return Err(UserWriteError::CommittedUnverified);
        }
        Ok(())
    }
}

fn policy(tx: &onecd::ReadTxn, card: Option<&Card>) -> Result<()> {
    if card.is_some_and(|c| {
        !c.authentication.standard
            || !c.password_policy.is_empty()
            || c.password_hash_algorithm != 0
            || c.password_does_not_satisfy_requirements
            || c.cannot_change_password
            || c.second_factor_processing > 1
    }) {
        return Err(UserWriteError::UnsupportedPolicy);
    }
    for name in ["V8USERPWDPLCS", "V8CMSDPWDS"] {
        let t = tx
            .table(name)
            .map_err(|_| UserWriteError::UnsupportedPolicy)?;
        if t.scan().next().transpose().map_err(storage)?.is_some() {
            return Err(UserWriteError::UnsupportedPolicy);
        }
    }
    let params = tx.table("PARAMS").map_err(storage)?;
    let col = params
        .descr()
        .field("FILENAME")
        .ok_or(UserWriteError::UnsupportedPolicy)?
        .0;
    for r in params.scan() {
        let (_, r) = r.map_err(storage)?;
        if matches!(r.get(col),Some(Value::VarChar(n)|Value::Char(n)) if n.trim_end().eq_ignore_ascii_case("usrpwdplcs.plcs"))
        {
            return Err(UserWriteError::UnsupportedPolicy);
        }
    }
    Ok(())
}
fn clear(data: &UserData, utc: BslDate) -> Result<UserData> {
    let f = data
        .tree
        .as_list()
        .ok_or(UserWriteError::UnsupportedPolicy)?;
    if f.get(30).and_then(brace::Node::as_u32) != Some(1) {
        return Err(UserWriteError::UnsupportedLayout(
            "unqualified password-history transition",
        ));
    }
    if f.len() != 38
        || f.get(17).and_then(brace::Node::as_u32) != Some(0)
        || f.get(27).and_then(brace::Node::as_u32) != Some(0)
        || f.get(32).and_then(brace::Node::as_u32) != Some(0)
    {
        return Err(UserWriteError::UnsupportedPolicy);
    }
    let mut result = data.clone();
    let hashes = v8users::password::hashes("");
    for (i, v) in [
        (2, brace::Node::str("")),
        (12, brace::Node::str(&hashes.plain)),
        (13, brace::Node::str(&hashes.upper)),
    ] {
        *result
            .tree
            .get_mut(&[i])
            .ok_or(UserWriteError::UnsupportedPolicy)? = v;
    }
    let p = utc.parts();
    *result
        .tree
        .get_mut(&[29])
        .ok_or(UserWriteError::UnsupportedPolicy)? = brace::Node::atom(format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        p.year, p.month, p.day, p.hour, p.minute, p.second
    ));
    let fields = result
        .tree
        .as_list_mut()
        .ok_or(UserWriteError::UnsupportedPolicy)?;
    let count = fields
        .get(30)
        .and_then(brace::Node::as_u32)
        .ok_or(UserWriteError::UnsupportedPolicy)?;
    let end = 31 + count as usize;
    *fields
        .get_mut(30)
        .ok_or(UserWriteError::UnsupportedPolicy)? = brace::Node::int(i64::from(count) + 1);
    fields.insert(end, brace::Node::str(&hashes.upper));
    *fields
        .get_mut(end + 2)
        .ok_or(UserWriteError::UnsupportedPolicy)? = brace::Node::str("");
    UserData::parse(&result.to_text()).map_err(layout)
}
fn project(user: &UserRow, descr: &TableDescr) -> Result<Vec<Value>> {
    let date = ibschema::values::datetime_from_ticks(user.changed.ticks() as u64)
        .map_err(|_| UserWriteError::UnsupportedLayout("date range"))?;
    descr
        .fields
        .iter()
        .map(|f| {
            Ok(match f.name.as_str() {
                "ID" => Value::Binary(ibschema::values::RefId::from_uuid(user.id).0.to_vec()),
                "NAME" => Value::VarChar(user.name.clone()),
                "DESCR" => Value::VarChar(user.descr.clone()),
                "OSNAME" => user.osname.clone().map_or(Value::Null, Value::VarChar),
                "CHANGED" => Value::DateTime(date),
                "ROLESID" => Value::Numeric(Numeric::from_i64(i64::from(user.rolesid))),
                "SHOW" => Value::Bool(user.show),
                "EAUTH" => Value::Bool(user.eauth),
                "ADMROLE" => Value::Bool(user.admrole),
                "USSPRH" => Value::Numeric(Numeric::from_i64(user.ussprh as i64)),
                "EMAIL" => Value::VarChar(user.email.clone()),
                "DATA" => Value::Image(BlobVal::Data(user.data.clone())),
                _ => return Err(UserWriteError::UnsupportedLayout("unknown user column")),
            })
        })
        .collect()
}
fn rebuild(data: &UserData, changed: BslDate, facts: &Facts) -> Result<UserRow> {
    UserRow::rebuild(
        &v8users::data::encode(&data.to_text(), &v8users::new_key()).map_err(layout)?,
        changed,
        &v8users::rebuild::RebuildContext {
            administration_roles: Some(&facts.admin),
            user_separators: &[],
        },
        &v8users::new_key(),
    )
    .map_err(layout)
}
fn cleanup(tx: &mut WriteTxn<'_>, id: &str) -> Result<()> {
    let bytes =
        ibschema::values::RefId::from_uuid(Uuid16::parse(id).ok_or(UserWriteError::NotFound)?)
            .0
            .to_vec();
    let slots = {
        let t = tx.read_table("V8USERSMATKEYS").map_err(storage)?;
        let col = t
            .descr()
            .field("ID")
            .ok_or(UserWriteError::UnsupportedLayout("matching keys schema"))?
            .0;
        let mut slots = Vec::new();
        for (i, r) in t.scan().enumerate() {
            let (slot, row) = r.map_err(storage)?;
            if i >= 100_000 {
                return Err(UserWriteError::UnsupportedLayout("matching keys limit"));
            }
            if matches!(row.get(col),Some(Value::Binary(b)) if b==&bytes) {
                slots.push(slot);
            }
        }
        slots
    };
    for slot in slots {
        tx.table("V8USERSMATKEYS")
            .map_err(storage)?
            .delete(slot)
            .map_err(storage)?;
    }
    Ok(())
}
fn names(name: Option<&str>, full: Option<&str>) -> Result<()> {
    if name.is_some_and(|s| {
        s.trim_end_matches(' ').is_empty() || s.encode_utf16().take(65).count() > 64
    }) {
        return Err(UserWriteError::InvalidInput("name"));
    }
    if full.is_some_and(|s| s.encode_utf16().take(129).count() > 128) {
        return Err(UserWriteError::InvalidInput("full_name"));
    }
    Ok(())
}
enum ClearAdmission {
    Public,
    #[cfg(all(feature = "testing", unix))]
    SealedInput(String),
    #[cfg(any(test, feature = "testing"))]
    SealedFixture,
}
impl ClearAdmission {
    fn admitted(&self) -> bool {
        match self {
            Self::Public => super::qualification::public_clear(),
            #[cfg(all(feature = "testing", unix))]
            Self::SealedInput(_) => true,
            #[cfg(any(test, feature = "testing"))]
            Self::SealedFixture => true,
        }
    }
}
#[cfg(test)]
thread_local! {
    static TRANSACTION_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
pub(super) fn apply(base: &Path, request: &UserWriteRequest) -> Result<UserWriteOutcome> {
    apply_transaction(base, request, ClearAdmission::Public)
}
#[cfg(any(test, feature = "testing"))]
pub(super) fn apply_fixture_clear(
    base: &Path,
    request: &UserWriteRequest,
) -> Result<UserWriteOutcome> {
    apply_transaction(base, request, ClearAdmission::SealedFixture)
}
#[cfg(all(feature = "testing", unix))]
pub(super) fn apply_qualified_clear(
    base: &Path,
    request: &UserWriteRequest,
    source: &str,
) -> Result<UserWriteOutcome> {
    apply_transaction(
        base,
        request,
        ClearAdmission::SealedInput(source.to_owned()),
    )
}
fn apply_transaction(
    base: &Path,
    request: &UserWriteRequest,
    clear_admission: ClearAdmission,
) -> Result<UserWriteOutcome> {
    #[cfg(test)]
    TRANSACTION_ENTRIES.with(|n| n.set(n.get() + 1));
    if request.revision.len() > 128 {
        return Err(UserWriteError::StaleRevision);
    }
    match &request.action {
        UserAction::Create { draft } => names(Some(&draft.name), Some(&draft.full_name))?,
        UserAction::Update { patch, .. } => {
            names(patch.name.as_deref(), patch.full_name.as_deref())?
        }
        _ => {}
    }
    let clearing = matches!(request.action, UserAction::ClearPassword { .. });
    if (clearing && !clear_admission.admitted())
        || (!cfg!(windows) && !(cfg!(any(target_os = "linux", target_os = "macos")) && clearing))
    {
        return Err(UserWriteError::UnsupportedPlatform);
    }
    let path = snapshot::path(base)?;
    #[cfg(test)]
    tests::transaction_event("before-open");
    #[cfg(unix)]
    let source = super::source::Source::open(&path, onecd::Mode::ReadWrite)?;
    #[cfg(unix)]
    let db = source.database(onecd::Mode::ReadWrite)?;
    #[cfg(not(unix))]
    let db = Database::open(&path, OpenOptions::read_write()).map_err(storage)?;
    #[cfg(test)]
    tests::transaction_event("opened");
    #[cfg(unix)]
    let witness = {
        source.verify_path()?;
        source.witness()?
    };
    #[cfg(not(unix))]
    let witness = snapshot::witness(&path)?;
    #[cfg(all(feature = "testing", unix))]
    if let ClearAdmission::SealedInput(pin) = &clear_admission
        && !snapshot::matches_pin(&witness, pin)
    {
        return Err(UserWriteError::UnsupportedSource);
    }
    snapshot::consume(&request.revision, &path, &witness)?;
    let tx = db.read();
    if !db.verify().map_err(storage)?.violations.is_empty() {
        return Err(UserWriteError::UnsupportedLayout("integrity check failed"));
    }
    let facts = Facts::read(&tx)?;
    let directory = Directory::read(&tx)?;
    let mut mirror = legacy(&tx, &directory)?;
    let mut cards: Vec<_> = directory.rows.iter().map(|r| r.card.clone()).collect();
    resolve_roles(&mut cards, &facts.cfg)
        .map_err(|_| UserWriteError::UnsupportedLayout("public output limit"))?;
    let target = match &request.action {
        UserAction::Create { .. } => None,
        UserAction::Update { identity, .. }
        | UserAction::ClearPassword { identity }
        | UserAction::Delete { identity } => {
            if identity.len() > 4096 {
                return Err(UserWriteError::InvalidInput("identity"));
            }
            let card = find(&cards, identity).map_err(|e| match e {
                Error::Ambiguous => UserWriteError::Ambiguous,
                Error::NotFound => UserWriteError::NotFound,
                _ => UserWriteError::InvalidInput("identity"),
            })?;
            Some(
                directory
                    .rows
                    .iter()
                    .find(|r| r.card.id == card.id)
                    .ok_or(UserWriteError::NotFound)?,
            )
        }
    };
    if target.is_some_and(|r| !r.card.data_separation.is_empty()) {
        return Err(UserWriteError::DataAreaReadOnly);
    }
    if facts.separated {
        return Err(UserWriteError::UnsupportedLayout(
            "global user semantics in a separated configuration require native qualification",
        ));
    }
    let deleting = matches!(request.action, UserAction::Delete { .. });
    if matches!(
        request.action,
        UserAction::Delete { .. } | UserAction::ClearPassword { .. }
    ) && !request.confirmed
    {
        return Err(UserWriteError::ConfirmationRequired);
    }
    if deleting && cards.len() == 1 {
        if !request.allow_last_user {
            return Err(UserWriteError::LastUserConfirmationRequired);
        }
        return Err(UserWriteError::UnsupportedLayout(
            "final-user and legacy restart semantics require native qualification",
        ));
    }
    let counter = directory
        .service
        .as_ref()
        .and_then(|r| r.data.rolesid())
        .unwrap_or(0);
    let sets: Vec<_> = directory
        .rows
        .iter()
        .map(|r| (r.data.roles(), r.card.role_set))
        .collect();
    let mut sets = v8users::roles::RoleSets::from_rows(&sets, counter);
    if matches!(request.action, UserAction::ClearPassword { .. })
        && (directory.service.is_none() || sets.max() > counter)
    {
        return Err(UserWriteError::UnsupportedLayout(
            "служебный счётчик ролей не согласован с наборами ролей — требуется проверка в 1С",
        ));
    }
    let mut changed = target.map_or(BslDate::EMPTY, |r| r.changed);
    let data = match &request.action {
        UserAction::Create { draft } => {
            if cards.len() >= MAX_USERS - 1 {
                return Err(UserWriteError::InvalidInput("users limit"));
            }
            policy(&tx, None)?;
            let roles = facts.roles(&draft.roles)?;
            let number = sets.number_for(&roles).map_err(layout)?;
            let id = Uuid16::from_bytes(
                v8users::new_key()
                    .as_slice()
                    .try_into()
                    .map_err(|_| UserWriteError::StorageBeforeCommit)?,
            );
            let mut secret = [0u8; 32];
            for (chunk, key) in secret
                .chunks_mut(16)
                .zip([v8users::new_key(), v8users::new_key()])
            {
                chunk.copy_from_slice(&key);
            }
            changed = local_now()?;
            let utc = now()?;
            Some(UserData::build(&NewUser {
                id,
                name: draft.name.clone(),
                full_name: draft.full_name.clone(),
                roles,
                std_auth: true,
                show_in_list: draft.show_in_list,
                password: None,
                rolesid: number,
                changed_local: changed,
                password_changed_utc: Some(utc),
                secret32: secret,
            }))
        }
        UserAction::Update { patch, .. } => {
            let source = target.ok_or(UserWriteError::NotFound)?;
            let roles = patch.roles.as_ref().map(|r| facts.roles(r)).transpose()?;
            if roles.is_some()
                && source
                    .card
                    .roles
                    .iter()
                    .any(|r| !facts.roles.iter().any(|known| known.id == r.id))
            {
                return Err(UserWriteError::UnsupportedLayout(
                    "cannot replace unresolved existing roles",
                ));
            }
            let number = roles
                .as_ref()
                .map(|r| sets.number_for(r))
                .transpose()
                .map_err(layout)?;
            Some(
                source
                    .data
                    .edit_public(&PublicPatch {
                        name: patch.name.clone(),
                        full_name: patch.full_name.clone(),
                        roles,
                        show_in_list: patch.show_in_list,
                        rolesid: number,
                        changed_local: None,
                    })
                    .map_err(layout)?,
            )
        }
        UserAction::ClearPassword { .. } => {
            if !clear_admission.admitted() {
                return Err(UserWriteError::UnsupportedPolicy);
            }
            let source = target.ok_or(UserWriteError::NotFound)?;
            policy(&tx, Some(&source.card))?;
            let utc = now()?;
            if source
                .data
                .password_changed_utc()
                .is_some_and(|previous| previous > utc)
            {
                return Err(UserWriteError::UnsupportedLayout(
                    "password change date is in the future",
                ));
            }
            changed = source.changed.max(local_now()?);
            Some(clear(&source.data, utc)?)
        }
        UserAction::Delete { .. } => None,
    };
    let prepared = data
        .as_ref()
        .map(|data| rebuild(data, changed, &facts))
        .transpose()?;
    let mut user = prepared
        .as_ref()
        .map(|r| Card::from_blob(&r.data))
        .transpose()
        .map_err(layout)?;
    if let Some(card) = user.as_mut() {
        resolve_roles(std::slice::from_mut(card), &facts.cfg)
            .map_err(|_| UserWriteError::UnsupportedLayout("public output limit"))?;
        let request = onecd::KeyRequest::when_bound(None);
        let key = name_key(&card.name, request.as_ref())
            .map_err(|_| UserWriteError::InvalidInput("name"))?;
        for other in cards.iter().filter(|c| c.id != card.id) {
            if name_key(&other.name, request.as_ref())
                .map_err(|_| UserWriteError::UnsupportedLayout("stored name collation"))?
                .bytes()
                == key.bytes()
            {
                return Err(UserWriteError::InvalidInput("duplicate name"));
            }
        }
    }
    let mut remaining = cards
        .iter()
        .filter(|c| target.is_none_or(|t| t.card.id != c.id))
        .collect::<Vec<_>>();
    if let Some(c) = &user {
        remaining.push(c);
    }
    if !remaining.is_empty() && !remaining.iter().any(|c| facts.is_admin(c)) {
        return Err(UserWriteError::LastAdministrator);
    }
    preserve_mirror(mirror.as_ref().map(|m| m.id), target, data.as_ref())?;
    let update_mirror = mirror
        .as_ref()
        .is_some_and(|m| target.is_some_and(|t| t.data.id() == Some(m.id)));
    if update_mirror {
        mirror
            .as_mut()
            .ok_or_else(legacy_refusal)?
            .prepare(&tx, prepared.as_ref().ok_or_else(legacy_refusal)?)?;
    }
    let mut output = OutputBudget::new();
    for card in &remaining {
        output
            .admit(card)
            .map_err(|_| UserWriteError::InvalidInput("result collection or output limit"))?;
    }
    let users_remaining = remaining.len();
    drop(remaining);
    let outcome = UserWriteOutcome {
        revision: snapshot::nonce(),
        user,
        deleted_id: deleting
            .then(|| target.map(|t| t.card.id.clone()))
            .flatten(),
        users_remaining,
    };
    serde_json::to_writer(OutputBudget::new(), &outcome)
        .map_err(|_| UserWriteError::InvalidInput("output limit"))?;
    let related = Related::new(sets.max(), outcome.deleted_id.as_deref())?;
    let mut write = db.write().map_err(storage)?;
    if update_mirror {
        let mirror = mirror.as_ref().ok_or_else(legacy_refusal)?;
        write
            .table("PARAMS")
            .map_err(storage)?
            .update(mirror.slot, &mirror.row)
            .map_err(storage)?;
    }
    let descr = write
        .read_table("V8USERS")
        .map_err(storage)?
        .descr()
        .clone();
    if let Some(user) = &prepared {
        let row = project(user, &descr)?;
        let mut t = write.table("V8USERS").map_err(storage)?;
        if let Some(old) = target {
            t.update(old.slot, &row).map_err(storage)?;
        } else {
            t.insert(&row).map_err(storage)?;
        }
    }
    if deleting {
        let old = target.ok_or(UserWriteError::NotFound)?;
        cleanup(&mut write, &old.card.id)?;
        write
            .table("V8USERS")
            .map_err(storage)?
            .delete(old.slot)
            .map_err(storage)?;
    }
    let mut service_size = directory.service.as_ref().map_or(0, |s| s.size);
    if sets.max() != counter || directory.service.is_none() {
        let (data, changed) = if let Some(s) = &directory.service {
            (
                s.data
                    .edit_public(&PublicPatch {
                        rolesid: Some(sets.max()),
                        ..Default::default()
                    })
                    .map_err(layout)?,
                s.changed,
            )
        } else {
            (
                UserData::service_row(
                    Uuid16::parse(SERVICE_ROW_ID)
                        .ok_or(UserWriteError::UnsupportedLayout("service UUID"))?,
                    sets.max(),
                    BslDate::EMPTY,
                ),
                BslDate::EMPTY,
            )
        };
        let service = rebuild(&data, changed, &facts)?;
        service_size = service.data.len();
        let row = project(&service, &descr)?;
        let mut table = write.table("V8USERS").map_err(storage)?;
        if let Some(s) = &directory.service {
            table.update(s.slot, &row).map_err(storage)?;
        } else {
            table.insert(&row).map_err(storage)?;
        }
    }
    let data_total = directory
        .rows
        .iter()
        .filter(|r| target.is_none_or(|t| t.card.id != r.card.id))
        .try_fold(service_size, |n, r| n.checked_add(r.size))
        .and_then(|n| n.checked_add(prepared.as_ref().map_or(0, |r| r.data.len())));
    if data_total.is_none_or(|n| n > MAX_TOTAL_DATA) {
        return Err(UserWriteError::InvalidInput("result DATA budget"));
    }
    #[cfg(unix)]
    let current = {
        source.verify_path()?;
        source.witness()?
    };
    #[cfg(not(unix))]
    let current = snapshot::witness(&path)?;
    if current != witness {
        return Err(UserWriteError::StaleRevision);
    }
    #[cfg(test)]
    tests::transaction_event("commit");
    let result = finish(write, || {
        #[cfg(unix)]
        source
            .sync_parent()
            .map_err(|_| UserWriteError::CommittedUnverified)?;
        #[cfg(unix)]
        let reopened = source
            .database(onecd::Mode::ReadOnly)
            .map_err(|_| UserWriteError::CommittedUnverified)?;
        #[cfg(not(unix))]
        let raw = onecd::FileStorage::open(&path, false)
            .map_err(|_| UserWriteError::CommittedUnverified)?;
        #[cfg(not(unix))]
        let reopened = Database::open_storage(Arc::new(raw), OpenOptions::read_only())
            .map_err(|_| UserWriteError::CommittedUnverified)?;
        verify_committed(&reopened, &outcome, prepared.as_ref(), &related)?;
        if let Some(mirror) = &mirror {
            mirror
                .verify(&reopened.read())
                .map_err(|_| UserWriteError::CommittedUnverified)?;
        }
        #[cfg(unix)]
        let after = {
            source
                .verify_path()
                .map_err(|_| UserWriteError::CommittedUnverified)?;
            source
                .witness()
                .map_err(|_| UserWriteError::CommittedUnverified)?
        };
        #[cfg(not(unix))]
        let after = snapshot::witness(&path).map_err(|_| UserWriteError::CommittedUnverified)?;
        snapshot::register(outcome.revision.clone(), path.clone(), after)
            .map_err(|_| UserWriteError::CommittedUnverified)
    });
    #[cfg(unix)]
    if matches!(result, Err(UserWriteError::CommitUnknown)) && source.refused_before_write() {
        return Err(UserWriteError::StaleRevision);
    }
    result?;
    Ok(outcome)
}
struct Related {
    counter: u32,
    deleted: Option<[u8; 16]>,
}
impl Related {
    fn new(counter: u32, deleted: Option<&str>) -> Result<Self> {
        let deleted = deleted
            .map(|id| {
                Uuid16::parse(id)
                    .map(|id| ibschema::values::RefId::from_uuid(id).0)
                    .ok_or(UserWriteError::NotFound)
            })
            .transpose()?;
        Ok(Self { counter, deleted })
    }
    fn verify(&self, tx: &onecd::ReadTxn) -> Result<()> {
        let directory = Directory::read(tx)?;
        let service = directory
            .service
            .ok_or(UserWriteError::CommittedUnverified)?;
        if service.data.rolesid() != Some(self.counter) {
            return Err(UserWriteError::CommittedUnverified);
        }
        let table = tx.table("V8USERS").map_err(storage)?;
        let column = table
            .descr()
            .field("ROLESID")
            .ok_or(UserWriteError::CommittedUnverified)?
            .0;
        let row = table
            .get(service.slot)
            .map_err(storage)?
            .ok_or(UserWriteError::CommittedUnverified)?;
        if row.get(column) != Some(&Value::Numeric(Numeric::from_i64(i64::from(self.counter)))) {
            return Err(UserWriteError::CommittedUnverified);
        }
        if let Some(deleted) = self.deleted {
            let table = tx.table("V8USERSMATKEYS").map_err(storage)?;
            let id = table
                .descr()
                .field("ID")
                .ok_or(UserWriteError::CommittedUnverified)?
                .0;
            for (i, row) in table.scan().enumerate() {
                if i >= 100_000 {
                    return Err(UserWriteError::CommittedUnverified);
                }
                let (_, row) = row.map_err(storage)?;
                if matches!(row.get(id),Some(Value::Binary(b)) if b.as_slice()==deleted) {
                    return Err(UserWriteError::CommittedUnverified);
                }
            }
        }
        Ok(())
    }
}

fn verify_committed(
    db: &Database,
    outcome: &UserWriteOutcome,
    prepared: Option<&UserRow>,
    related: &Related,
) -> Result<()> {
    if !db
        .verify()
        .map_err(|_| UserWriteError::CommittedUnverified)?
        .violations
        .is_empty()
    {
        return Err(UserWriteError::CommittedUnverified);
    }
    let actual = read_cards(&db.read()).map_err(|_| UserWriteError::CommittedUnverified)?;
    if actual.len() != outcome.users_remaining
        || outcome
            .deleted_id
            .as_ref()
            .is_some_and(|id| actual.iter().any(|c| &c.id == id))
        || outcome.user.as_ref().is_some_and(|wanted| {
            actual.iter().find(|c| c.id == wanted.id).is_none_or(|c| {
                c.name != wanted.name
                    || c.full_name != wanted.full_name
                    || c.role_set != wanted.role_set
                    || c.password_set != wanted.password_set
            })
        })
    {
        return Err(UserWriteError::CommittedUnverified);
    }
    related
        .verify(&db.read())
        .map_err(|_| UserWriteError::CommittedUnverified)?;
    if let Some(expected) = &prepared {
        let txn = db.read();
        let table = txn
            .table("V8USERS")
            .map_err(|_| UserWriteError::CommittedUnverified)?;
        let id = table
            .descr()
            .field("ID")
            .ok_or(UserWriteError::CommittedUnverified)?
            .0;
        let data = table
            .descr()
            .field("DATA")
            .ok_or(UserWriteError::CommittedUnverified)?
            .0;
        let expected_id = ibschema::values::RefId::from_uuid(expected.id).0;
        let mut matched = false;
        for row in table.scan() {
            let (_, row) = row.map_err(|_| UserWriteError::CommittedUnverified)?;
            if matches!(row.get(id),Some(Value::Binary(b)) if b.as_slice()==expected_id) {
                let bytes = match row.get(data) {
                    Some(Value::Image(BlobVal::Ref(r))) => table
                        .blob_bytes(*r)
                        .map_err(|_| UserWriteError::CommittedUnverified)?,
                    _ => return Err(UserWriteError::CommittedUnverified),
                };
                if bytes != expected.data {
                    return Err(UserWriteError::CommittedUnverified);
                }
                matched = true;
            }
        }
        if !matched {
            return Err(UserWriteError::CommittedUnverified);
        }
    }
    Ok(())
}
fn finish(write: WriteTxn<'_>, verify: impl FnOnce() -> Result<()>) -> Result<()> {
    write.commit().map_err(|_| UserWriteError::CommitUnknown)?;
    verify().map_err(|_| UserWriteError::CommittedUnverified)
}
fn now() -> Result<BslDate> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| UserWriteError::StorageBeforeCommit)?
            .as_secs(),
    )
    .map_err(|_| UserWriteError::StorageBeforeCommit)?;
    let epoch =
        BslDate::from_parts(1970, 1, 1, 0, 0, 0).ok_or(UserWriteError::StorageBeforeCommit)?;
    seconds
        .checked_mul(BslDate::TICKS_PER_SECOND)
        .and_then(|s| epoch.ticks().checked_add(s))
        .and_then(BslDate::from_ticks)
        .ok_or(UserWriteError::StorageBeforeCommit)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn local_now() -> Result<BslDate> {
    use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};
    let mut time = std::mem::MaybeUninit::<SYSTEMTIME>::uninit();
    unsafe {
        GetLocalTime(time.as_mut_ptr());
    }
    let t = unsafe { time.assume_init() };
    BslDate::from_parts(
        t.wYear.into(),
        t.wMonth.into(),
        t.wDay.into(),
        t.wHour.into(),
        t.wMinute.into(),
        t.wSecond.into(),
    )
    .ok_or(UserWriteError::StorageBeforeCommit)
}
#[cfg(unix)]
#[allow(unsafe_code)]
fn local_now() -> Result<BslDate> {
    let mut seconds = 0;
    if unsafe { libc::time(&mut seconds) } == -1 {
        return Err(UserWriteError::StorageBeforeCommit);
    }
    let mut time = std::mem::MaybeUninit::<libc::tm>::uninit();
    if unsafe { libc::localtime_r(&seconds, time.as_mut_ptr()) }.is_null() {
        return Err(UserWriteError::StorageBeforeCommit);
    }
    let time = unsafe { time.assume_init() };
    let part = |n: i32| u32::try_from(n).map_err(|_| UserWriteError::StorageBeforeCommit);
    BslDate::from_parts(
        part(
            time.tm_year
                .checked_add(1900)
                .ok_or(UserWriteError::StorageBeforeCommit)?,
        )?,
        part(
            time.tm_mon
                .checked_add(1)
                .ok_or(UserWriteError::StorageBeforeCommit)?,
        )?,
        part(time.tm_mday)?,
        part(time.tm_hour)?,
        part(time.tm_min)?,
        part(time.tm_sec)?,
    )
    .ok_or(UserWriteError::StorageBeforeCommit)
}
#[cfg(not(any(unix, windows)))]
fn local_now() -> Result<BslDate> {
    Err(UserWriteError::UnsupportedPlatform)
}
