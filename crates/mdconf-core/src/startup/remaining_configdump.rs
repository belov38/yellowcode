use super::{
    ManagedDiagnostics, ManagedIndex,
    index::{GuidText, text},
    source_copy::copy_node,
    values::{append, empty},
};
use crate::parts::ConfigDumpInfo;
use brace::{Guid, Node};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};
fn subnames() -> Option<&'static mdschema::StaticValue> {
    mdschema::static_schema()
        .table("rights/configdump.json")?
        .get("subnames")
}
fn has_suffix(kind: &str, suffix: &str) -> bool {
    subnames()
        .and_then(|t| t.get(kind))
        .and_then(|t| t.get(suffix))
        .and_then(|t| t.as_str())
        .is_some()
        || (kind == "CalculationRegister.Recalculation" && suffix == "0")
}
fn generic(child: &str, suffix: &str) -> bool {
    let mut agreed = None;
    for (path, fields) in subnames().and_then(|v| v.as_object()).unwrap_or_default() {
        if path.split_once('.').map(|(_, tail)| tail) != Some(child) {
            continue;
        }
        if let Some(name) = fields.get(suffix).and_then(|v| v.as_str()) {
            match agreed {
                None => agreed = Some(name),
                Some(old) if old == name => {}
                Some(_) => return false,
            }
        }
    }
    agreed.is_some()
}
fn validate(
    index: &ManagedIndex,
    settings: &crate::ConfigSettings,
    versions: &[(String, Guid)],
    diagnostics: &mut ManagedDiagnostics,
) -> Result<(), BudgetError> {
    let configuration = text(index.memory_budget(), &["Configuration.", &settings.name])?;
    for (name, _) in versions {
        if name.is_empty() || matches!(name.as_str(), "root" | "version" | "versions") {
            continue;
        }
        let (raw, suffix) = name
            .split_once('.')
            .map_or((name.as_str(), None), |(id, s)| (id, Some(s)));
        let id = Guid::parse(raw);
        let canonical = match id {
            Some(id) => GuidText::new(id).as_str()? == raw,
            None => false,
        };
        let owner = if canonical && (id == Some(index.cfg_id) || id == Some(index.header_id)) {
            Some(configuration.as_str())
        } else if canonical {
            id.and_then(|id| index.names.get(&id)).map(|n| n.as_str())
        } else {
            None
        };
        let Some(owner) = owner else {
            diagnostics.miss(&["configdump:element:", name])?;
            break;
        };
        let Some(suffix) = suffix else {
            continue;
        };
        let mut path =
            memory_budget::BudgetedText::new(index.memory_budget(), Category::Metadata, u64::MAX)?;
        for (i, part) in owner.split('.').step_by(2).enumerate() {
            if i > 0 {
                path.try_push_str(".")?;
            }
            path.try_push_str(part)?;
        }
        let path = path.as_str();
        if has_suffix(path, suffix)
            || (path.starts_with("Subsystem") && has_suffix("Subsystem", suffix))
            || path
                .split_once('.')
                .is_some_and(|(_, child)| generic(child, suffix))
        {
            continue;
        }
        diagnostics.miss(&["configdump:suffix:", path, ":", suffix])?;
        break;
    }
    Ok(())
}
pub(super) fn decode(
    index: &ManagedIndex,
    settings: &crate::ConfigSettings,
    node: &Node,
    keep: bool,
    diagnostics: &mut ManagedDiagnostics,
) -> Result<Option<Charged<ConfigDumpInfo>>, BudgetError> {
    let budget = index.memory_budget();
    let Some(list) = node.as_list() else {
        diagnostics.miss(&["configdump:versions"])?;
        return Ok(None);
    };
    let Some(n) = list
        .get(1)
        .and_then(Node::text)
        .and_then(|n| n.trim().parse::<i64>().ok())
    else {
        diagnostics.miss(&["configdump:versions"])?;
        return Ok(None);
    };
    let n = usize::try_from(n).unwrap_or(0);
    let available = list.len().saturating_sub(2) / 2;
    let mut versions = BudgetedVec::<(String, Guid)>::try_with_capacity(
        budget,
        Category::Metadata,
        n.min(available),
    )?
    .into_charged_vec();
    for i in 0..n.min(available.saturating_add(1)) {
        let pair = i
            .checked_mul(2)
            .and_then(|i| i.checked_add(2))
            .and_then(|at| at.checked_add(2).and_then(|end| list.get(at..end)));
        let pair = pair.and_then(|p| Some((p.first()?.text()?, p.get(1)?.text()?)));
        let Some((name, raw)) = pair else {
            diagnostics.miss(&["configdump:versions"])?;
            return Ok(None);
        };
        let Some(id) = Guid::parse(raw) else {
            if name.is_empty() {
                diagnostics.miss(&["configdump:version"])?;
                continue;
            }
            diagnostics.miss(&["configdump:version:", name])?;
            return Ok(None);
        };
        versions = append(
            versions,
            text(budget, &[name])?.into_string()?.map(|name| (name, id)),
        )?;
    }
    let source = if keep {
        copy_node(budget, node)?
    } else {
        empty(budget, Node::empty())?
    };
    let output = versions
        .try_zip(source)?
        .map(|(versions, source)| ConfigDumpInfo { versions, source });
    validate(index, settings, &output.versions, diagnostics)?;
    Ok(Some(output))
}
