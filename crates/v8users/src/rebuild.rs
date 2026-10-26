use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};

use brace::binary::Writer;
use bsl_prim::text::fold_name;

use crate::{
    BslDate, Error, MAX_DATA_BYTES, Uuid16,
    card::Separation,
    data,
    layout::MAX_COLLECTION,
    user::{UserData, UserRow, date_node},
};

const DEFAULT_USER_ID: &str = "071523a4-516f-4fce-ba4b-0d11ab7a1893";
const GLOBAL_USER: u64 = 1 << 32;

pub struct RebuildContext<'a> {
    pub administration_roles: Option<&'a [Uuid16]>,
    pub user_separators: &'a [&'a str],
}

impl UserRow {
    pub fn rebuild(
        blob: &[u8],
        changed_column: BslDate,
        context: &RebuildContext<'_>,
        key: &[u8],
    ) -> Result<Self, Error> {
        if changed_column.frac_ticks() != 0 {
            return Err(Error::FractionalChanged);
        }
        if key.is_empty() || key.len() > u8::MAX as usize {
            return Err(Error::BadKeyLength(key.len()));
        }
        let decoded = data::decode(blob)?;
        let mut parsed = UserData::parse(&decoded.text)?;
        let card = parsed.card()?;
        for (field, value, width) in [
            (1, card.name.as_str(), 64),
            (3, card.full_name.as_str(), 128),
            (25, card.email.as_str(), 128),
        ] {
            if value.encode_utf16().take(width + 1).count() > width {
                return Err(Error::BadField(field));
            }
        }
        let data_changed = parsed.changed_local();
        let changed = data_changed.map_or(changed_column, |d| d.max(changed_column));
        let id = parsed.id().ok_or(Error::BadField(0))?;
        let admrole = card.id == DEFAULT_USER_ID
            || context.administration_roles.is_none_or(|admin_roles| {
                card.data_separation.is_empty()
                    && parsed.roles().iter().any(|role| admin_roles.contains(role))
            });
        let ussprh = separator_hash(&card.data_separation, context.user_separators)?;
        let osname = if card.authentication.os && !card.os_user.trim().is_empty() {
            let mut units = 0;
            Some(
                ['\\', '\\']
                    .into_iter()
                    .chain(card.os_user.chars())
                    .take_while(|c| {
                        units += c.len_utf16();
                        units <= 128
                    })
                    .collect(),
            )
        } else {
            None
        };
        let text = if data_changed.is_some_and(|d| d != changed) {
            *parsed.tree.get_mut(&[16]).ok_or(Error::BadField(16))? = date_node(changed);
            parsed.to_text()
        } else {
            decoded.text
        };
        let output_size = text
            .len()
            .checked_add(key.len())
            .and_then(|n| n.checked_add(4))
            .ok_or(Error::Limit)?;
        if output_size > MAX_DATA_BYTES {
            return Err(Error::Limit);
        }
        Ok(Self {
            id,
            name: card.name,
            descr: card.full_name,
            osname,
            changed,
            rolesid: card.role_set,
            show: card.authentication.standard && card.show_in_list,
            data: data::encode(&text, key)?,
            eauth: card.authentication.standard,
            admrole,
            ussprh,
            email: card.email,
        })
    }
}

fn separator_hash(values: &[Separation], names: &[&str]) -> Result<u64, Error> {
    if names.len() > MAX_COLLECTION
        || names
            .iter()
            .try_fold(0usize, |bytes, name| bytes.checked_add(name.len()))
            .is_none_or(|bytes| bytes > MAX_DATA_BYTES)
    {
        return Err(Error::InvalidRebuildContext);
    }
    let names: Vec<_> = names.iter().map(|name| fold_name(name)).collect();
    let unique: BTreeSet<_> = names.iter().map(String::as_str).collect();
    if unique.len() != names.len() {
        return Err(Error::InvalidRebuildContext);
    }
    let mut lookup = BTreeMap::new();
    for value in values {
        lookup
            .entry(fold_name(&value.name))
            .or_insert(value.value.as_str());
    }
    let mut stream = Writer::new(CrcSink(crc32fast::Hasher::new()));
    for name in names {
        let Some(value) = lookup.get(&name) else {
            return Ok(GLOBAL_USER);
        };
        stream.str(value).map_err(|_| Error::BadField(17))?;
    }
    let sink = stream.finish().map_err(|_| Error::BadField(17))?;
    Ok(u64::from(sink.0.finalize()))
}

struct CrcSink(crc32fast::Hasher);

impl Write for CrcSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
