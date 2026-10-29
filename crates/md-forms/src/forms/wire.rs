use super::{Form, FormResolution, FormSettingsLookup, FormShared, FormSource, shared::Shared};
use brace::{Guid, Node};
use mdschema::forms::FormCompat;
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolError;
type Result<T> = std::result::Result<T, ProtocolError>;

#[derive(Clone, Copy)]
struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let head = self.0.get(..count).ok_or(ProtocolError)?;
        self.0 = self.0.get(count..).ok_or(ProtocolError)?;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8> {
        self.take(1)?.first().copied().ok_or(ProtocolError)
    }
    fn flag(&mut self) -> Result<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProtocolError),
        }
    }
    fn number(&mut self) -> Result<usize> {
        let value = u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| ProtocolError)?);
        usize::try_from(value).map_err(|_| ProtocolError)
    }
    fn count(&mut self, minimum: usize) -> Result<usize> {
        let count = self.number()?;
        if count > self.0.len() / minimum {
            return Err(ProtocolError);
        }
        Ok(count)
    }
    fn blob(&mut self) -> Result<&'a [u8]> {
        let size = self.number()?;
        self.take(size)
    }
    fn text(&mut self) -> Result<&'a str> {
        std::str::from_utf8(self.blob()?).map_err(|_| ProtocolError)
    }
    fn guid(&mut self) -> Result<Guid> {
        Ok(Guid::from_bytes_le(
            self.take(16)?.try_into().map_err(|_| ProtocolError)?,
        ))
    }
    fn finish(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError)
        }
    }
}

pub struct Resolution {
    names: HashMap<Guid, String>,
    types: HashMap<Guid, String>,
    predefined: HashMap<Guid, String>,
    headers: HashMap<Guid, Vec<usize>>,
}
impl FormSettingsLookup for Resolution {
    fn name(&self, id: &Guid) -> Option<&str> {
        self.names.get(id).map(String::as_str)
    }
    fn type_name(&self, id: &Guid) -> Option<&str> {
        self.types.get(id).map(String::as_str)
    }
    fn sub_header(&self, id: &Guid) -> Option<&[usize]> {
        self.headers.get(id).map(Vec::as_slice)
    }
    fn common_picture(&self, name: &str) -> bool {
        self.names
            .values()
            .any(|n| n.strip_prefix("CommonPicture.") == Some(name))
    }
}
impl FormResolution for Resolution {
    fn predefined_name(&self, id: &Guid) -> Option<&str> {
        self.predefined.get(id).map(String::as_str)
    }
}
pub struct Facts {
    resolution: Resolution,
    v85: bool,
    compat: FormCompat,
    order: Vec<Guid>,
    patterns: HashMap<Guid, Node>,
}
fn strings(cur: &mut Cursor<'_>) -> Result<HashMap<Guid, String>> {
    let count = cur.count(24)?;
    let mut values = HashMap::with_capacity(count);
    for _ in 0..count {
        let id = cur.guid()?;
        let text = cur.text()?.to_owned();
        if values.insert(id, text).is_some() {
            return Err(ProtocolError);
        }
    }
    Ok(values)
}
pub fn decode_facts(bytes: &[u8]) -> Result<Facts> {
    let mut cur = Cursor(bytes);
    if cur.take(4)? != b"YCF1" {
        return Err(ProtocolError);
    }
    let v85 = cur.flag()?;
    let compat = match cur.byte()? {
        0 => FormCompat::Base,
        1 => FormCompat::V8_3_12,
        2 if !v85 => FormCompat::V8_5_1,
        _ => return Err(ProtocolError),
    };
    if cur.take(2)? != [0, 0] {
        return Err(ProtocolError);
    }
    let names = strings(&mut cur)?;
    let types = strings(&mut cur)?;
    let predefined = strings(&mut cur)?;
    let count = cur.count(24)?;
    let mut headers = HashMap::with_capacity(count);
    for _ in 0..count {
        let id = cur.guid()?;
        let count = cur.count(8)?;
        let mut path = Vec::with_capacity(count);
        for _ in 0..count {
            path.push(cur.number()?);
        }
        if headers.insert(id, path).is_some() {
            return Err(ProtocolError);
        }
    }
    let count = cur.count(16)?;
    let mut order = Vec::with_capacity(count);
    for _ in 0..count {
        order.push(cur.guid()?);
    }
    let count = cur.count(24)?;
    let mut patterns = HashMap::with_capacity(count);
    for _ in 0..count {
        let id = cur.guid()?;
        let pattern = brace::parse_doc(cur.blob()?)
            .map_err(|_| ProtocolError)?
            .root;
        if patterns.insert(id, pattern).is_some() {
            return Err(ProtocolError);
        }
    }
    cur.finish()?;
    Ok(Facts {
        resolution: Resolution {
            names,
            types,
            predefined,
            headers,
        },
        v85,
        compat,
        order,
        patterns,
    })
}
fn blob(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_le_bytes());
    out.extend_from_slice(value);
}
fn optional_text(out: &mut Vec<u8>, value: Option<&str>) {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        blob(out, value.as_bytes());
    }
}
pub fn render(source: Vec<u8>, facts: Facts, owner: Option<&str>, shared: bool) -> Result<Vec<u8>> {
    let Facts {
        resolution,
        v85,
        compat,
        order,
        patterns,
    } = facts;
    let state = if shared {
        Some(FormShared::Native(Arc::new(
            Shared::from_resolution(v85, compat, order, patterns, &resolution)
                .map_err(|_| ProtocolError)?,
        )))
    } else {
        None
    };
    let form = Form {
        source: Some(FormSource::Text(Arc::from(source))),
        shared: state,
        ..Form::default()
    };
    let serialization = super::serialization(&form);
    let xml = super::form_xml_resolved(&resolution, &form, owner);
    let mut out = b"YFR1".to_vec();
    out.push(u8::from(serialization.is_some()));
    if let Some(value) = serialization {
        value.with_text(|value| blob(&mut out, value.as_bytes()));
    }
    if let Some((xml, pictures, deferred)) = xml {
        out.extend_from_slice(&[2, u8::from(deferred)]);
        blob(&mut out, xml.as_bytes());
        out.extend_from_slice(&(pictures.len() as u64).to_le_bytes());
        for (path, bytes) in pictures {
            blob(&mut out, path.as_bytes());
            blob(&mut out, &bytes);
        }
    } else if serialization.is_some_and(|value| !value.is_4_59()) {
        out.push(1);
        let refusal = super::form_refusal_resolved(&resolution, &form, owner);
        optional_text(&mut out, refusal.as_deref());
    } else {
        out.push(0);
    }
    Ok(out)
}

pub struct Projection<'a> {
    pub serialization: Option<&'a str>,
    pub outcome: Outcome<'a>,
}
pub enum Outcome<'a> {
    Absent,
    Refused(Option<&'a str>),
    Present(Present<'a>),
}
pub struct Present<'a> {
    pub xml: &'a str,
    pub deferred: bool,
    pictures: &'a [u8],
    count: usize,
}
impl<'a> Present<'a> {
    pub fn pictures(&self) -> Pictures<'a> {
        Pictures {
            cursor: Cursor(self.pictures),
            remaining: self.count,
        }
    }
}
pub struct Pictures<'a> {
    cursor: Cursor<'a>,
    remaining: usize,
}
impl<'a> Iterator for Pictures<'a> {
    type Item = (&'a str, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        Some((self.cursor.text().ok()?, self.cursor.blob().ok()?))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for Pictures<'_> {}
fn serialization_text(value: &str) -> bool {
    let Some((a, b)) = value.split_once('/') else {
        return false;
    };
    [a, b].into_iter().all(|part| {
        !part.is_empty()
            && (part.len() == 1 || !part.starts_with('0'))
            && part.bytes().all(|b| b.is_ascii_digit())
            && part.parse::<u16>().is_ok()
    })
}
pub fn decode_result(bytes: &[u8]) -> Result<Projection<'_>> {
    decode_result_checked(bytes, || Ok::<_, ProtocolError>(()))
}
pub fn decode_result_checked<E: From<ProtocolError>>(
    bytes: &[u8],
    mut check: impl FnMut() -> std::result::Result<(), E>,
) -> std::result::Result<Projection<'_>, E> {
    check()?;
    let mut cur = Cursor(bytes);
    if cur.take(4)? != b"YFR1" {
        return Err(ProtocolError.into());
    }
    let serialization = if cur.flag()? {
        Some(checked_text(&mut cur, &mut check)?)
    } else {
        None
    };
    if serialization.is_some_and(|s| s.len() > 11 || !serialization_text(s)) {
        return Err(ProtocolError.into());
    }
    let outcome = match cur.byte()? {
        0 => {
            if serialization.is_some_and(|s| s != "4/59") {
                return Err(ProtocolError.into());
            }
            Outcome::Absent
        }
        1 => {
            if serialization.is_none_or(|s| s == "4/59") {
                return Err(ProtocolError.into());
            }
            Outcome::Refused(if cur.flag()? {
                Some(checked_text(&mut cur, &mut check)?)
            } else {
                None
            })
        }
        2 => {
            let deferred = cur.flag()?;
            let xml = checked_text(&mut cur, &mut check)?;
            let count = cur.count(16)?;
            let pictures = cur.0;
            for _ in 0..count {
                check()?;
                checked_text(&mut cur, &mut check)?;
                cur.blob()?;
            }
            Outcome::Present(Present {
                xml,
                deferred,
                pictures,
                count,
            })
        }
        _ => return Err(ProtocolError.into()),
    };
    cur.finish()?;
    check()?;
    Ok(Projection {
        serialization,
        outcome,
    })
}

fn checked_text<'a, E: From<ProtocolError>>(
    cursor: &mut Cursor<'a>,
    check: &mut impl FnMut() -> std::result::Result<(), E>,
) -> std::result::Result<&'a str, E> {
    let bytes = cursor.blob()?;
    let mut at = 0;
    while at < bytes.len() {
        check()?;
        let end = (at + (64 << 10)).min(bytes.len());
        let chunk = bytes.get(at..end).ok_or(ProtocolError)?;
        match std::str::from_utf8(chunk) {
            Ok(_) => at = end,
            Err(error) if error.error_len().is_none() && end < bytes.len() => {
                at += error.valid_up_to()
            }
            Err(_) => return Err(ProtocolError.into()),
        }
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ProtocolError)?;
    check()?;
    Ok(text)
}
