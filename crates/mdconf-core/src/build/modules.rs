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
    let Some(inflated) = zraw::try_inflate(stored, zraw::DEFAULT_LIMIT) else {
        return Ok(ModuleCheck::Differ("not_deflate"));
    };
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
    } else if out == inflated {
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
    let Some(inflated) = zraw::try_inflate(stored, zraw::DEFAULT_LIMIT) else {
        return Ok(Probe::Unreadable("not_deflate"));
    };
    let has_text = match Container::open(&inflated[..]) {
        Ok(container) => container.entries().iter().any(|e| e.name == "text"),
        Err(_) => return Ok(Probe::Unreadable("not_a_container")),
    };
    Ok(if has_text {
        Probe::HasText
    } else {
        Probe::NoText(inflated)
    })
}

fn format_error(what: &str) -> crate::Error {
    crate::Error::Format {
        entry: String::new(),
        what: what.into(),
    }
}

fn open_stored(stored: &[u8]) -> crate::Result<Vec<u8>> {
    zraw::try_inflate(stored, zraw::DEFAULT_LIMIT).ok_or_else(|| format_error("not_deflate"))
}

pub(crate) fn text_of(stored: &[u8]) -> crate::Result<Option<Vec<u8>>> {
    let inflated = open_stored(stored)?;
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
    let inflated = open_stored(base_stored)?;
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
