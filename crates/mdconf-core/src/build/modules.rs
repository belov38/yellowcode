use crate::modules::is_plain_module_text;
use std::borrow::Cow;
use v8cont::{
    Container, EntrySpec, StorageVer, V8Time, VecData, WriteOptions, write_container, write_exact,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModuleCheck {
    Equal,
    Differ(&'static str),
    NoText,
}

pub(crate) fn check_module(stored: &[u8], text: &[u8]) -> crate::Result<ModuleCheck> {
    let (inflated, compressed) = match open_stored(stored) {
        Ok(opened) => opened,
        Err(reason) => return Ok(ModuleCheck::Differ(reason)),
    };
    if is_plain_module_text(&inflated) {
        let encoded = if compressed {
            deflate_text(text)?
        } else {
            text.to_vec()
        };
        return Ok(if encoded == stored {
            ModuleCheck::Equal
        } else if text == inflated.as_ref() {
            ModuleCheck::Differ("deflate")
        } else {
            ModuleCheck::Differ("stored_bytes")
        });
    }
    let Ok(container) = Container::open(&inflated[..]) else {
        return Ok(ModuleCheck::Differ("not_a_container"));
    };
    let Ok(captured) = container.capture_layout() else {
        return Ok(ModuleCheck::Differ("layout_not_captured"));
    };
    if !container.entries().iter().any(|e| e.name == "text") {
        return Ok(ModuleCheck::NoText);
    }
    let mut specs = Vec::with_capacity(container.entries().len());
    let mut data = Vec::with_capacity(container.entries().len());
    for e in container.entries() {
        let bytes = if e.name == "text" {
            text.to_vec()
        } else {
            container.read_data(e)?
        };
        specs.push(EntrySpec {
            name: e.name.clone(),
            ctime: e.ctime,
            mtime: e.mtime,
            attrs: e.attrs,
            data_len: e.data_addr.map(|_| bytes.len() as u64),
        });
        data.push(bytes);
    }
    let mut out = Vec::with_capacity(inflated.len());
    if write_exact(
        &mut out,
        &captured.layout,
        &captured.residue,
        captured.placeholder.as_deref(),
        &specs,
        &mut VecData(&data),
    )
    .is_err()
    {
        return Ok(ModuleCheck::Differ("layout_does_not_fit"));
    }
    let deflated = zraw::deflate(&out, zraw::LEVEL_1C).map_err(|source| crate::Error::Deflate {
        entry: String::new(),
        source,
    })?;
    Ok(if deflated == stored {
        ModuleCheck::Equal
    } else if out == inflated.as_ref() {
        ModuleCheck::Differ("deflate")
    } else {
        ModuleCheck::Differ("stored_bytes")
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Probe {
    NoText(Vec<u8>),
    HasText,
    Unreadable(&'static str),
}

pub(crate) fn probe_module(stored: &[u8]) -> crate::Result<Probe> {
    let (inflated, _) = match open_stored(stored) {
        Ok(opened) => opened,
        Err(reason) => return Ok(Probe::Unreadable(reason)),
    };
    if is_plain_module_text(&inflated) {
        return Ok(Probe::HasText);
    }
    let has_text = match Container::open(&inflated[..]) {
        Ok(container) => container.entries().iter().any(|e| e.name == "text"),
        Err(_) => return Ok(Probe::Unreadable("not_a_container")),
    };
    Ok(if has_text {
        Probe::HasText
    } else {
        Probe::NoText(inflated.into_owned())
    })
}

fn format_error(what: &str) -> crate::Error {
    crate::Error::Format {
        entry: String::new(),
        what: what.into(),
    }
}

fn open_stored(stored: &[u8]) -> Result<(Cow<'_, [u8]>, bool), &'static str> {
    match zraw::try_inflate(stored, zraw::DEFAULT_LIMIT) {
        Some(inflated) => Ok((Cow::Owned(inflated), true)),
        None if is_plain_module_text(stored) => Ok((Cow::Borrowed(stored), false)),
        None => Err("not_deflate"),
    }
}

fn deflate_text(text: &[u8]) -> crate::Result<Vec<u8>> {
    zraw::deflate(text, zraw::LEVEL_1C).map_err(|source| crate::Error::Deflate {
        entry: String::new(),
        source,
    })
}

pub(crate) fn text_of(stored: &[u8]) -> crate::Result<Option<Vec<u8>>> {
    let (inflated, _) = open_stored(stored).map_err(format_error)?;
    if is_plain_module_text(&inflated) {
        return Ok(Some(inflated.into_owned()));
    }
    let container = Container::open(&inflated[..]).map_err(|_| format_error("not_a_container"))?;
    match container.find("text") {
        Some(e) => Ok(Some(container.read_data(e)?)),
        None => Ok(None),
    }
}

pub(crate) fn module_entry(
    base_stored: &[u8],
    text: &[u8],
    time: Option<V8Time>,
) -> crate::Result<Vec<u8>> {
    let (inflated, compressed) = open_stored(base_stored).map_err(format_error)?;
    if is_plain_module_text(&inflated) {
        if !is_plain_module_text(text) {
            return Err(format_error(
                "plain module replacement must be UTF-8 BOM text without NUL or brace payload",
            ));
        }
        return if compressed {
            deflate_text(text)
        } else {
            Ok(text.to_vec())
        };
    }
    let container = Container::open(&inflated[..]).map_err(|_| format_error("not_a_container"))?;
    if container.find("text").is_none() {
        return Err(format_error("no text entry"));
    }
    let mut specs = Vec::with_capacity(container.entries().len());
    let mut data = Vec::with_capacity(container.entries().len());
    for e in container.entries() {
        let bytes = if e.name == "text" {
            text.to_vec()
        } else {
            container.read_data(e)?
        };
        let (ctime, mtime) = time.map_or((e.ctime, e.mtime), |t| (t, t));
        specs.push(EntrySpec {
            name: e.name.clone(),
            ctime,
            mtime,
            attrs: e.attrs,
            data_len: e.data_addr.map(|_| bytes.len() as u64),
        });
        data.push(bytes);
    }
    let mut out = Vec::with_capacity(inflated.len() + text.len());
    match time {
        None => {
            let captured = container
                .capture_layout()
                .map_err(|_| format_error("layout_not_captured"))?;
            write_exact(
                &mut out,
                &captured.layout,
                &captured.residue,
                captured.placeholder.as_deref(),
                &specs,
                &mut VecData(&data),
            )
            .map_err(|_| format_error("layout_does_not_fit"))?;
        }
        Some(_) => {
            let opts = WriteOptions {
                format: container.format(),
                toc_page_min: 0,
                storage_ver: StorageVer::Fixed(container.header().storage_ver),
            };
            write_container(&mut out, &specs, &mut VecData(&data), &opts)?;
        }
    }
    zraw::deflate(&out, zraw::LEVEL_1C).map_err(|source| crate::Error::Deflate {
        entry: String::new(),
        source,
    })
}
