use super::*;
use crate::config::{ManagedConfigTxnSource, MeteredConfigSource, OpenLease, metered::model};
use mdconf::{
    Configuration, ConfigurationExt, EntryMeta, EntrySource, Inflated, LoadMeter, LoadOptions,
    LoadProfile, MdKind, MeterRefusal, Stages,
};
use memory_budget::{BudgetError, Evictor, Limits};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

fn memory(error: memory_budget::MemoryLimit) -> Error {
    Error::Memory(BudgetError::Limit(error))
}
fn config_error(error: mdconf::Error) -> Error {
    match error {
        mdconf::Error::Memory(m) => memory(m),
        mdconf::Error::Format { what, .. } if what.starts_with("users RIGHTS") => bad(what),
        _ => bad("invalid CONFIG for users reconstruction"),
    }
}
fn format_error() -> mdconf::Error {
    mdconf::Error::Format {
        entry: "CONFIG".into(),
        what: "users RIGHTS of unsupported shape".into(),
    }
}
fn size_error() -> mdconf::Error {
    mdconf::Error::Format {
        entry: "CONFIG".into(),
        what: "users RIGHTS over 512 MiB of decoded rights".into(),
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RightsShape {
    objects: u64,
    pairs: u64,
    restrictions: u64,
    fields: u64,
    condition_bytes: u64,
    template_bytes: u64,
    templates: u64,
}
impl RightsShape {
    const OBJECT: u64 = 1024;
    const PAIR: u64 = 256;
    const RESTRICTION: u64 = 512;
    const FIELD: u64 = 128;
    const TEMPLATE: u64 = 256;
    const FIXED: u64 = 64 * 1024;

    fn allowance(&self, max_name: u64) -> Option<u64> {
        let name = max_name.checked_add(128)?;
        let objects = self
            .objects
            .checked_mul(Self::OBJECT.checked_add(name.checked_mul(2)?)?)?;
        let pairs = self.pairs.checked_mul(Self::PAIR)?;
        let restrictions = self
            .restrictions
            .checked_mul(Self::RESTRICTION)?
            .checked_add(self.condition_bytes.checked_mul(2)?)?;
        let fields = self
            .fields
            .checked_mul(Self::FIELD.checked_add(name.checked_mul(2)?)?)?;
        let templates = self
            .templates
            .checked_mul(Self::TEMPLATE)?
            .checked_add(self.template_bytes.checked_mul(2)?)?;
        [objects, pairs, restrictions, fields, templates]
            .into_iter()
            .try_fold(Self::FIXED, u64::checked_add)
    }
}

struct RightsMeter {
    query: Arc<OpenLease>,
    roles: Vec<String>,
    max_name: u64,
    retained: Mutex<(std::collections::HashMap<String, u64>, u64)>,
}
impl RightsMeter {
    fn lock(&self) -> std::sync::MutexGuard<'_, (std::collections::HashMap<String, u64>, u64)> {
        self.retained
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
impl LoadMeter for RightsMeter {
    fn admit(&self, entry: &str, inflated: u64) -> std::result::Result<(), MeterRefusal> {
        self.query.admit(entry, inflated)?;
        if self.roles.iter().any(|role| role == entry) {
            let bytes = self
                .lock()
                .0
                .get(entry)
                .copied()
                .unwrap_or_else(|| inflated.saturating_mul(4096 + 4 * (self.max_name + 128)));
            self.query.grow(bytes, false).map_err(MeterRefusal)?;
        }
        Ok(())
    }
    fn hold(&self, entry: &str, bytes: u64) -> std::result::Result<(), MeterRefusal> {
        self.query.hold(entry, bytes)
    }
    fn release(&self, bytes: u64) {
        self.query.release(bytes);
    }
    fn admit_copy(&self, entry: &str, bytes: u64) -> std::result::Result<(), MeterRefusal> {
        self.query.admit_copy(entry, bytes)
    }
}
struct RightsSource<'a> {
    inner: MeteredConfigSource<'a>,
    meter: Arc<RightsMeter>,
    budget: MemoryBudget,
}
impl EntrySource for RightsSource<'_> {
    fn list(&self) -> mdconf::Result<Vec<EntryMeta>> {
        self.inner.list()
    }
    fn stored(&self, name: &str) -> mdconf::Result<Option<Vec<u8>>> {
        self.inner.stored(name)
    }
    fn stored_bounded(&self, name: &str, limit: usize) -> mdconf::Result<Option<Vec<u8>>> {
        self.inner.stored_bounded(name, limit)
    }
    fn stored_inflated_bounded(&self, name: &str, limit: u64) -> mdconf::Result<Option<Inflated>> {
        let Some(data) = self.inner.stored_inflated_bounded(name, limit)? else {
            return Ok(None);
        };
        if self.meter.roles.iter().any(|role| role == name) {
            let len = data.bytes.len() as u64;
            let scratch = len
                .checked_mul(128)
                .and_then(|n| n.checked_add(MIB))
                .ok_or_else(size_error)?;
            let _lease = self
                .budget
                .try_reserve(Category::Metadata, scratch)
                .map_err(mdconf::Error::Memory)?;
            let tree = brace::parse_bytes(&data.bytes).map_err(|_| format_error())?;
            let allowance = rights_shape(&tree)?
                .allowance(self.meter.max_name)
                .ok_or_else(size_error)?;
            let mut retained = self.meter.lock();
            let previous = retained.0.get(name).copied().unwrap_or(0);
            let total = retained
                .1
                .checked_sub(previous)
                .and_then(|t| t.checked_add(allowance))
                .filter(|n| *n <= 512 * MIB)
                .ok_or_else(size_error)?;
            retained.0.insert(name.to_string(), allowance);
            retained.1 = total;
        }
        Ok(Some(data))
    }
}

fn list(node: Option<&brace::Node>) -> mdconf::Result<&[brace::Node]> {
    node.and_then(brace::Node::as_list).ok_or_else(format_error)
}
fn count(node: Option<&brace::Node>) -> mdconf::Result<usize> {
    node.and_then(brace::Node::as_u32)
        .map(|n| n as usize)
        .ok_or_else(format_error)
}
fn unique<'a>(nodes: impl Iterator<Item = &'a brace::Node>) -> mdconf::Result<u64> {
    let mut keys = BTreeSet::new();
    for node in nodes {
        let id = node
            .text()
            .and_then(Uuid16::parse)
            .ok_or_else(format_error)?;
        if !keys.insert(id) {
            return Err(format_error());
        }
    }
    Ok(keys.len() as u64)
}
fn text_len(node: Option<&brace::Node>) -> u64 {
    node.and_then(brace::Node::text)
        .map_or(0, |t| t.len() as u64)
}
fn rights_shape(tree: &brace::Node) -> mdconf::Result<RightsShape> {
    let mut shape = RightsShape::default();
    let objects = list(tree.at(1))?;
    let n = count(objects.first())?;
    let objects = objects
        .get(1..n.checked_add(1).ok_or_else(format_error)?)
        .ok_or_else(format_error)?;
    shape.objects = n as u64;
    for object in objects {
        let rights = list(object.at(1))?;
        if rights.first().and_then(brace::Node::text) == Some("0") {
            shape.pairs += unique(rights.iter().skip(1).step_by(2))?;
        } else {
            let end = count(rights.get(1))?
                .checked_mul(2)
                .and_then(|n| n.checked_add(2))
                .ok_or_else(format_error)?;
            shape.pairs += unique(
                rights
                    .get(2..end)
                    .ok_or_else(format_error)?
                    .iter()
                    .step_by(2),
            )?;
            let rest = rights.get(end..).ok_or_else(format_error)?;
            if !rest.is_empty() {
                let end = count(rest.first())?
                    .checked_add(1)
                    .ok_or_else(format_error)?;
                let associations = rest.get(1..end).ok_or_else(format_error)?;
                let mut keys = BTreeSet::new();
                for item in associations {
                    let id = item
                        .at(0)
                        .and_then(brace::Node::text)
                        .and_then(Uuid16::parse)
                        .ok_or_else(format_error)?;
                    if !keys.insert(id) {
                        return Err(format_error());
                    }
                    let items = item
                        .at(1)
                        .and_then(brace::Node::as_list)
                        .unwrap_or_default();
                    shape.restrictions += (items.len().saturating_sub(1) as u64).max(1);
                    for rls in items.iter().skip(1) {
                        let rls = rls.as_list().unwrap_or_default();
                        shape.condition_bytes += text_len(rls.get(1));
                        shape.fields += rls.len().saturating_sub(3) as u64;
                    }
                }
            }
        }
    }
    if let Some(templates) = tree.at(2).and_then(brace::Node::as_list) {
        for template in templates.iter().skip(1) {
            let template = template.as_list().unwrap_or_default();
            shape.templates += 1;
            shape.template_bytes += text_len(template.first()) + text_len(template.get(1));
        }
    }
    Ok(shape)
}

fn separated(object: &mdconf::MdObject, property: &str) -> bool {
    object.props.iter().any(|p| {
        p.name == property && matches!(&p.value, mdconf::Value::Text(v) if v == "Separate")
    })
}

pub(in crate::restore) fn facts(path: &Path, supplied: Option<&MemoryBudget>) -> Result<Facts> {
    let budget = supplied
        .cloned()
        .unwrap_or_else(|| MemoryBudget::new(Limits::new(1024 * MIB)));
    let mut facts = Facts::default();
    let (roles, max_name, _bounds_lease) = load(path, &budget, None, |cfg| {
        let Some(cfg) = cfg else {
            return Ok((Vec::new(), 0, None));
        };
        let max_name = cfg
            .index
            .names
            .values()
            .map(String::len)
            .chain(std::iter::once(
                "Configuration.".len() + cfg.settings.name.len(),
            ))
            .max()
            .unwrap_or(0);
        if max_name > 4096 {
            return Err(bad("users resolved name limit"));
        }
        if mdschema::forms::std_attrs()
            .map_err(|_| bad("standard attribute schema"))?
            .iter()
            .flat_map(|(_, items)| items)
            .any(|(_, name)| ".StandardAttribute.".len() + name.len() > 128)
        {
            return Err(bad("users standard attribute name limit"));
        }
        let role_count = cfg
            .objects
            .iter()
            .filter(|o| o.kind == MdKind::Role)
            .count();
        let names = cfg
            .objects
            .iter()
            .filter(|o| o.kind == MdKind::CommonAttribute && separated(o, "UsersSeparation"));
        let (name_count, name_bytes) = names.clone().try_fold((0usize, 0usize), |(n, b), o| {
            b.checked_add(o.name.len())
                .map(|b| (n + 1, b))
                .ok_or_else(|| bad("users name size overflow"))
        })?;
        if role_count > MAX_NAMES || name_count > MAX_NAMES || name_bytes > MIB as usize {
            return Err(bad("users configuration facts limit"));
        }
        let held = scaled(
            role_count as u64,
            16,
            scaled(name_count as u64, 48, name_bytes as u64 + KIB64)?,
        )?;
        facts._lease = reserve(Some(&budget), held)?;
        facts.separated = cfg
            .objects
            .iter()
            .filter(|o| o.kind == MdKind::CommonAttribute)
            .any(|o| separated(o, "UsersSeparation") || separated(o, "AuthenticationSeparation"));
        facts.names = names.map(|o| o.name.clone()).collect();
        facts.administration = Some(Vec::with_capacity(role_count));
        let bounds_lease = reserve(Some(&budget), scaled(role_count as u64, 128, KIB64)?)?;
        let roles = cfg
            .objects
            .iter()
            .filter(|o| o.kind == MdKind::Role)
            .map(|o| format!("{}.0", o.id))
            .collect();
        Ok((roles, max_name as u64, bounds_lease))
    })?;
    if facts.administration.is_none() {
        facts.administration = Some(Vec::new());
        return Ok(facts);
    }
    let right = mdconf::parts::Right::from_guid(
        Uuid16::parse("900e3c92-6e18-4874-846a-b28780b5b54c")
            .ok_or_else(|| bad("Administration right"))?,
    )
    .ok_or_else(|| bad("Administration right schema"))?;
    load(path, &budget, Some((roles, max_name)), |cfg| {
        let cfg = cfg.ok_or_else(|| bad("CONFIG vanished"))?;
        if cfg
            .diagnostics
            .misses
            .keys()
            .any(|key| key.starts_with("rights:"))
        {
            return Err(bad("invalid CONFIG rights for users reconstruction"));
        }
        for role in cfg.objects.iter().filter(|o| o.kind == MdKind::Role) {
            if role
                .rights()
                .is_some_and(|r| r.value(&mdconf::parts::RightTarget::Configuration, right))
            {
                facts
                    .administration
                    .as_mut()
                    .ok_or_else(|| bad("users Administration facts"))?
                    .push(role.id);
            }
        }
        Ok(())
    })?;
    Ok(facts)
}

fn load<T>(
    path: &Path,
    budget: &MemoryBudget,
    rights: Option<(Vec<String>, u64)>,
    extract: impl FnOnce(Option<&Configuration>) -> Result<T>,
) -> Result<T> {
    let fixed = OpenLease::reserve_fixed(budget)?;
    let db = Arc::new(
        Database::open(
            path,
            onecd::OpenOptions {
                memory_budget: Some(budget.clone()),
                locking: Locking::None,
                ..onecd::OpenOptions::read_only()
            },
        )
        .map_err(|e| match e {
            onecd::Error::Memory(e) => Error::Memory(e),
            other => Error::Engine(other),
        })?,
    );
    let weak: std::sync::Weak<dyn Evictor + Send> = Arc::<Database>::downgrade(&db);
    let budget = budget.try_with_evictor(weak)?;
    let lease = Arc::new(OpenLease::new(
        fixed,
        Arc::clone(&db) as Arc<dyn Evictor + Send>,
    ));
    let txn = db.read();
    let source = ManagedConfigTxnSource::read_in(&txn, "CONFIG", &budget)?;
    if source.is_empty() {
        return extract(None);
    }
    lease
        .grow(
            scaled(
                source.list().len() as u64,
                model::PER_ENTRY,
                model::CONFIG_FIXED,
            )?,
            false,
        )
        .map_err(memory)?;
    let source = MeteredConfigSource::new(source, budget.clone());
    let extra = if rights.is_some() {
        Stages::RIGHTS
    } else {
        Stages::NONE
    };
    let cfg = if let Some((roles, max_name)) = rights {
        let meter = Arc::new(RightsMeter {
            query: lease.clone(),
            roles,
            max_name,
            retained: Mutex::new(Default::default()),
        });
        let source = RightsSource {
            inner: source,
            meter: meter.clone(),
            budget: budget.clone(),
        };
        Configuration::load(
            &source,
            &LoadOptions {
                keep_source: false,
                profile: LoadProfile::Query,
                extra_stages: extra,
                meter: Some(meter),
                ..Default::default()
            },
        )
        .map_err(config_error)?
    } else {
        Configuration::load(
            &source,
            &LoadOptions {
                keep_source: false,
                profile: LoadProfile::Query,
                meter: Some(lease.clone()),
                ..Default::default()
            },
        )
        .map_err(config_error)?
    };
    extract(Some(&cfg))
}
