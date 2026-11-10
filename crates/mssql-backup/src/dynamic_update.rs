use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io,
};

pub const MARKER_NAME: &str = "DynamicallyUpdated";

pub const PROVEN_GENERATIONS: usize = 12;

const COPY_INFIX: &str = "_dynupdate_";
const VERSIONS: &str = "versions";
const DELETED: &str = "deleted";
const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const GUID_LEN: usize = 36;
const MAX_MARKER_BYTES: u64 = 64 * 1024;
const MAX_DELETED_LIST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DELETED_TEXT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub parts: usize,
    pub stored_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub meta: usize,
    pub data: usize,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    pub generations: usize,
    pub replaced: usize,
    pub added: usize,
    pub deleted: usize,
}

#[derive(Debug)]
pub struct Applied {
    pub entries: Vec<Placed>,
    pub summary: Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    Marker,
    EmptyGeneration,
    UnlistedGeneration,
    DeletedList,
    TooManyGenerations,
    GenerationShape,
    Interplay,
    DeletedMissing,
    CopyName,
    MultiPartCopy,
    VersionsMissing,
}

impl Refusal {
    pub const ALL: [Refusal; 11] = [
        Refusal::Marker,
        Refusal::EmptyGeneration,
        Refusal::UnlistedGeneration,
        Refusal::DeletedList,
        Refusal::TooManyGenerations,
        Refusal::GenerationShape,
        Refusal::Interplay,
        Refusal::DeletedMissing,
        Refusal::CopyName,
        Refusal::MultiPartCopy,
        Refusal::VersionsMissing,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Refusal::Marker => "marker",
            Refusal::EmptyGeneration => "empty_generation",
            Refusal::UnlistedGeneration => "unlisted_generation",
            Refusal::DeletedList => "deleted_list",
            Refusal::TooManyGenerations => "too_many_generations",
            Refusal::GenerationShape => "generation_shape",
            Refusal::Interplay => "interplay",
            Refusal::DeletedMissing => "deleted_missing",
            Refusal::CopyName => "copy_name",
            Refusal::MultiPartCopy => "multipart_copy",
            Refusal::VersionsMissing => "versions_missing",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "dynamic-update leftovers of the CONFIG cannot be applied [{}]: {detail}; use --raw to get the CONFIG as stored",
        .refusal.code()
    )]
    Refused { refusal: Refusal, detail: String },
    #[error("reading an entry of the dynamic update: {0}")]
    Io(#[from] io::Error),
    #[error("two CONFIG entries have the same name")]
    Duplicate,
}

fn refuse(refusal: Refusal, detail: impl Into<String>) -> Error {
    Error::Refused {
        refusal,
        detail: detail.into(),
    }
}

pub fn plan(
    entries: &[Entry<'_>],
    read: &mut dyn FnMut(usize) -> io::Result<Vec<u8>>,
) -> Result<Option<Applied>, Error> {
    let Some((marker_at, marker)) = entries
        .iter()
        .enumerate()
        .find(|(_, entry)| entry.name == MARKER_NAME)
    else {
        return Ok(None);
    };
    let guids = read_marker(marker_at, marker, read)?;
    let classified = classify(entries, marker_at, guids)?;
    check_generations(&classified.generations)?;
    let deleted = read_deleted_lists(&classified.generations, read)?;
    simulate(&classified, &deleted).map(Some)
}

fn read_marker(
    index: usize,
    marker: &Entry<'_>,
    read: &mut dyn FnMut(usize) -> io::Result<Vec<u8>>,
) -> Result<Vec<String>, Error> {
    if marker.stored_size > MAX_MARKER_BYTES {
        return Err(refuse(
            Refusal::Marker,
            format!(
                "the entry is {} bytes, more than the {MAX_MARKER_BYTES} a marker can need",
                marker.stored_size
            ),
        ));
    }
    let guids = parse_marker(&read(index)?).map_err(|detail| refuse(Refusal::Marker, detail))?;
    if guids.len() > PROVEN_GENERATIONS {
        return Err(refuse(
            Refusal::TooManyGenerations,
            format!(
                "the marker lists {} generations; the rule was verified on up to {PROVEN_GENERATIONS}",
                guids.len()
            ),
        ));
    }
    Ok(guids)
}

fn is_guid(text: &str) -> bool {
    text.len() == GUID_LEN
        && text.bytes().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn parse_decimal(text: &str) -> Option<usize> {
    let plain = !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    plain.then(|| text.parse().ok()).flatten()
}

fn parse_marker(bytes: &[u8]) -> Result<Vec<String>, String> {
    const SHAPE: &str = "the entry is not a byte order mark followed by {1,N,G1..GN}";
    let text = bytes
        .strip_prefix(&BOM)
        .and_then(|rest| std::str::from_utf8(rest).ok())
        .and_then(|text| text.strip_prefix("{1,"))
        .and_then(|text| text.strip_suffix('}'))
        .ok_or_else(|| SHAPE.to_owned())?;
    let mut fields = text.split(',');
    let count = fields
        .next()
        .and_then(parse_decimal)
        .ok_or_else(|| SHAPE.to_owned())?;
    let listed: Vec<&str> = fields.collect();
    if !listed.iter().all(|field| is_guid(field)) {
        return Err(SHAPE.to_owned());
    }
    if listed.is_empty() || count == 0 {
        return Err("the marker lists no generation".to_owned());
    }
    if listed.len() != count {
        return Err(format!(
            "the marker says N = {count} and lists {} generations",
            listed.len()
        ));
    }
    if listed.iter().collect::<HashSet<_>>().len() != listed.len() {
        return Err("the marker lists a generation twice".to_owned());
    }
    Ok(listed.into_iter().map(str::to_owned).collect())
}

enum Target {
    Object(String),
    Versions,
    Deleted,
}

enum Parsed<'a> {
    Ordinary,
    Malformed(&'static str),
    Copy { generation: &'a str, target: Target },
}

fn is_stream_suffix(text: &str) -> bool {
    text.strip_prefix('.').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn is_object_name(text: &str) -> bool {
    match text.get(..GUID_LEN) {
        Some(guid) => {
            is_guid(guid)
                && text
                    .get(GUID_LEN..)
                    .is_some_and(|rest| rest.is_empty() || is_stream_suffix(rest))
        }
        None => false,
    }
}

fn parse_copy(name: &str) -> Parsed<'_> {
    let Some(at) = name.find(COPY_INFIX) else {
        return Parsed::Ordinary;
    };
    let (Some(base), Some(tail)) = (name.get(..at), name.get(at + COPY_INFIX.len()..)) else {
        return Parsed::Malformed("the name cannot be split");
    };
    let (Some(generation), Some(suffix)) = (tail.get(..GUID_LEN), tail.get(GUID_LEN..)) else {
        return Parsed::Malformed("the part after _dynupdate_ is shorter than a GUID");
    };
    if !is_guid(generation) {
        return Parsed::Malformed("the part after _dynupdate_ does not start with a GUID");
    }
    if !suffix.is_empty() && !is_stream_suffix(suffix) {
        return Parsed::Malformed("the generation is followed by something other than .k");
    }
    let target = if base == VERSIONS && suffix.is_empty() {
        Target::Versions
    } else if base == DELETED && suffix.is_empty() {
        Target::Deleted
    } else if is_guid(base) {
        Target::Object(format!("{base}{suffix}"))
    } else {
        return Parsed::Malformed("the base is not an object GUID, versions or deleted");
    };
    Parsed::Copy { generation, target }
}

struct ObjectCopy {
    index: usize,
    target: String,
}

struct Listed {
    index: usize,
    stored_size: u64,
}

struct Generation {
    guid: String,
    objects: Vec<ObjectCopy>,
    versions: Vec<usize>,
    deleted: Vec<Listed>,
}

struct Classified<'a> {
    generations: Vec<Generation>,
    ordinary: Vec<(usize, &'a str)>,
}

fn classify<'a>(
    entries: &[Entry<'a>],
    marker_at: usize,
    guids: Vec<String>,
) -> Result<Classified<'a>, Error> {
    let mut generations: Vec<Generation> = guids
        .into_iter()
        .map(|guid| Generation {
            guid,
            objects: Vec::new(),
            versions: Vec::new(),
            deleted: Vec::new(),
        })
        .collect();
    let listed = generations.len();
    let mut ordinary = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if index == marker_at {
            continue;
        }
        let (generation, target) = match parse_copy(entry.name) {
            Parsed::Ordinary => {
                ordinary.push((index, entry.name));
                continue;
            }
            Parsed::Malformed(reason) => {
                return Err(refuse(
                    Refusal::CopyName,
                    format!(
                        "an entry named like a copy (_dynupdate_) is not one that was verified: {reason}"
                    ),
                ));
            }
            Parsed::Copy { generation, target } => (generation, target),
        };
        let Some((slot, generation)) = generations
            .iter_mut()
            .enumerate()
            .find(|(_, candidate)| candidate.guid == generation)
        else {
            return Err(refuse(
                Refusal::UnlistedGeneration,
                format!(
                    "an entry carries a generation that the marker (it lists {listed}) does not"
                ),
            ));
        };
        if entry.parts > 1 {
            return Err(refuse(
                Refusal::MultiPartCopy,
                format!(
                    "generation {} of {listed}: a copy was assembled from {} SQL parts",
                    slot + 1,
                    entry.parts
                ),
            ));
        }
        match target {
            Target::Object(target) => generation.objects.push(ObjectCopy { index, target }),
            Target::Versions => generation.versions.push(index),
            Target::Deleted => generation.deleted.push(Listed {
                index,
                stored_size: entry.stored_size,
            }),
        }
    }
    Ok(Classified {
        generations,
        ordinary,
    })
}

fn check_generations(generations: &[Generation]) -> Result<(), Error> {
    let listed = generations.len();
    for (slot, generation) in generations.iter().enumerate() {
        let number = slot + 1;
        if generation.objects.len() + generation.versions.len() + generation.deleted.len() == 0 {
            return Err(refuse(
                Refusal::EmptyGeneration,
                format!("generation {number} of {listed}: no entry carries it"),
            ));
        }
        if generation.versions.len() > 1 {
            return Err(refuse(
                Refusal::GenerationShape,
                format!(
                    "generation {number} of {listed}: {} entries versions_dynupdate_<generation>, at most one is expected",
                    generation.versions.len()
                ),
            ));
        }
        if generation.deleted.len() > 1 {
            return Err(refuse(
                Refusal::GenerationShape,
                format!(
                    "generation {number} of {listed}: {} entries deleted_dynupdate_<generation>, at most one is expected",
                    generation.deleted.len()
                ),
            ));
        }
    }
    Ok(())
}

struct Mention {
    name: String,
    again: bool,
}

fn read_deleted_lists(
    generations: &[Generation],
    read: &mut dyn FnMut(usize) -> io::Result<Vec<u8>>,
) -> Result<Vec<Vec<Mention>>, Error> {
    let mut lists = Vec::with_capacity(generations.len());
    for (slot, generation) in generations.iter().enumerate() {
        let Some(listed) = generation.deleted.first() else {
            lists.push(Vec::new());
            continue;
        };
        let number = slot + 1;
        if listed.stored_size > MAX_DELETED_LIST_BYTES {
            return Err(refuse(
                Refusal::DeletedList,
                format!(
                    "generation {number}: the list is {} bytes, more than {MAX_DELETED_LIST_BYTES}",
                    listed.stored_size
                ),
            ));
        }
        let names = parse_deleted(&read(listed.index)?).map_err(|detail| {
            refuse(
                Refusal::DeletedList,
                format!("generation {number}: {detail}"),
            )
        })?;
        lists.push(names);
    }
    Ok(lists)
}

fn parse_deleted(packed: &[u8]) -> Result<Vec<Mention>, String> {
    const PAIRS: &str =
        "the list is not a count followed by \"name\",flag pairs with the flag 0 or 1";
    let text = zraw::inflate(packed, MAX_DELETED_TEXT_BYTES)
        .map_err(|error| format!("the list does not inflate as one raw deflate stream: {error}"))?;
    let text = text
        .strip_prefix(&BOM)
        .and_then(|rest| std::str::from_utf8(rest).ok())
        .ok_or_else(|| "the inflated list is not UTF-8 after a byte order mark".to_owned())?;
    let (count, mut rest) = text
        .split_once(',')
        .map_or((text, None), |(count, pairs)| (count, Some(pairs)));
    let count = parse_decimal(count).ok_or_else(|| PAIRS.to_owned())?;
    let mut names = Vec::new();
    while let Some(pairs) = rest {
        let (name, tail) = pairs
            .strip_prefix('"')
            .and_then(|after| after.split_once('"'))
            .ok_or_else(|| PAIRS.to_owned())?;
        let (again, tail) = match (tail.strip_prefix(",1"), tail.strip_prefix(",0")) {
            (Some(tail), _) => (false, tail),
            (None, Some(tail)) => (true, tail),
            (None, None) => return Err(PAIRS.to_owned()),
        };
        names.push((name, again));
        rest = match tail.strip_prefix(',') {
            Some(next) => Some(next),
            None if tail.is_empty() => None,
            None => return Err(PAIRS.to_owned()),
        };
    }
    if names.len() != count {
        return Err(format!(
            "the list says {count} names and holds {}",
            names.len()
        ));
    }
    if names.is_empty() {
        return Err("the list holds no name".to_owned());
    }
    if let Some(at) = names.iter().position(|(name, _)| !is_object_name(name)) {
        return Err(format!(
            "name {} of the list is not an object file name (a GUID or GUID.k)",
            at + 1
        ));
    }
    Ok(names
        .into_iter()
        .map(|(name, again)| Mention {
            name: name.to_owned(),
            again,
        })
        .collect())
}

struct Slot {
    meta: usize,
    data: usize,
    added: bool,
}

fn simulate<'a>(classified: &Classified<'a>, deleted: &[Vec<Mention>]) -> Result<Applied, Error> {
    let mut current: HashMap<Cow<'a, str>, Slot> =
        HashMap::with_capacity(classified.ordinary.len());
    for &(index, name) in &classified.ordinary {
        let slot = Slot {
            meta: index,
            data: index,
            added: false,
        };
        if current.insert(Cow::Borrowed(name), slot).is_some() {
            return Err(Error::Duplicate);
        }
    }
    let mut deleted_by: HashMap<&str, usize> = HashMap::new();
    let mut lowercase: Option<HashSet<String>> = None;
    let mut summary = Summary {
        generations: classified.generations.len(),
        ..Summary::default()
    };
    for (slot, generation) in classified.generations.iter().enumerate() {
        let number = slot + 1;
        let mut copied: HashSet<&str> = HashSet::new();
        let copies = generation
            .objects
            .iter()
            .map(|copy| (copy.target.as_str(), copy.index, false))
            .chain(
                generation
                    .versions
                    .iter()
                    .map(|&index| (VERSIONS, index, true)),
            );
        for (target, index, is_versions) in copies {
            if !copied.insert(target) {
                return Err(refuse(
                    Refusal::Interplay,
                    format!("generation {number}: two copies of one entry"),
                ));
            }
            if let Some(by) = deleted_by.get(target) {
                return Err(refuse(
                    Refusal::Interplay,
                    format!(
                        "generation {number} copies an entry that generation {by} listed as deleted"
                    ),
                ));
            }
            match current.get_mut(target) {
                Some(existing) => {
                    existing.data = index;
                    summary.replaced += 1;
                }
                None if is_versions => {
                    return Err(refuse(
                        Refusal::VersionsMissing,
                        format!("generation {number}: there is no versions entry to replace"),
                    ));
                }
                None => {
                    let known = lowercase.get_or_insert_with(|| {
                        classified
                            .ordinary
                            .iter()
                            .map(|(_, name)| name.to_ascii_lowercase())
                            .collect()
                    });
                    if !known.insert(target.to_ascii_lowercase()) {
                        return Err(refuse(
                            Refusal::CopyName,
                            format!(
                                "generation {number}: a copy would add an entry whose name differs from an existing one in case only"
                            ),
                        ));
                    }
                    current.insert(
                        Cow::Owned(target.to_owned()),
                        Slot {
                            meta: index,
                            data: index,
                            added: true,
                        },
                    );
                    summary.added += 1;
                }
            }
        }
        for (position, mention) in deleted.get(slot).into_iter().flatten().enumerate() {
            let name = mention.name.as_str();
            let position = position + 1;
            if copied.contains(name) {
                return Err(refuse(
                    Refusal::Interplay,
                    format!("generation {number} copies and deletes the same entry"),
                ));
            }
            if current.contains_key(name) {
                if mention.again {
                    return Err(refuse(
                        Refusal::DeletedList,
                        format!(
                            "generation {number}: name {position} of the deleted list has the flag 0 (a deletion listed again) but an entry of that name exists"
                        ),
                    ));
                }
                current.remove(name);
            } else if deleted_by.get(name).is_some_and(|&by| by < number) {
                if !mention.again {
                    return Err(refuse(
                        Refusal::DeletedList,
                        format!(
                            "generation {number}: name {position} of the deleted list has the flag 1 (a deletion of its own) but an earlier generation already deleted it"
                        ),
                    ));
                }
            } else {
                return Err(refuse(
                    Refusal::DeletedMissing,
                    format!(
                        "generation {number}: name {position} of the deleted list is not an entry at that point and no earlier generation deleted it"
                    ),
                ));
            }
            deleted_by.insert(name, number);
            summary.deleted += 1;
        }
    }
    let mut placed: Vec<(Cow<'a, str>, Placed)> = current
        .into_iter()
        .map(|(name, slot)| {
            let rename = slot.added.then(|| name.to_string());
            (
                name,
                Placed {
                    meta: slot.meta,
                    data: slot.data,
                    name: rename,
                },
            )
        })
        .collect();
    placed.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(Applied {
        entries: placed.into_iter().map(|(_, placed)| placed).collect(),
        summary,
    })
}
