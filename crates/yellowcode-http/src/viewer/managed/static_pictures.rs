use serde::{
    Deserializer,
    de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use std::fmt;
const INFO: &[u8] = include_bytes!("../../../../ui-style/data/pictures.json");
const BIN: &[u8] = include_bytes!("../../../../ui-style/data/pictures-taxi.bin");
struct Picture {
    mime: &'static str,
    offset: u64,
    len: u64,
}
struct Aliases<'a>(&'a str);
impl<'de> DeserializeSeed<'de> for Aliases<'_> {
    type Value = bool;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Aliases<'_> {
    type Value = bool;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("embedded picture aliases")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut rows: A) -> Result<bool, A::Error> {
        let mut matched = false;
        while let Some(name) = rows.next_element::<&str>()? {
            matched |= name == self.0;
        }
        Ok(matched)
    }
}
struct Row<'a>(&'a str);
impl<'de> DeserializeSeed<'de> for Row<'_> {
    type Value = Option<Picture>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Row<'_> {
    type Value = Option<Picture>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("embedded picture row")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut row: A) -> Result<Self::Value, A::Error> {
        let (mut matched, mut mime, mut offset, mut len) = (false, None, None, None);
        while let Some(key) = row.next_key::<&str>()? {
            match key {
                "en" | "id" => matched |= row.next_value::<&str>()? == self.0,
                "ru" => matched |= row.next_value_seed(Aliases(self.0))?,
                "format" => {
                    mime = Some(match row.next_value::<&str>()? {
                        "png" => "image/png",
                        "svg" => "image/svg+xml",
                        _ => {
                            return Err(serde::de::Error::custom(
                                "invalid embedded picture format",
                            ));
                        }
                    })
                }
                "offset" => offset = Some(row.next_value::<u64>()?),
                "len" => len = Some(row.next_value::<u64>()?),
                _ => {
                    let _ = row.next_value::<IgnoredAny>()?;
                }
            }
        }
        let picture = Picture {
            mime: mime.ok_or_else(|| serde::de::Error::missing_field("format"))?,
            offset: offset.ok_or_else(|| serde::de::Error::missing_field("offset"))?,
            len: len.ok_or_else(|| serde::de::Error::missing_field("len"))?,
        };
        Ok(matched.then_some(picture))
    }
}
struct Rows<'a>(&'a str);
impl<'de> Visitor<'de> for Rows<'_> {
    type Value = Option<Picture>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("embedded picture inventory")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut rows: A) -> Result<Self::Value, A::Error> {
        let mut found = None;
        while let Some(row) = rows.next_element_seed(Row(self.0))? {
            if row.is_some() {
                found = row;
            }
        }
        Ok(found)
    }
}
pub(super) fn lookup(key: &str) -> Option<(&'static str, &'static [u8])> {
    let mut decoder = serde_json::Deserializer::from_slice(INFO);
    let row = decoder.deserialize_seq(Rows(key)).ok()??;
    decoder.end().ok()?;
    let start = usize::try_from(row.offset).ok()?;
    let end = start.checked_add(usize::try_from(row.len).ok()?)?;
    Some((row.mime, BIN.get(start..end)?))
}
