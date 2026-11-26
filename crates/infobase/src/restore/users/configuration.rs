use super::*;
use crate::config::{ManagedConfigTxnSource, MeteredConfigSource, OpenLease, metered::model};
use mdconf::{Configuration, ConfigurationExt, EntrySource, LoadOptions, LoadProfile, MdKind};
use memory_budget::{BudgetError, BudgetedVec, Evictor, Limits};
use std::sync::Arc;

const ADMINISTRATION_RIGHT_GUID: &str = "900e3c92-6e18-4874-846a-b28780b5b54c";

fn memory(error: memory_budget::MemoryLimit) -> Error {
    Error::Memory(BudgetError::Limit(error))
}
fn config_error(error: mdconf::Error) -> Error {
    match error {
        mdconf::Error::Memory(m) => memory(m),
        _ => bad("invalid CONFIG for users reconstruction"),
    }
}
fn invalid_rights() -> Error {
    bad("invalid CONFIG rights for users reconstruction")
}
fn unsupported_shape() -> Error {
    bad("users RIGHTS of unsupported shape")
}

fn list(node: Option<&brace::Node>) -> Result<&[brace::Node]> {
    node.and_then(brace::Node::as_list)
        .ok_or_else(invalid_rights)
}
fn count(node: Option<&brace::Node>) -> Result<usize> {
    node.and_then(brace::Node::text)
        .and_then(|t| t.trim().parse::<i64>().ok())
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(invalid_rights)
}
fn counted_exact(nodes: &[brace::Node]) -> Result<&[brace::Node]> {
    let n = count(nodes.first())?;
    let end = n.checked_add(1).ok_or_else(invalid_rights)?;
    if nodes.len() != end {
        return Err(invalid_rights());
    }
    nodes.get(1..end).ok_or_else(invalid_rights)
}
fn bool3(node: Option<&brace::Node>) -> Result<bool> {
    match node.and_then(brace::Node::text) {
        Some("1") => Ok(true),
        Some("0" | "-1" | "4294967295") => Ok(false),
        _ => Err(invalid_rights()),
    }
}
fn check_unique_guids(guids: &mut BudgetedVec<Uuid16>) -> Result<()> {
    guids.sort_unstable();
    if guids.windows(2).any(|w| matches!(w, [a, b] if a == b)) {
        return Err(unsupported_shape());
    }
    Ok(())
}

fn parse_root_pairs(
    pairs: &[brace::Node],
    admin_right: Uuid16,
    budget: &MemoryBudget,
) -> Result<Option<bool>> {
    let mut seen = BudgetedVec::try_with_capacity(budget, Category::Metadata, pairs.len() / 2)
        .map_err(Error::Memory)?;
    let mut admin = None;
    for [key, val] in pairs.as_chunks::<2>().0 {
        let id = key
            .text()
            .and_then(Uuid16::parse)
            .ok_or_else(unsupported_shape)?;
        let value = bool3(Some(val))?;
        seen.try_push(id).map_err(Error::Memory)?;
        if id == admin_right {
            admin = Some(value);
        }
    }
    check_unique_guids(&mut seen)?;
    Ok(admin)
}

fn parse_root_rightlist(
    rl: &[brace::Node],
    admin_right: Uuid16,
    budget: &MemoryBudget,
) -> Result<Option<bool>> {
    match rl.first().and_then(brace::Node::text) {
        Some("0") => {
            let tail = rl.get(1..).ok_or_else(invalid_rights)?;
            if !tail.len().is_multiple_of(2) {
                return Err(invalid_rights());
            }
            parse_root_pairs(tail, admin_right, budget)
        }
        Some("1") => {
            let n = count(rl.get(1))?;
            let end = n
                .checked_mul(2)
                .and_then(|x| x.checked_add(2))
                .ok_or_else(invalid_rights)?;
            let pairs = rl.get(2..end).ok_or_else(invalid_rights)?;
            let admin = parse_root_pairs(pairs, admin_right, budget)?;
            let rest = rl.get(end..).ok_or_else(invalid_rights)?;
            if !rest.is_empty() {
                let associations = counted_exact(rest)?;
                let mut seen =
                    BudgetedVec::try_with_capacity(budget, Category::Metadata, associations.len())
                        .map_err(Error::Memory)?;
                for item in associations {
                    let item = list(Some(item))?;
                    let id = item
                        .first()
                        .and_then(brace::Node::text)
                        .and_then(Uuid16::parse)
                        .ok_or_else(unsupported_shape)?;
                    let _rls = counted_exact(list(item.get(1))?)?;
                    seen.try_push(id).map_err(Error::Memory)?;
                }
                check_unique_guids(&mut seen)?;
            }
            Ok(admin)
        }
        _ => Err(invalid_rights()),
    }
}

fn root_administration(
    tree: &brace::Node,
    cfg_id: Uuid16,
    admin_right: Uuid16,
    budget: &MemoryBudget,
) -> Result<bool> {
    let root = list(Some(tree))?;
    let objects = counted_exact(list(root.get(1))?)?;
    let _templates = counted_exact(list(root.get(2))?)?;
    let set_for_new_objects = bool3(root.get(3))?;
    let _set_for_attributes_by_default = bool3(root.get(4))?;
    let _independent_rights_of_child_objects = bool3(root.get(5))?;
    let mut saw_root = false;
    let mut admin = None;
    for object in objects {
        let pair = list(Some(object))?;
        let objref = list(pair.first())?;
        let rightlist = list(pair.get(1))?;
        let id = objref
            .get(1)
            .and_then(brace::Node::text)
            .and_then(Uuid16::parse)
            .ok_or_else(invalid_rights)?;
        let standard = match objref.get(2).and_then(brace::Node::text) {
            Some("0") => false,
            Some("1") => true,
            _ => return Err(invalid_rights()),
        };
        if id != cfg_id || standard {
            continue;
        }
        if saw_root {
            return Err(unsupported_shape());
        }
        saw_root = true;
        admin = parse_root_rightlist(rightlist, admin_right, budget)?;
    }
    Ok(admin.unwrap_or(set_for_new_objects))
}

fn role_has_administration(
    source: &MeteredConfigSource<'_>,
    budget: &MemoryBudget,
    name: &str,
    cfg_id: Uuid16,
    admin_right: Uuid16,
) -> Result<bool> {
    let Some(inflated) = source
        .stored_inflated_bounded(name, zraw::DEFAULT_LIMIT)
        .map_err(|e| match e {
            mdconf::Error::Memory(m) => memory(m),
            _ => invalid_rights(),
        })?
    else {
        return Ok(false);
    };
    if !inflated.was_deflated || v8cont::is_container(&inflated.bytes) {
        return Err(invalid_rights());
    }
    let body = inflated
        .bytes
        .strip_prefix(b"\xef\xbb\xbf")
        .unwrap_or(&inflated.bytes);
    if body.first() != Some(&b'{') {
        return Err(invalid_rights());
    }
    let tree =
        brace::parse_bytes_budgeted(&inflated.bytes, budget, Category::Metadata).map_err(|e| {
            match e {
                brace::ManagedParseError::Memory(m) => Error::Memory(m),
                brace::ManagedParseError::Parse(_) => invalid_rights(),
            }
        })?;
    drop(inflated);
    root_administration(&tree, cfg_id, admin_right, budget)
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
    let fixed = OpenLease::reserve_fixed(&budget)?;
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
    let managed = ManagedConfigTxnSource::read_in(&txn, "CONFIG", &budget)?;
    if managed.is_empty() {
        return Ok(Facts::without_configuration());
    }
    lease
        .grow(
            scaled(
                managed.list().len() as u64,
                model::PER_ENTRY,
                model::CONFIG_FIXED,
            )?,
            false,
        )
        .map_err(memory)?;
    let source = MeteredConfigSource::new(managed, budget.clone());

    let mut facts = Facts::default();
    let (cfg_id, roles) = {
        let cfg = Configuration::load(
            &source,
            &LoadOptions {
                keep_source: false,
                profile: LoadProfile::Query,
                meter: Some(lease.clone()),
                ..Default::default()
            },
        )
        .map_err(config_error)?;
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
        let mut roles = BudgetedVec::try_with_capacity(&budget, Category::Metadata, role_count)
            .map_err(Error::Memory)?;
        for role in cfg.objects.iter().filter(|o| o.kind == MdKind::Role) {
            roles.try_push(role.id).map_err(Error::Memory)?;
        }
        (cfg.id, roles)
    };
    let transient = lease.bytes().saturating_sub(lease.retained());
    lease.shrink(transient).map_err(memory)?;

    let admin_right =
        Uuid16::parse(ADMINISTRATION_RIGHT_GUID).ok_or_else(|| bad("Administration right"))?;
    for &role_id in roles.iter() {
        let name = format!("{role_id}.0");
        if role_has_administration(&source, &budget, &name, cfg_id, admin_right)? {
            facts
                .administration
                .as_mut()
                .ok_or_else(|| bad("users Administration facts"))?
                .push(role_id);
        }
    }
    Ok(facts)
}
