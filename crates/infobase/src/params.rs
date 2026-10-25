use ibschema::dbnames::DbNames;
use ibschema::values::Uuid16;
use rand::Rng;

use crate::error::{Error, Result};
use crate::seed::seed;

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamsValues {
    pub db_u1: Uuid16,
    pub db_u2: Uuid16,
    pub n: u64,
    pub log_uuid: Uuid16,
    pub version_uuid: Uuid16,
}

impl ParamsValues {
    pub fn random_empty_base() -> ParamsValues {
        ParamsValues {
            db_u1: Uuid16::NIL,
            db_u2: Uuid16::NIL,
            n: rand::rng().random(),
            log_uuid: random_uuid(),
            version_uuid: random_uuid(),
        }
    }
}

pub fn random_uuid() -> Uuid16 {
    let mut b: [u8; 16] = rand::rng().random();
    if let Some(x) = b.get_mut(6) {
        *x = (*x & 0x0F) | 0x40;
    }
    if let Some(x) = b.get_mut(8) {
        *x = (*x & 0x3F) | 0x80;
    }
    Uuid16::from_bytes(b)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamsFile {
    pub name: String,
    pub stored: Vec<u8>,
}

fn substitute(file: &str, template: &str, values: &[(&str, String)]) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        let (head, tail) = rest.split_at(i);
        out.push_str(head);
        let name_len = tail
            .get(1..)
            .and_then(|t| t.find('}'))
            .filter(|&n| n > 0)
            .filter(|&n| {
                tail.get(1..=n).is_some_and(|s| {
                    s.starts_with(|c: char| c.is_ascii_lowercase())
                        && s.chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                })
            });
        match name_len {
            Some(n) => {
                let key = tail.get(..n + 2).unwrap_or_default();
                let (_, value) = values.iter().find(|(k, _)| *k == key).ok_or_else(|| {
                    Error::Inconsistent(format!("PARAMS {file}: unknown placeholder {key}"))
                })?;
                out.push_str(value);
                rest = tail.get(n + 2..).unwrap_or_default();
            }
            None => {
                out.push('{');
                rest = tail.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

pub fn empty_base_files(
    v: &ParamsValues,
    names: &DbNames,
    locale: &str,
) -> Result<Vec<ParamsFile>> {
    let values: Vec<(&str, String)> = vec![
        ("{locale}", format!("\"{}\"", locale.replace('"', "\"\""))),
        ("{db_u1}", v.db_u1.to_string()),
        ("{db_u2}", v.db_u2.to_string()),
        ("{n}", v.n.to_string()),
        ("{log_uuid}", v.log_uuid.to_string()),
        ("{version_uuid}", v.version_uuid.to_string()),
    ];
    let mut files = Vec::new();
    for t in &seed().params {
        let stored = if t.deflate {
            zraw::deflate(&names.to_text(), zraw::LEVEL_1C)
                .map_err(|e| Error::Inconsistent(format!("PARAMS {}: deflate: {e}", t.name)))?
        } else {
            let mut b = BOM.to_vec();
            b.extend_from_slice(substitute(&t.name, &t.text, &values)?.as_bytes());
            b
        };
        files.push(ParamsFile {
            name: t.name.clone(),
            stored,
        });
    }
    Ok(files)
}
