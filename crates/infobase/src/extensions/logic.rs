use super::cas::CasSource;
use super::zipped::{ZippedInfo, zipped_info};
use brace::{Guid, Node};
use mdconf::EntrySource;
use mdconf::extension_header::ObjectBelonging;
use mdconf::extension_objects::ModuleReadFailure;
use mdconf::extension_reader::ExtensionRoot;
use memory_budget::{Category, Limits, MemoryBudget};
use onecd::{BlobVal, Value};
use std::path::Path;

const BUDGET: u64 = 512 << 20;
const MAX_EXTENSIONS: usize = super::MAX_ITEMS;
const MAX_INTERCEPTS: usize = 65_536;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtensionLogic {
    pub extensions: Vec<ExtensionFacts>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionFacts {
    pub name: String,
    pub intercepts: Vec<Intercept>,
    pub subscriptions: Vec<String>,
    pub unreadable: Option<String>,
    pub row: Result<ExtensionRow, String>,
    pub info: Result<ZippedInfo, String>,
    pub version: Result<String, String>,
    pub structure: Result<ExtensionStructure, String>,
}

impl Default for ExtensionFacts {
    fn default() -> Self {
        fn not_read<T>() -> Result<T, String> {
            Err("not read".to_string())
        }
        ExtensionFacts {
            name: String::new(),
            intercepts: Vec::new(),
            subscriptions: Vec::new(),
            unreadable: None,
            row: not_read(),
            info: not_read(),
            version: not_read(),
            structure: not_read(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionRow {
    pub id: Guid,
    pub order: i64,
    pub purpose_code: i64,
    pub scope_code: i64,
    pub distributed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtensionStructure {
    pub own: Vec<Guid>,
    pub changed: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intercept {
    pub module: String,
    pub annotation: &'static str,
    pub method: String,
    pub target: String,
}

impl ExtensionLogic {
    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
    }

    pub fn read(path: &Path) -> ExtensionLogic {
        match read_all(path) {
            Ok(logic) => logic,
            Err(why) => ExtensionLogic {
                extensions: vec![ExtensionFacts {
                    unreadable: Some(why),
                    ..Default::default()
                }],
            },
        }
    }
}

fn read_all(path: &Path) -> Result<ExtensionLogic, String> {
    let budget = MemoryBudget::new(Limits::new(BUDGET));
    let db = onecd::Database::open(
        path,
        onecd::OpenOptions {
            memory_budget: Some(budget.clone()),
            locking: onecd::Locking::None,
            ..onecd::OpenOptions::read_only()
        },
    )
    .map_err(|e| format!("the base: {e}"))?;
    let txn = db.read();
    let Some(info) = txn
        .find_table("_ExtensionsInfo")
        .map_err(|e| format!("_ExtensionsInfo: {e}"))?
    else {
        return Ok(ExtensionLogic::default());
    };
    let mut rows = Vec::new();
    for slot in 1..info.num_slots() {
        let Some(row) = info
            .get_managed(slot, &budget, Category::Metadata, super::ROW_BYTES)
            .map_err(|e| format!("_ExtensionsInfo: {e}"))?
        else {
            continue;
        };
        if rows.len() == MAX_EXTENSIONS {
            return Err(format!("more than {MAX_EXTENSIONS} extensions"));
        }
        rows.push((row_columns(&row), row_facts(&info, &row, &budget)));
    }
    let mut extensions = Vec::with_capacity(rows.len());
    for row in rows {
        let (columns, row) = row;
        let mut facts = match row {
            Ok((name, info, Some(generation))) => {
                let mut facts = extension(&txn, &budget, name, generation);
                facts.info = info;
                facts
            }
            Ok((name, info, None)) => ExtensionFacts {
                name,
                unreadable: Some("no applied copy in ConfigCAS".into()),
                info,
                ..Default::default()
            },
            Err((name, why)) => ExtensionFacts {
                name,
                unreadable: Some(why.clone()),
                info: Err(why),
                ..Default::default()
            },
        };
        facts.row = columns;
        extensions.push(facts);
    }
    Ok(ExtensionLogic { extensions })
}

type RowFacts = Result<(String, Result<ZippedInfo, String>, Option<[u8; 20]>), (String, String)>;

fn integer(value: Option<&Value>) -> Option<i64> {
    let Some(Value::Numeric(n)) = value else {
        return None;
    };
    if n.scale() != 0 {
        return None;
    }
    let abs = n
        .digits()
        .iter()
        .try_fold(0i64, |n, d| n.checked_mul(10)?.checked_add(i64::from(*d)))?;
    Some(if n.is_negative() { -abs } else { abs })
}

fn row_columns(row: &[Value]) -> Result<ExtensionRow, String> {
    let id = match row.first() {
        Some(Value::Binary(id)) => <[u8; 16]>::try_from(id.as_slice())
            .map(|id| ibschema::values::RefId(id).to_uuid())
            .map_err(|_| "the ID has another length".to_string())?,
        _ => return Err("the ID has another type".into()),
    };
    let number =
        |at: usize, what: &str| integer(row.get(at)).ok_or(format!("{what} is not a whole number"));
    let distributed = match row.get(8) {
        Some(Value::Bool(b)) => *b,
        _ => return Err("the distributed flag has another type".into()),
    };
    Ok(ExtensionRow {
        id,
        order: number(1, "the order")?,
        purpose_code: number(4, "the purpose")?,
        scope_code: number(5, "the scope")?,
        distributed,
    })
}

fn row_facts(table: &onecd::TableRead<'_>, row: &[Value], budget: &MemoryBudget) -> RowFacts {
    let name = match row.get(2) {
        Some(Value::VarChar(name)) => name.clone(),
        _ => String::new(),
    };
    let bytes = match row.get(6) {
        Some(Value::Image(BlobVal::Data(bytes))) => bytes.clone(),
        Some(Value::Image(BlobVal::Ref(reference))) => {
            if reference.len as usize > super::GENERATION_BYTES {
                return Err((name, "the zipped information is too long".into()));
            }
            match table.blob_bytes_managed(
                *reference,
                budget,
                Category::ReaderWorking,
                super::GENERATION_BYTES as u64 + 256,
            ) {
                Ok(bytes) => bytes.to_vec(),
                Err(e) => return Err((name, format!("the zipped information: {e}"))),
            }
        }
        _ => return Err((name, "the zipped information has another type".into())),
    };
    match super::generation_key(&bytes, budget) {
        Ok(key) => {
            let info =
                zipped_info(&bytes, budget).map_err(|e| format!("the zipped information: {e}"));
            Ok((name, info, key))
        }
        Err(e) => Err((name, format!("the zipped information: {e}"))),
    }
}

fn extension(
    txn: &onecd::ReadTxn,
    budget: &MemoryBudget,
    name: String,
    generation: [u8; 20],
) -> ExtensionFacts {
    let mut facts = ExtensionFacts {
        name,
        ..Default::default()
    };
    if let Err(why) = collect(txn, budget, generation, &mut facts) {
        facts.unreadable = Some(why);
    }
    facts
}

fn collect(
    txn: &onecd::ReadTxn,
    budget: &MemoryBudget,
    generation: [u8; 20],
    facts: &mut ExtensionFacts,
) -> Result<(), String> {
    let source = CasSource::open(txn, budget, generation).map_err(|e| format!("ConfigCAS: {e}"))?;
    let root = ExtensionRoot::read_installed(&source, budget, |manifest| {
        source.prepare(manifest);
        Ok(())
    })
    .map_err(|e| format!("the root of the extension: {e}"))?
    .ok_or("the applied copy has no configinfo")?;
    facts.version = version(&source, budget, root.manifest().root_id);
    let mut tops = Vec::new();
    let mut objects = root
        .objects(&source)
        .map_err(|e| format!("the objects of the extension: {e}"))?;
    while let Some(object) = objects
        .next()
        .map_err(|e| format!("an object of the extension: {e}"))?
    {
        let header = object
            .installed_header()
            .map_err(|e| format!("an object header of the extension: {e}"))?;
        let kind = object.reference.kind.unwrap_or("Unknown");
        let name = header.name.to_string();
        tops.push(Top {
            entry: object.reference.entry_id,
            belonging: header.belonging(),
            kind: kind.to_string(),
            name: name.clone(),
        });
        if kind == "EventSubscription" {
            facts.subscriptions.push(name.clone());
        }
        for module in object
            .modules()
            .map_err(|e| format!("{kind}.{name}: {e}"))?
        {
            let module = module.map_err(|e| format!("{kind}.{name}: {e}"))?;
            let path = format!("{kind}.{name}.{}", module.name());
            let text = match module.read() {
                Ok(text) => text,
                Err(e) if matches!(e.source, ModuleReadFailure::NoSource) => {
                    return Err(format!("{path}: the module keeps no source text"));
                }
                Err(e) => return Err(format!("{path}: {e}")),
            };
            let text = std::str::from_utf8(text.as_ref())
                .map_err(|_| format!("{path}: the module text is not UTF-8"))?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(text);
            for (annotation, method, target) in
                scan_intercepts(text).map_err(|why| format!("{path}: {why}"))?
            {
                if facts.intercepts.len() == MAX_INTERCEPTS {
                    return Err(format!("more than {MAX_INTERCEPTS} intercepts"));
                }
                facts.intercepts.push(Intercept {
                    module: path.clone(),
                    annotation,
                    method,
                    target,
                });
            }
        }
    }
    drop(objects);
    let names: Vec<String> = root
        .manifest()
        .entries
        .iter()
        .map(|e| e.name.to_ascii_lowercase())
        .collect();
    let read = |name: &str| entry(&source, budget, name);
    facts.structure = structure(&read, budget, root.manifest().root_id, &names, &tops);
    Ok(())
}

struct Top {
    entry: Guid,
    belonging: ObjectBelonging,
    kind: String,
    name: String,
}

const ENTRY_BYTES: u64 = 64 << 20;
const MAX_ENTRIES: usize = 65_536;

fn entry(
    source: &CasSource<'_>,
    budget: &MemoryBudget,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    let Some(stored) = source
        .stored_owned_bounded(name, ENTRY_BYTES as usize)
        .map_err(|e| format!("{name}: {e}"))?
    else {
        return Ok(None);
    };
    zraw::inflate_budgeted(&stored, ENTRY_BYTES, budget, Category::ReaderWorking)
        .map(|bytes| Some(bytes.to_vec()))
        .map_err(|e| format!("{name}: {e}"))
}

type ReadEntry<'a> = dyn Fn(&str) -> Result<Option<Vec<u8>>, String> + 'a;

fn tree(
    read: &ReadEntry<'_>,
    budget: &MemoryBudget,
    name: &str,
) -> Result<memory_budget::Charged<Node>, String> {
    let bytes = read(name)?.ok_or(format!("{name}: no such entry"))?;
    brace::parse_bytes_budgeted(&bytes, budget, Category::ReaderWorking)
        .map_err(|e| format!("{name}: {e}"))
}

fn version(source: &CasSource<'_>, budget: &MemoryBudget, root_id: Guid) -> Result<String, String> {
    let read = |name: &str| entry(source, budget, name);
    version_of_root(&*tree(&read, budget, &root_id.to_string())?)
}

fn version_of_root(root: &Node) -> Result<String, String> {
    let body = root
        .get(&[3, 1, 1])
        .ok_or("the root has no body of the configuration")?;
    let len = body.as_list().map_or(0, <[Node]>::len);
    if !matches!(
        (body.at(0).and_then(Node::as_atom), len),
        (Some("53"), 49) | (Some("55"), 51) | (Some("76"), 77)
    ) {
        return Err("the body of the root has another version".into());
    }
    let mdschema::StaticConfigPath::Body(path) = mdschema::static_schema()
        .config_prop("Version")
        .and_then(|p| p.path())
        .ok_or("the schema has no Version")?
    else {
        return Err("the schema has no Version in the body".into());
    };
    let at: Vec<usize> = path.iter().collect();
    match body.get(&at).and_then(Node::as_str) {
        Some("") => Ok(String::new()),
        Some(_) => Err("the position of Version in this body is not confirmed".into()),
        None => Err("Version is not a string".into()),
    }
}

fn header(node: &Node) -> Option<(Guid, bool)> {
    let [
        version,
        identity,
        name,
        synonym,
        comment,
        belonging,
        count,
        ..,
    ] = node.as_list()?
    else {
        return None;
    };
    version.as_u32()?;
    count.as_u32()?;
    let [a, b, id] = identity.as_list()? else {
        return None;
    };
    a.as_u32()?;
    b.as_u32()?;
    if name.as_str().is_none()
        || !synonym.as_list()?.iter().all(|n| !n.is_list())
        || comment.as_str().is_none()
    {
        return None;
    }
    Some((id.as_guid()?, belonging.as_bool()?))
}

type Scanned = (Vec<(Guid, bool)>, std::collections::HashSet<Guid>);

fn scan_tree(root: &Node) -> Scanned {
    let mut headers = Vec::new();
    let mut named = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if let Some(h) = header(node) {
            headers.push(h);
        }
        match node.as_list() {
            Some(items) => stack.extend(items.iter().rev()),
            None => {
                if let Some(g) = node.as_guid() {
                    named.insert(g);
                }
            }
        }
    }
    (headers, named)
}

fn has_intercept(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars.get(i) != Some(&'&') {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while chars.get(end).is_some_and(|c| is_ident_char(*c)) {
            end += 1;
        }
        let word = fold(
            &chars
                .get(start..end)
                .unwrap_or_default()
                .iter()
                .collect::<String>(),
        );
        i = end;
        if !KINDS.iter().any(|(k, _)| *k == word) {
            continue;
        }
        let mut j = end;
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        if chars.get(j) != Some(&'(') {
            continue;
        }
        j += 1;
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        if chars.get(j) == Some(&'"') {
            return true;
        }
    }
    false
}

fn structure(
    read: &ReadEntry<'_>,
    budget: &MemoryBudget,
    root_id: Guid,
    names: &[String],
    tops: &[Top],
) -> Result<ExtensionStructure, String> {
    use std::collections::{BTreeSet, HashMap, HashSet};
    if names.len() > MAX_ENTRIES {
        return Err(format!("more than {MAX_ENTRIES} entries"));
    }
    let mut files: HashMap<Guid, Scanned> = HashMap::new();
    let mut parts: HashMap<Guid, Vec<&str>> = HashMap::new();
    for name in names {
        match name.split_once('.') {
            None => {
                let Some(id) = Guid::parse(name) else {
                    continue;
                };
                if id == root_id {
                    continue;
                }
                files.insert(id, scan_tree(&*tree(read, budget, name)?));
            }
            Some((id, _)) => {
                if let Some(id) = Guid::parse(id) {
                    parts.entry(id).or_default().push(name);
                }
            }
        }
    }
    let mut own: BTreeSet<Guid> = BTreeSet::new();
    for (headers, _) in files.values() {
        own.extend(
            headers
                .iter()
                .filter(|(_, borrowed)| !borrowed)
                .map(|(id, _)| *id),
        );
    }
    let mut changed = Vec::new();
    for top in tops {
        match top.belonging {
            ObjectBelonging::Own => continue,
            ObjectBelonging::Adopted => {}
            ObjectBelonging::Unknown => {
                return Err(format!("the object {} has another belonging", top.entry));
            }
        }
        let mut family = vec![top.entry];
        let mut seen: HashSet<Guid> = HashSet::from([top.entry]);
        let mut at = 0;
        while let Some(&id) = family.get(at) {
            at += 1;
            let Some((_, named)) = files.get(&id) else {
                return Err(format!("the object entry {id} is not in the manifest"));
            };
            let mut next: Vec<Guid> = named
                .iter()
                .copied()
                .filter(|g| files.contains_key(g) && !seen.contains(g))
                .collect();
            next.sort_unstable();
            for g in next {
                seen.insert(g);
                family.push(g);
            }
        }
        let own_descendant = family.iter().any(|id| {
            files.get(id).is_some_and(|(headers, _)| {
                headers
                    .iter()
                    .enumerate()
                    .any(|(i, (_, borrowed))| !borrowed && !(*id == top.entry && i == 0))
            })
        });
        let mut intercept = false;
        for id in &family {
            for name in parts.get(id).into_iter().flatten() {
                let bytes = read(name)?.ok_or(format!("{name}: no such entry"))?;
                if has_intercept(&String::from_utf8_lossy(&bytes)) {
                    intercept = true;
                }
            }
        }
        if own_descendant || intercept {
            changed.push((top.kind.clone(), top.name.clone()));
        }
    }
    Ok(ExtensionStructure {
        own: own.into_iter().collect(),
        changed,
    })
}

const KINDS: [(&str, &str); 8] = [
    ("перед", "&Перед"),
    ("before", "&Перед"),
    ("после", "&После"),
    ("after", "&После"),
    ("вместо", "&Вместо"),
    ("around", "&Вместо"),
    ("изменениеиконтроль", "&ИзменениеИКонтроль"),
    ("changeandvalidate", "&ИзменениеИКонтроль"),
];

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

fn fold(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

pub fn scan_intercepts(text: &str) -> Result<Vec<(&'static str, String, String)>, String> {
    let chars: Vec<char> = text.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let span = |a: usize, b: usize| {
        chars
            .iter()
            .skip(a)
            .take(b.saturating_sub(a))
            .collect::<String>()
    };
    let line_of = |i: usize| chars.iter().take(i).filter(|c| **c == '\n').count() + 1;
    let skip_space = |mut j: usize| {
        while at(j).is_some_and(char::is_whitespace) {
            j += 1;
        }
        j
    };
    let skip_ident = |mut j: usize| {
        while at(j).is_some_and(is_ident_char) {
            j += 1;
        }
        j
    };
    let mut out = Vec::new();
    let mut pending: Option<(&'static str, String)> = None;
    let mut i = 0;
    while let Some(c) = at(i) {
        if c == '/' && at(i + 1) == Some('/') {
            while at(i).is_some_and(|c| c != '\n') {
                i += 1;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            let close = c;
            i += 1;
            loop {
                match at(i) {
                    None => return Err(format!("line {}: an unclosed literal", line_of(i))),
                    Some(x) if x == close => {
                        if close == '"' && at(i + 1) == Some('"') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            continue;
        }
        if c == '&' {
            let start = i + 1;
            let end = skip_ident(start);
            let word = fold(&span(start, end));
            i = end;
            let Some(&(_, annotation)) = KINDS.iter().find(|(k, _)| *k == word) else {
                continue;
            };
            let line = line_of(start);
            let missing = || format!("line {line}: {annotation} without a method name");
            let mut j = skip_space(i);
            if at(j) != Some('(') {
                return Err(missing());
            }
            j = skip_space(j + 1);
            if at(j) != Some('"') {
                return Err(missing());
            }
            let name_start = j + 1;
            let mut k = name_start;
            while at(k).is_some_and(|c| c != '"' && c != '\n') {
                k += 1;
            }
            if at(k) != Some('"') {
                return Err(missing());
            }
            let target = span(name_start, k);
            if target.is_empty() || !target.chars().all(is_ident_char) {
                return Err(format!("line {line}: {annotation} names no method"));
            }
            j = skip_space(k + 1);
            if at(j) != Some(')') {
                return Err(missing());
            }
            if pending.is_some() {
                return Err(format!("line {line}: two intercepts of one method"));
            }
            pending = Some((annotation, target));
            i = j + 1;
            continue;
        }
        let starts_word = i == 0 || !at(i - 1).is_some_and(is_ident_char);
        if is_ident_char(c) && starts_word {
            let start = i;
            i = skip_ident(i);
            let word = fold(&span(start, i));
            if matches!(
                word.as_str(),
                "процедура" | "функция" | "procedure" | "function"
            ) && let Some((annotation, target)) = pending.take()
            {
                let name_start = skip_space(i);
                let name_end = skip_ident(name_start);
                if name_start == name_end {
                    return Err(format!(
                        "line {}: {annotation} is not followed by a method",
                        line_of(start)
                    ));
                }
                out.push((annotation, span(name_start, name_end), target));
                i = name_end;
            }
            continue;
        }
        i += 1;
    }
    if let Some((annotation, _)) = pending {
        return Err(format!("{annotation} is not followed by a method"));
    }
    Ok(out)
}
