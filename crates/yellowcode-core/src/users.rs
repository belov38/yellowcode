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
