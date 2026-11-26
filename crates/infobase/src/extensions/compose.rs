use super::cas::CasSource;
use crate::config::metered::model;
use crate::config::{ConfigTxnSource, OpenLease};
use crate::maybe_charged::MaybeCharged;
use crate::raw::managed::folder_file_data_in_budgeted;
use crate::raw::{RawIb, inflate_raw};
use ibschema::dbnames::{DbName, DbNames};
use ibschema::logical::LogicalSchema;
use ibschema::rules::{self, Built, RulesOptions};
use ibschema::storage::StorageMap;
use mdconf::configinfo::ConfigInfo;
use mdconf::extension_children::decode_attribute;
use mdconf::extension_header::{InstalledHeader, ObjectBelonging};
use mdconf::extension_reader::ExtensionRoot;
use mdconf::model::{Child, ChildTag, LocalString, MdObject, Prop, SubKind, SubObject, Value};
use mdconf::{Codec, Configuration, EntryMeta, EntrySource, MdKind};
use mdschema::{ChildSpec, KindSpec};
use memory_budget::{BudgetError, Category, Charged, Limits, MemoryBudget, MemoryLimit};
use onecd::{BlobVal, Value as Cell};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

const BUDGET: u64 = 512 << 20;

#[derive(Debug)]
pub(crate) enum ComposeError {
    Memory(BudgetError),
    Refused(String),
}

impl From<String> for ComposeError {
    fn from(why: String) -> Self {
        Self::Refused(why)
    }
}

impl From<&str> for ComposeError {
    fn from(why: &str) -> Self {
        Self::Refused(why.to_string())
    }
}

impl ComposeError {
    fn refused(reason: impl Into<String>) -> Self {
        Self::Refused(reason.into())
    }

    fn into_reason(self) -> String {
        match self {
            Self::Memory(e) => e.to_string(),
            Self::Refused(why) => why,
        }
    }

    fn prefixed(self, prefix: &str) -> Self {
        match self {
            Self::Memory(e) => Self::Memory(e),
            Self::Refused(why) => Self::Refused(format!("{prefix}: {why}")),
        }
    }
}

fn find_memory(err: &(dyn std::error::Error + 'static)) -> Option<BudgetError> {
    let mut cause = Some(err);
    while let Some(e) = cause {
        if let Some(b) = e.downcast_ref::<BudgetError>() {
            return Some(*b);
        }
        if let Some(m) = e.downcast_ref::<MemoryLimit>() {
            return Some(BudgetError::Limit(*m));
        }
        if let Some(mdconf::Error::Memory(m)) = e.downcast_ref::<mdconf::Error>() {
            return Some(BudgetError::Limit(*m));
        }
        if let Some(crate::Error::Memory(b)) = e.downcast_ref::<crate::Error>() {
            return Some(*b);
        }
        if let Some(io) = e.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::OutOfMemory
        {
            return Some(BudgetError::Overflow);
        }
        cause = e.source();
    }
    None
}

fn from_err(prefix: &str, err: impl std::error::Error + 'static) -> ComposeError {
    match find_memory(&err) {
        Some(mem) => ComposeError::Memory(mem),
        None => ComposeError::Refused(format!("{prefix}: {err}")),
    }
}

pub(crate) struct TransientGuard<'a> {
    lease: &'a OpenLease,
    bytes: u64,
}

impl<'a> TransientGuard<'a> {
    fn new(lease: &'a OpenLease) -> Self {
        Self { lease, bytes: 0 }
    }

    fn grow(&mut self, bytes: u64) -> Result<(), ComposeError> {
        if bytes == 0 {
            return Ok(());
        }
        self.lease
            .grow(bytes, false)
            .map_err(|m| ComposeError::Memory(BudgetError::Limit(m)))?;
        self.bytes = self.bytes.saturating_add(bytes);
        Ok(())
    }

    fn shrink(&mut self, bytes: u64) {
        let take = bytes.min(self.bytes);
        let _ = self.lease.shrink(take);
        self.bytes -= take;
    }
}

impl Drop for TransientGuard<'_> {
    fn drop(&mut self) {
        let _ = self.lease.shrink(self.bytes);
        self.bytes = 0;
    }
}

trait ExtEntries {
    fn entry_tree(&self, id: brace::Guid) -> Result<MaybeCharged<brace::Node>, ComposeError>;
    fn has_entry(&self, name: &str) -> Result<bool, ComposeError>;
}

struct Cas<'a> {
    folder: ConfigTxnSource<'a>,
    rows: HashMap<String, String>,
    manifest: String,
    entries: RefCell<HashMap<String, String>>,
}

fn hex(bytes: &[u8; 20]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl<'a> Cas<'a> {
    fn open(txn: &'a onecd::ReadTxn, generation: [u8; 20]) -> crate::Result<Self> {
        let folder = ConfigTxnSource::read(txn, "CONFIGCAS")?;
        let rows = folder
            .list()
            .map_err(|e| crate::Error::Inconsistent(format!("CONFIGCAS: {e}")))?
            .into_iter()
            .map(|e: EntryMeta| (e.name.to_ascii_lowercase(), e.name))
            .collect();
        Ok(Self {
            folder,
            rows,
            manifest: hex(&generation),
            entries: RefCell::new(HashMap::new()),
        })
    }

    fn prepare(&self, manifest: &ConfigInfo) {
        *self.entries.borrow_mut() = manifest
            .entries
            .iter()
            .map(|e| (e.name.to_ascii_lowercase(), hex(&e.stored_sha1)))
            .collect();
    }

    fn row(&self, name: &str) -> Option<String> {
        let key = if name == "configinfo" {
            self.manifest.clone()
        } else {
            self.entries
                .borrow()
                .get(&name.to_ascii_lowercase())?
                .clone()
        };
        self.rows.get(&key).cloned()
    }
}

impl EntrySource for Cas<'_> {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported).into())
    }
    fn stored(&self, _: &str) -> mdconf::Result<Option<Vec<u8>>> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported).into())
    }
    fn stored_len(&self, name: &str) -> mdconf::Result<Option<u64>> {
        match self.row(name) {
            Some(row) => self.folder.stored_len(&row),
            None => Ok(None),
        }
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> mdconf::Result<Option<Vec<u8>>> {
        match self.row(name) {
            Some(row) => self.folder.stored_bounded(&row, limit),
            None => Ok(None),
        }
    }
}

impl ExtEntries for Cas<'_> {
    fn entry_tree(&self, id: brace::Guid) -> Result<MaybeCharged<brace::Node>, ComposeError> {
        let name = id.to_string();
        let stored = self
            .stored_bounded(&name, ENTRY_LIMIT)
            .map_err(|e| from_err(&format!("the entry {name}"), e))?
            .ok_or_else(|| ComposeError::refused(format!("the entry {name} is missing")))?;
        let bytes = inflate_raw(&stored).map_err(|e| from_err(&format!("the entry {name}"), e))?;
        let node =
            brace::parse_bytes(&bytes).map_err(|e| from_err(&format!("the entry {name}"), e))?;
        Ok(MaybeCharged::Legacy(node))
    }

    fn has_entry(&self, name: &str) -> Result<bool, ComposeError> {
        self.stored_len(name)
            .map(|len| len.is_some())
            .map_err(|e| from_err("the rights entry", e))
    }
}

struct MeteredCas<'a> {
    source: CasSource<'a>,
    budget: &'a MemoryBudget,
}

impl ExtEntries for MeteredCas<'_> {
    fn entry_tree(&self, id: brace::Guid) -> Result<MaybeCharged<brace::Node>, ComposeError> {
        let name = id.to_string();
        let stored = self
            .source
            .stored_owned_bounded(&name, ENTRY_LIMIT)
            .map_err(|e| from_err(&format!("the entry {name}"), e))?
            .ok_or_else(|| ComposeError::refused(format!("the entry {name} is missing")))?;
        let bytes = zraw::inflate_budgeted(
            &stored,
            zraw::DEFAULT_LIMIT,
            self.budget,
            Category::Metadata,
        )
        .map_err(|e| from_err(&format!("the entry {name}"), e))?;
        drop(stored);
        let node = brace::parse_bytes_budgeted(&bytes, self.budget, Category::Metadata)
            .map_err(|e| from_err(&format!("the entry {name}"), e))?;
        Ok(MaybeCharged::Charged(node))
    }

    fn has_entry(&self, name: &str) -> Result<bool, ComposeError> {
        self.source
            .stored_len(name)
            .map(|len| len.is_some())
            .map_err(|e| from_err("the rights entry", e))
    }
}

pub(crate) struct Composed {
    pub(crate) config: Configuration,
    pub(crate) schema: LogicalSchema,
    pub(crate) built: Built,
}

pub(crate) struct MeteredComposeInput<'l, 'i> {
    pub(crate) txn: &'i onecd::ReadTxn,
    pub(crate) budget: &'i MemoryBudget,
    pub(crate) lease: &'l OpenLease,
    pub(crate) main: &'i Configuration,
    pub(crate) schema: &'i LogicalSchema,
    pub(crate) extension: Option<&'i LogicalSchema>,
    pub(crate) dbnames: &'i DbNames,
    pub(crate) base: &'i LogicalSchema,
    pub(crate) names_len: u64,
    pub(crate) schema_len: u64,
    pub(crate) ext_schema_len: u64,
}

pub(crate) struct ComposedMetered<'a> {
    pub(crate) config: Configuration,
    pub(crate) map: StorageMap,
    pub(crate) schema: Charged<Arc<LogicalSchema>>,
    pub(crate) schema_len: u64,
    pub(crate) guard: TransientGuard<'a>,
}

struct Installed {
    id: brace::Guid,
    order: i64,
    name: String,
    generation: Option<[u8; 20]>,
    active: bool,
}

pub(crate) fn compose(
    raw: &RawIb,
    main: &Configuration,
    schema: &LogicalSchema,
    extension: Option<&LogicalSchema>,
    dbnames: &DbNames,
    base: &LogicalSchema,
) -> Result<Composed, String> {
    let budget = MemoryBudget::new(Limits::new(BUDGET));
    let txn = raw.db().read();
    let installed = installed(&txn, &budget).map_err(ComposeError::into_reason)?;
    let mut additions = Additions::default();
    let mut names = dbnames.clone();
    if let Some(ext) = installed.iter().find(|e| !e.active) {
        return Err(format!("{}: the extension is not active", ext.name));
    }
    for ext in &installed {
        let generation = ext
            .generation
            .ok_or_else(|| format!("{}: no applied copy in ConfigCAS", ext.name))?;
        let source =
            Cas::open(&txn, generation).map_err(|e| format!("{}: ConfigCAS: {e}", ext.name))?;
        add_extension_with(
            &source,
            &source,
            &|manifest| source.prepare(manifest),
            &budget,
            main,
            &mut additions,
        )
        .map_err(|e| e.prefixed(&ext.name).into_reason())?;
        merge_names(raw, ext, &mut names)?;
    }
    let schema = merged_schema(schema, extension)?;
    let mut config = main.clone();
    additions.apply(&mut config)?;
    let built = rules::build(
        &config,
        &mut names,
        base,
        &RulesOptions {
            skip_unsupported: false,
        },
    )
    .map_err(|e| format!("the rules of the model with the extensions: {e}"))?;
    if !built.allocated.is_empty() {
        return Err(format!(
            "the rules of the model with the extensions need {} DBNames numbers no DBNames has",
            built.allocated.len()
        ));
    }
    let diff = crate::config::check_schema(&built, &schema);
    if let Some(first) = diff.first() {
        return Err(format!(
            "the rules of the model with the extensions differ from the merged schema: {first}"
        ));
    }
    Ok(Composed {
        config,
        schema,
        built,
    })
}

pub(crate) fn compose_metered<'l>(
    input: MeteredComposeInput<'l, '_>,
) -> Result<ComposedMetered<'l>, ComposeError> {
    let MeteredComposeInput {
        txn,
        budget,
        lease,
        main,
        schema,
        extension,
        dbnames,
        base,
        names_len,
        schema_len,
        ext_schema_len,
    } = input;
    let mut guard = TransientGuard::new(lease);
    let installed = installed(txn, budget)?;
    if let Some(ext) = installed.iter().find(|e| !e.active) {
        return Err(ComposeError::refused(format!(
            "{}: the extension is not active",
            ext.name
        )));
    }
    let mut additions = Additions::default();
    let mut names_bytes = model::PER_DBNAMES_BYTE.saturating_mul(names_len);
    guard.grow(names_bytes)?;
    let mut names = dbnames.clone();
    for ext in &installed {
        let generation = ext.generation.ok_or_else(|| {
            ComposeError::refused(format!("{}: no applied copy in ConfigCAS", ext.name))
        })?;
        let source = CasSource::open(txn, budget, generation)
            .map_err(|e| from_err(&format!("{}: ConfigCAS", ext.name), e))?;
        let entries = MeteredCas { source, budget };
        add_extension_with(
            &entries.source,
            &entries,
            &|manifest| entries.source.prepare(manifest),
            budget,
            main,
            &mut additions,
        )
        .map_err(|e| e.prefixed(&ext.name))?;
        let extra = merge_names_metered(txn, budget, &mut guard, ext, &mut names)?;
        names_bytes = names_bytes.saturating_add(extra);
    }
    let merged_est = schema_len.saturating_add(ext_schema_len);
    let schema_scratch = model::PER_RULES_BYTE.saturating_mul(merged_est);
    guard.grow(schema_scratch)?;
    let merged_raw = merged_schema(schema, extension).map_err(ComposeError::refused)?;
    let merged_text = merged_raw.to_text();
    drop(merged_raw);
    let merged_len = u64::try_from(merged_text.len()).unwrap_or(u64::MAX);
    let charged_schema = LogicalSchema::from_text_budgeted(&merged_text, budget)
        .map_err(|e| from_err("the merged schema", e))?
        .map(Arc::new);
    drop(merged_text);
    guard.shrink(schema_scratch);

    let cfg_clone_bytes = lease
        .bytes()
        .saturating_sub(lease.retained())
        .max(model::CONFIG_FIXED);
    guard.grow(cfg_clone_bytes)?;
    let mut config = main.clone();
    additions
        .apply(&mut config)
        .map_err(ComposeError::refused)?;

    let rules_bytes = model::PER_RULES_BYTE.saturating_mul(merged_len);
    guard.grow(rules_bytes)?;
    let built = rules::build(
        &config,
        &mut names,
        base,
        &RulesOptions {
            skip_unsupported: false,
        },
    )
    .map_err(|e| from_err("the rules of the model with the extensions", e))?;
    if !built.allocated.is_empty() {
        return Err(ComposeError::refused(format!(
            "the rules of the model with the extensions need {} DBNames numbers no DBNames has",
            built.allocated.len()
        )));
    }
    let diff = crate::config::check_schema(&built, &charged_schema);
    if let Some(first) = diff.first() {
        return Err(ComposeError::refused(format!(
            "the rules of the model with the extensions differ from the merged schema: {first}"
        )));
    }
    let new_map_bytes = model::PER_MAP_BYTE.saturating_mul(merged_len);
    guard.grow(new_map_bytes)?;
    let map = StorageMap::new(Arc::clone(&charged_schema), &built)
        .map_err(|e| from_err("the storage map of the model with the extensions", e))?;
    drop(built);
    drop(names);
    guard.shrink(rules_bytes.saturating_add(names_bytes));

    Ok(ComposedMetered {
        config,
        map,
        schema: charged_schema,
        schema_len: merged_len,
        guard,
    })
}

fn installed(txn: &onecd::ReadTxn, budget: &MemoryBudget) -> Result<Vec<Installed>, ComposeError> {
    let Some(table) = txn
        .find_table("_ExtensionsInfo")
        .map_err(|e| from_err("_ExtensionsInfo", e))?
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for slot in 1..table.num_slots() {
        let Some(row) = table
            .get_managed(slot, budget, Category::Metadata, super::ROW_BYTES)
            .map_err(|e| from_err("_ExtensionsInfo", e))?
        else {
            continue;
        };
        if out.len() == super::MAX_ITEMS {
            return Err(ComposeError::refused(format!(
                "more than {} extensions",
                super::MAX_ITEMS
            )));
        }
        out.push(installed_row(&table, &row, budget)?);
    }
    out.sort_by_key(|x| (x.order, x.id));
    Ok(out)
}

fn installed_row(
    table: &onecd::TableRead<'_>,
    row: &[Cell],
    budget: &MemoryBudget,
) -> Result<Installed, ComposeError> {
    let bad = |what: &str| ComposeError::refused(format!("_ExtensionsInfo: the {what} of a row"));
    let id = match row.first() {
        Some(Cell::Binary(bytes)) => {
            let raw: [u8; 16] = bytes.as_slice().try_into().map_err(|_| bad("ID"))?;
            ibschema::values::RefId(raw).to_uuid()
        }
        _ => return Err(bad("ID")),
    };
    let order = match row.get(1) {
        Some(Cell::Numeric(n)) if n.scale() == 0 => {
            let abs = n
                .digits()
                .iter()
                .try_fold(0i64, |n, d| n.checked_mul(10)?.checked_add(i64::from(*d)))
                .ok_or_else(|| bad("order"))?;
            if n.is_negative() { -abs } else { abs }
        }
        _ => return Err(bad("order")),
    };
    let name = match row.get(2) {
        Some(Cell::VarChar(name)) => name.clone(),
        _ => return Err(bad("name")),
    };
    let bytes = match row.get(6) {
        Some(Cell::Image(BlobVal::Data(bytes))) => bytes.clone(),
        Some(Cell::Image(BlobVal::Ref(reference))) => {
            if reference.len as usize > super::GENERATION_BYTES {
                return Err(ComposeError::refused(format!(
                    "{name}: the zipped information is too long"
                )));
            }
            table
                .blob_bytes_managed(
                    *reference,
                    budget,
                    Category::ReaderWorking,
                    super::GENERATION_BYTES as u64 + 256,
                )
                .map_err(|e| from_err(&format!("{name}: the zipped information"), e))?
                .to_vec()
        }
        _ => {
            return Err(ComposeError::refused(format!(
                "{name}: the zipped information has another type"
            )));
        }
    };
    let generation = super::generation_key(&bytes, budget)
        .map_err(|e| from_err(&format!("{name}: the zipped information"), e))?;
    let active = super::zipped_info(&bytes, budget)
        .map_err(|e| from_err(&format!("{name}: the zipped information"), e))?
        .active;
    Ok(Installed {
        id,
        order,
        name,
        generation,
        active,
    })
}

const ENTRY_LIMIT: usize = 1 << 20;

fn add_extension_with<'m>(
    source: &dyn EntrySource,
    entries: &dyn ExtEntries,
    prepare: &dyn Fn(&ConfigInfo),
    budget: &MemoryBudget,
    main: &'m Configuration,
    additions: &mut Additions<'m>,
) -> Result<(), ComposeError> {
    let schema = mdschema::schema().map_err(|e| from_err("the metadata schema", e))?;
    let root = ExtensionRoot::read_installed(source, budget, |manifest| {
        prepare(manifest);
        Ok(())
    })
    .map_err(|e| from_err("the root of the extension", e))?
    .ok_or_else(|| ComposeError::refused("the applied copy has no configinfo"))?;
    let mut objects = root
        .objects(source)
        .map_err(|e| from_err("the objects of the extension", e))?;
    while let Some(object) = objects
        .next()
        .map_err(|e| from_err("an object of the extension", e))?
    {
        let header = object
            .installed_header()
            .map_err(|e| from_err("an object header of the extension", e))?;
        let kind_name = object.reference.kind.unwrap_or("Unknown");
        let path = format!("{kind_name}.{}", header.name);
        let kind = MdKind::from_name(kind_name).ok_or_else(|| {
            ComposeError::refused(format!("{path}: a kind the model does not know"))
        })?;
        match header.belonging() {
            ObjectBelonging::Own => match kind {
                MdKind::Role | MdKind::CommonPicture => {
                    let object = own_object(entries, main, additions, kind, &header)
                        .map_err(|e| e.prefixed(&path))?;
                    additions.objects.push(object);
                }
                _ => {
                    return Err(ComposeError::refused(format!(
                        "{path}: an own object of this kind"
                    )));
                }
            },
            ObjectBelonging::Adopted => {
                let owner = main
                    .find(kind, header.name)
                    .filter(|o| target_is(header.extended_object, o.id))
                    .ok_or_else(|| {
                        ComposeError::refused(format!(
                            "{path}: the borrowed object is not in the main configuration"
                        ))
                    })?;
                let spec = schema.kind(kind_name).ok_or_else(|| {
                    ComposeError::refused(format!("{path}: a kind without a spec"))
                })?;
                if spec.children.is_empty() && spec.subobjects.is_empty() {
                    continue;
                }
                let tree = entries
                    .entry_tree(object.reference.entry_id)
                    .map_err(|e| e.prefixed(&path))?;
                let added = own_children(entries, main, owner, spec, &tree)
                    .map_err(|e| e.prefixed(&path))?;
                additions.children.push((owner, added));
            }
            ObjectBelonging::Unknown => {
                return Err(ComposeError::refused(format!(
                    "{path}: an unknown belonging"
                )));
            }
        }
    }
    Ok(())
}

fn target_is(target: Option<brace::Guid>, id: brace::Guid) -> bool {
    target.is_none_or(|t| t == brace::Guid::NIL || t == id)
}

fn group(node: &brace::Node) -> Result<(brace::Guid, &[brace::Node]), String> {
    let bad = || "a malformed collection".to_string();
    let [class, count, items @ ..] = node.as_list().ok_or_else(bad)? else {
        return Err(bad());
    };
    let class = class.as_guid().ok_or_else(bad)?;
    if count.as_atom().and_then(|c| c.parse::<usize>().ok()) != Some(items.len()) {
        return Err(bad());
    }
    Ok((class, items))
}

fn groups(node: &brace::Node, skip: usize) -> Result<&[brace::Node], String> {
    let bad = || "a malformed list of collections".to_string();
    let list = node.as_list().ok_or_else(bad)?;
    let (count, groups) = list
        .get(skip..)
        .and_then(|l| l.split_first())
        .ok_or_else(bad)?;
    if count.as_atom().and_then(|c| c.parse::<usize>().ok()) != Some(groups.len()) {
        return Err(bad());
    }
    Ok(groups)
}

fn item_header<'n>(
    item: &'n brace::Node,
    path: Option<&[usize]>,
) -> Result<InstalledHeader<'n>, String> {
    let node = path
        .and_then(|p| item.get(p))
        .ok_or("an item without a header")?;
    InstalledHeader::from_node(node).map_err(|e| format!("an item header: {e}"))
}

fn borrowed(header: &InstalledHeader<'_>) -> Result<bool, String> {
    match header.belonging() {
        ObjectBelonging::Adopted => Ok(true),
        ObjectBelonging::Own => Ok(false),
        ObjectBelonging::Unknown => Err(format!("{}: an unknown belonging", header.name)),
    }
}

fn by_class(children: &[(String, ChildSpec)], class: brace::Guid) -> Option<&ChildSpec> {
    children
        .iter()
        .find(|(id, _)| brace::Guid::parse(id) == Some(class))
        .map(|(_, cs)| cs)
}

#[derive(Default)]
struct Added {
    attributes: Vec<Child>,
    templates: Vec<SubObject>,
    sections: Vec<(brace::Guid, Vec<Child>)>,
}

fn own_children(
    entries: &dyn ExtEntries,
    main: &Configuration,
    owner: &MdObject,
    spec: &KindSpec,
    tree: &brace::Node,
) -> Result<Added, ComposeError> {
    let schema = mdschema::schema().map_err(|e| from_err("the metadata schema", e))?;
    let mut added = Added::default();
    for node in groups(tree, 2)? {
        let (class, items) = group(node)?;
        if items.is_empty() {
            continue;
        }
        if let Some(cs) = by_class(&spec.children, class) {
            for item in items {
                let header = item_header(item, cs.header.as_deref())?;
                let name = header.name;
                match cs.tag.as_str() {
                    "Attribute" => {
                        if !borrowed(&header)? {
                            added.attributes.push(
                                decode_attribute(main, owner, class, false, item).ok_or_else(
                                    || format!("{name}: the own attribute does not decode"),
                                )?,
                            );
                        }
                    }
                    "TabularSection" => {
                        if !borrowed(&header)? {
                            return Err(format!("{name}: an own tabular section").into());
                        }
                        let Some(section) = owner.children.iter().find(|c| {
                            c.tag == ChildTag::TabularSection
                                && c.name == name
                                && target_is(header.extended_object, c.id)
                        }) else {
                            return Err(format!(
                                "{name}: the borrowed tabular section is not in the main object"
                            )
                            .into());
                        };
                        let mut own = Vec::new();
                        for node in groups(item, 1)? {
                            let (class, items) = group(node)?;
                            for item in items {
                                let nested = by_class(&cs.children, class).ok_or_else(|| {
                                    format!("{name}: a collection of the tabular section {class}")
                                })?;
                                let header = item_header(item, nested.header.as_deref())?;
                                if borrowed(&header)? {
                                    continue;
                                }
                                if nested.tag != "Attribute" {
                                    return Err(format!(
                                        "{name}.{}: an own {} of a tabular section",
                                        header.name, nested.tag
                                    )
                                    .into());
                                }
                                own.push(
                                    decode_attribute(main, owner, class, true, item).ok_or_else(
                                        || {
                                            format!(
                                                "{name}.{}: the own attribute does not decode",
                                                header.name
                                            )
                                        },
                                    )?,
                                );
                            }
                        }
                        if !own.is_empty() {
                            added.sections.push((section.id, own));
                        }
                    }
                    tag => {
                        if !borrowed(&header)? {
                            return Err(format!("{name}: an own {tag}").into());
                        }
                    }
                }
            }
        } else if let Some((tag, _)) = spec
            .subobjects
            .iter()
            .find(|(_, id)| brace::Guid::parse(id) == Some(class))
        {
            let path = schema
                .kind(tag)
                .and_then(|k| k.header.as_deref())
                .ok_or_else(|| format!("a {tag} without a header"))?;
            for item in items {
                let id = item
                    .as_guid()
                    .ok_or_else(|| format!("a malformed reference to a {tag}"))?;
                let sub = entries.entry_tree(id)?;
                let header = item_header(&sub, Some(path))?;
                if borrowed(&header)? {
                    continue;
                }
                if tag != "Template" {
                    return Err(format!("{}: an own {tag}", header.name).into());
                }
                added.templates.push(own_template(&header)?);
            }
        } else {
            return Err(format!("a collection {class} the kind does not have").into());
        }
    }
    Ok(added)
}

#[derive(Default)]
struct Additions<'m> {
    children: Vec<(&'m MdObject, Added)>,
    objects: Vec<MdObject>,
}

impl Additions<'_> {
    fn apply(self, config: &mut Configuration) -> Result<(), String> {
        for (owner, added) in self.children {
            append_children(config, owner, added)?;
        }
        for object in self.objects {
            let (id, kind, name) = (object.id, object.kind, object.name.clone());
            config.index.order.push(id);
            config.index.objs.insert(id, (kind, Some(name.clone())));
            config
                .index
                .names
                .insert(id, format!("{}.{name}", kind.as_str()));
            config.objects.push(object);
        }
        Ok(())
    }
}

fn append_children(
    config: &mut Configuration,
    owner: &MdObject,
    added: Added,
) -> Result<(), String> {
    let owner_name = config
        .index
        .names
        .get(&owner.id)
        .cloned()
        .ok_or_else(|| format!("{}: not in the index", owner.name))?;
    let mut names = Vec::new();
    let object = config
        .objects
        .iter_mut()
        .find(|o| o.id == owner.id)
        .ok_or_else(|| format!("{}: not in the model", owner.name))?;
    let attributes = object
        .children
        .iter()
        .rposition(|c| c.tag == ChildTag::Attribute)
        .map_or(0, |i| i + 1);
    for (offset, child) in added.attributes.into_iter().enumerate() {
        names.push((child.id, format!("{owner_name}.Attribute.{}", child.name)));
        object.children.insert(attributes + offset, child);
    }
    for (section, own) in added.sections {
        let Some(ts) = object
            .children
            .iter_mut()
            .find(|c| c.tag == ChildTag::TabularSection && c.id == section)
        else {
            return Err(format!("{}: the tabular section is gone", owner.name));
        };
        let ts_name = format!("{owner_name}.TabularSection.{}", ts.name);
        for child in own {
            names.push((child.id, format!("{ts_name}.Attribute.{}", child.name)));
            ts.children.push(child);
        }
    }
    for template in added.templates {
        names.push((
            template.id,
            format!("{owner_name}.Template.{}", template.name),
        ));
        config
            .index
            .subs
            .entry(owner.id)
            .or_default()
            .push((SubKind::Template, template.id));
        object.templates.push(template);
    }
    for (id, name) in names {
        if config.index.names.insert(id, name).is_some() {
            return Err(format!(
                "{}: an ID of the extension is in the main configuration",
                owner.name
            ));
        }
    }
    Ok(())
}

fn own_object(
    entries: &dyn ExtEntries,
    main: &Configuration,
    additions: &Additions<'_>,
    kind: MdKind,
    header: &mdconf::extension_header::InstalledHeader<'_>,
) -> Result<MdObject, ComposeError> {
    if main.index.objs.contains_key(&header.id)
        || main.object(header.id).is_some()
        || additions.objects.iter().any(|o| o.id == header.id)
    {
        return Err(ComposeError::refused(
            "an ID of the extension is in the main configuration",
        ));
    }
    if kind == MdKind::Role && entries.has_entry(&format!("{}.0", header.id))? {
        return Err(ComposeError::refused("a role of an extension with rights"));
    }
    let (name, synonym, comment) = header_texts(header).map_err(ComposeError::refused)?;
    let object = MdObject {
        kind,
        id: header.id,
        name: name.clone(),
        synonym: synonym.clone(),
        comment: comment.clone(),
        props: header_props(name, synonym, comment),
        generated_types: Vec::new(),
        this_node: None,
        children: Vec::new(),
        forms: Vec::new(),
        templates: Vec::new(),
        recalculations: Vec::new(),
        external: Vec::new(),
        subsystems: Vec::new(),
        modules: Vec::new(),
        parts: Default::default(),
        form: None,
        template: None,
        flowchart: None,
    };
    Ok(object)
}

fn header_texts(header: &InstalledHeader<'_>) -> Result<(String, LocalString, String), String> {
    let synonym = local_string(header.synonym).ok_or("the synonym does not read")?;
    Ok((header.name.to_string(), synonym, header.comment.to_string()))
}

fn header_props(name: String, synonym: LocalString, comment: String) -> Vec<Prop> {
    let text = |name: &str, value: String| Prop {
        name: name.into(),
        codec: Codec::Str,
        value: Value::Text(value),
    };
    vec![
        text("Name", name),
        Prop {
            name: "Synonym".into(),
            codec: Codec::LStr,
            value: Value::LStr(synonym),
        },
        text("Comment", comment),
    ]
}

fn own_template(header: &InstalledHeader<'_>) -> Result<SubObject, String> {
    let (name, synonym, comment) = header_texts(header)?;
    Ok(SubObject {
        kind: SubKind::Template,
        id: header.id,
        name: name.clone(),
        props: header_props(name, synonym, comment),
        module: None,
        form: None,
        template: None,
    })
}

fn local_string(node: &brace::Node) -> Option<LocalString> {
    let list = node.as_list()?;
    let count: usize = list.first()?.as_atom()?.parse().ok()?;
    if list.len() != 1 + 2 * count {
        return None;
    }
    let items = (0..count)
        .map(|i| {
            Some((
                list.get(1 + 2 * i)?.text()?.to_string(),
                list.get(2 + 2 * i)?.text()?.to_string(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(LocalString(items))
}

fn merge_names(raw: &RawIb, ext: &Installed, names: &mut DbNames) -> Result<(), String> {
    let file = format!("DBNames-Ext-{}", ext.id);
    let Some(found) = raw
        .folder_file("PARAMS", &file)
        .map_err(|e| format!("PARAMS {file}: {e}"))?
    else {
        return Ok(());
    };
    let text = inflate_raw(&found.data).map_err(|e| format!("PARAMS {file}: {e}"))?;
    let own = DbNames::parse(&text).map_err(|e| format!("PARAMS {file}: {e}"))?;
    merge_dbnames(&file, &own, names)
}

fn merge_names_metered(
    txn: &onecd::ReadTxn,
    budget: &MemoryBudget,
    guard: &mut TransientGuard<'_>,
    ext: &Installed,
    names: &mut DbNames,
) -> Result<u64, ComposeError> {
    let file = format!("DBNames-Ext-{}", ext.id);
    let prefix = format!("PARAMS {file}");
    let Some(stored) =
        folder_file_data_in_budgeted(txn, "PARAMS", &file, budget, zraw::DEFAULT_LIMIT)
            .map_err(|e| from_err(&prefix, e))?
    else {
        return Ok(0);
    };
    let text = zraw::inflate_budgeted(&stored, zraw::DEFAULT_LIMIT, budget, Category::Metadata)
        .map_err(|e| from_err(&prefix, e))?;
    drop(stored);
    let text_len = u64::try_from(text.len()).unwrap_or(u64::MAX);
    let own = DbNames::parse_budgeted(&text, budget).map_err(|e| from_err(&prefix, e))?;
    drop(text);
    let extra = model::PER_DBNAMES_BYTE.saturating_mul(text_len);
    guard.grow(extra)?;
    merge_dbnames(&file, &own, names).map_err(ComposeError::refused)?;
    Ok(extra)
}

fn merge_dbnames(file: &str, own: &DbNames, names: &mut DbNames) -> Result<(), String> {
    for entry in own.entries() {
        match names.number(entry.uuid, &entry.purpose) {
            Some(n) if n == entry.number => {}
            Some(n) => {
                return Err(format!(
                    "PARAMS {file}: {} {} is {} in DBNames, {} in the extension",
                    entry.uuid, entry.purpose, n, entry.number
                ));
            }
            None => names
                .push(DbName {
                    uuid: entry.uuid,
                    purpose: entry.purpose.clone(),
                    number: entry.number,
                })
                .map_err(|e| format!("PARAMS {file}: {e}"))?,
        }
    }
    Ok(())
}

fn merged_schema(
    main: &LogicalSchema,
    extension: Option<&LogicalSchema>,
) -> Result<LogicalSchema, String> {
    let mut merged = main.clone();
    for table in extension.map(|e| e.tables.as_slice()).unwrap_or_default() {
        let Some(slot) = merged.tables.iter_mut().find(|t| t.name == table.name) else {
            return Err(format!(
                "{}: a table of the extension schema the main schema lacks",
                table.name
            ));
        };
        *slot = table.clone();
    }
    Ok(merged)
}
