use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use quick_xml::{Reader, events::Event};

pub const MAX_FILE: usize = 4 * 1024 * 1024;
pub const MAX_PACKAGE: usize = 64 * 1024 * 1024;
const MAX_FILES: usize = 4096;

#[derive(Debug)]
pub struct Package {
    files: BTreeMap<String, Vec<u8>>,
    variants: Vec<Variant>,
}

#[derive(Debug)]
struct Variant {
    name: String,
    density: Option<String>,
    interface: Option<String>,
    themed: bool,
}

pub fn valid_path(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '\0'])
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

impl Package {
    pub fn read(&self, name: &str) -> Option<&[u8]> {
        self.files.get(name).map(Vec::as_slice)
    }

    pub fn taxi_variant(&self) -> Option<(&str, &[u8])> {
        self.variants
            .iter()
            .filter_map(|v| {
                if v.density.as_deref() != Some("ldpi") || v.themed {
                    return None;
                }
                let lower = v.name.to_ascii_lowercase();
                let rank = match (
                    v.interface.as_deref(),
                    lower.ends_with(".png"),
                    lower.ends_with(".svg"),
                ) {
                    (Some("taxi"), true, _) => 0,
                    (Some("taxi"), _, true) => 1,
                    (None, true, _) => 2,
                    (None, _, true) => 3,
                    (Some("version8_5"), _, true) => 4,
                    _ => return None,
                };
                Some((rank, v))
            })
            .min_by_key(|(rank, _)| *rank)
            .and_then(|(_, v)| Some((v.name.as_str(), self.read(&v.name)?)))
    }

    pub fn default_variant(&self) -> Option<(&str, &[u8])> {
        let ldpi = |v: &&Variant| v.density.as_deref() == Some("ldpi") && !v.themed;
        let v = (self.variants.iter().filter(ldpi))
            .find(|v| v.interface.as_deref() == Some("version8_2"))
            .or_else(|| {
                self.variants
                    .iter()
                    .filter(ldpi)
                    .find(|v| v.interface.is_none())
            })?;
        Some((v.name.as_str(), self.read(&v.name)?))
    }
}

pub fn read_package(bytes: &[u8]) -> Result<Package, String> {
    if bytes.len() > MAX_PACKAGE {
        return Err("picture package: compressed size limit".into());
    }
    let names = zip_directory_limit(bytes)?;
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("picture zip: {e}"))?;
    if archive.len() != names.len() {
        return Err("picture package: central directory member count mismatch".into());
    }
    let mut total = 0usize;
    for i in 0..archive.len() {
        let file = archive.by_index(i).map_err(|e| e.to_string())?;
        if names.get(i).copied() != Some(file.name()) || !valid_path(file.name()) || file.is_dir() {
            return Err("picture package: invalid member path".into());
        }
        let size = usize::try_from(file.size()).map_err(|_| "picture package: size overflow")?;
        if size > MAX_FILE {
            return Err("picture package: file size limit".into());
        }
        total = total
            .checked_add(size)
            .and_then(|n| n.checked_add(file.name().len()))
            .filter(|n| *n <= MAX_PACKAGE)
            .ok_or("picture package: total size limit")?;
    }
    let mut files = BTreeMap::new();
    for i in 0..archive.len() {
        let file = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().to_owned();
        if files.contains_key(&name) {
            return Err("picture package: duplicate member".into());
        }
        let size = usize::try_from(file.size()).map_err(|_| "picture package: size overflow")?;
        let mut data = Vec::new();
        data.try_reserve_exact(size + 1)
            .map_err(|_| "picture package: allocation failed")?;
        file.take((size + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|e| format!("picture package: {e}"))?;
        if data.len() != size {
            return Err("picture package: size mismatch".into());
        }
        files.insert(name, data);
    }
    let manifest = files
        .get("manifest.xml")
        .ok_or("picture package: no manifest.xml")?;
    let text =
        std::str::from_utf8(manifest).map_err(|_| "picture package: manifest is not UTF-8")?;
    let variants = variants(text.trim_start_matches('\u{feff}'))?;
    if variants.iter().any(|v| !files.contains_key(&v.name)) {
        return Err("picture package: missing variant member".into());
    }
    Ok(Package { files, variants })
}

fn variants(xml: &str) -> Result<Vec<Variant>, String> {
    let mut reader = Reader::from_str(xml);
    let mut depth = 0usize;
    let mut root = false;
    let mut variants = Vec::new();
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("picture manifest: {e}"))?
        {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                let empty = matches!(&event, Event::Empty(_));
                let e = match &event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => return Err("picture manifest: unexpected event".into()),
                };
                if depth == 0 {
                    if root || e.name().as_ref() != b"Picture" {
                        return Err("picture manifest: expected Picture root".into());
                    }
                    root = true;
                } else if depth == 1 && e.name().as_ref() == b"PictureVariant" {
                    let mut attrs = BTreeMap::new();
                    for a in e.attributes() {
                        let a = a.map_err(|e| e.to_string())?;
                        let key = std::str::from_utf8(a.key.as_ref())
                            .map_err(|_| "picture manifest: attribute name")?
                            .to_owned();
                        let value = a
                            .decode_and_unescape_value(reader.decoder())
                            .map_err(|e| e.to_string())?
                            .into_owned();
                        attrs.insert(key, value);
                    }
                    let name = attrs
                        .remove("name")
                        .filter(|s| valid_path(s))
                        .ok_or("picture manifest: invalid variant name")?;
                    variants.push(Variant {
                        name,
                        density: attrs.remove("screenDensity"),
                        interface: attrs.remove("interfaceVariant"),
                        themed: attrs.contains_key("theme"),
                    });
                } else {
                    return Err("picture manifest: unexpected element".into());
                }
                if !empty {
                    depth += 1;
                }
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("picture manifest: unexpected closing tag")?;
            }
            Event::CData(_) => return Err("picture manifest: CDATA unsupported".into()),
            Event::DocType(_) => return Err("picture manifest: DTD unsupported".into()),
            Event::Text(e) if !e.as_ref().iter().all(u8::is_ascii_whitespace) => {
                return Err("picture manifest: unexpected text".into());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !root || depth != 0 || variants.is_empty() {
        return Err("picture manifest: incomplete or empty".into());
    }
    Ok(variants)
}

fn zip_directory_limit(bytes: &[u8]) -> Result<Vec<&str>, String> {
    let start = bytes.len().saturating_sub(22 + 65535);
    let end = bytes
        .len()
        .checked_sub(22)
        .ok_or("picture zip: short input")?;
    let word = |offset: usize| -> Result<u16, String> {
        let b = bytes
            .get(offset..offset + 2)
            .ok_or("picture zip: truncated directory")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    };
    let dword = |offset: usize| -> Result<u32, String> {
        let b = bytes
            .get(
                offset
                    ..offset
                        .checked_add(4)
                        .ok_or("picture zip: offset overflow")?,
            )
            .ok_or("picture zip: truncated directory")?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    for at in (start..=end).rev() {
        if bytes.get(at..at + 4) != Some(b"PK\x05\x06") {
            continue;
        }
        if at + 22 + usize::from(word(at + 20)?) != bytes.len() {
            continue;
        }
        if bytes.get(at.saturating_sub(20)..at.saturating_sub(16)) == Some(b"PK\x06\x07")
            || bytes.get(at + 12..at + 16) == Some(&[255; 4])
            || bytes.get(at + 16..at + 20) == Some(&[255; 4])
        {
            return Err("picture zip: ZIP64 unsupported".into());
        }
        let count = usize::from(word(at + 10)?);
        if word(at + 4)? != 0
            || word(at + 6)? != 0
            || word(at + 8)? as usize != count
            || count > MAX_FILES
        {
            return Err("picture zip: unsupported directory or entry count limit".into());
        }
        let mut pos =
            usize::try_from(dword(at + 16)?).map_err(|_| "picture zip: offset overflow")?;
        let size = usize::try_from(dword(at + 12)?).map_err(|_| "picture zip: size overflow")?;
        let limit = pos
            .checked_add(size)
            .filter(|end| *end == at)
            .ok_or("picture zip: invalid directory extent")?;
        let mut names = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..count {
            let header_end = pos
                .checked_add(46)
                .filter(|end| *end <= limit)
                .ok_or("picture zip: truncated central header")?;
            if bytes.get(pos..pos + 4) != Some(b"PK\x01\x02") || word(pos + 34)? != 0 {
                return Err("picture zip: invalid central header".into());
            }
            let name_end = header_end
                .checked_add(usize::from(word(pos + 28)?))
                .filter(|end| *end <= limit)
                .ok_or("picture zip: truncated member name")?;
            let name = std::str::from_utf8(
                bytes
                    .get(header_end..name_end)
                    .ok_or("picture zip: truncated member name")?,
            )
            .map_err(|_| "picture zip: member name is not UTF-8")?;
            if !valid_path(name) {
                return Err("picture zip: invalid member path".into());
            }
            if !seen.insert(name) {
                return Err("picture package: duplicate member".into());
            }
            names.push(name);
            pos = name_end
                .checked_add(usize::from(word(pos + 30)?))
                .and_then(|end| end.checked_add(usize::from(word(pos + 32).ok()?)))
                .filter(|end| *end <= limit)
                .ok_or("picture zip: truncated central entry")?;
        }
        if pos != limit {
            return Err("picture zip: central directory count mismatch".into());
        }
        return Ok(names);
    }
    Err("picture zip: no end of central directory".into())
}
