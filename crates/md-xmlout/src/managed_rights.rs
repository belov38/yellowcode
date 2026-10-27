use crate::{DUMP_VERSION, ManagedXmlWriter};
use brace::Guid;
use md_model::parts::{Right, Rights};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};

fn boolean(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
pub fn managed_rights_xml(
    budget: &MemoryBudget,
    configuration_id: Guid,
    rights: &Rights,
    limit: u64,
) -> Result<Option<Charged<String>>, BudgetError> {
    let mut writer = ManagedXmlWriter::part_document(
        budget,
        limit,
        "Rights",
        &[
            ("xmlns", "http://v8.1c.ru/8.2/roles"),
            ("xmlns:xs", "http://www.w3.org/2001/XMLSchema"),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
            ("xsi:type", "Rights"),
            ("version", DUMP_VERSION),
        ],
    )?;
    for (tag, value) in [
        ("setForNewObjects", rights.set_for_new_objects),
        (
            "setForAttributesByDefault",
            rights.set_for_attributes_by_default,
        ),
        (
            "independentRightsOfChildObjects",
            rights.independent_rights_of_child_objects,
        ),
    ] {
        writer.leaf(tag, Some(boolean(value)), &[])?;
    }
    let mut objects =
        BudgetedVec::try_with_capacity(budget, Category::Metadata, rights.objects.len())?;
    for (ordinal, object) in rights.objects.iter().enumerate() {
        objects.try_push((ordinal, object))?;
    }
    objects.sort_unstable_by_key(|(ordinal, object)| {
        (object.target.sort_key(configuration_id), *ordinal)
    });
    for (_, object) in objects.iter() {
        let default = rights.default_for(&object.target);
        let dropped_count = object
            .rights
            .iter()
            .filter(|row| row.value == default && row.restrictions.is_empty())
            .count();
        let mut dropped: BudgetedVec<Right> =
            BudgetedVec::try_with_capacity(budget, Category::Metadata, dropped_count)?;
        for row in object
            .rights
            .iter()
            .filter(|row| row.value == default && row.restrictions.is_empty())
        {
            dropped.try_push(row.right)?;
        }
        dropped.sort_unstable();
        let mut written = object
            .rights
            .iter()
            .filter(|row| dropped.binary_search(&row.right).is_err())
            .peekable();
        if written.peek().is_none() {
            continue;
        }
        writer.open("object", &[])?;
        writer.leaf("name", Some(&object.full_name), &[])?;
        for row in written {
            let name = row.right.static_name();
            if name.is_empty() {
                return Ok(None);
            }
            writer.open("right", &[])?;
            writer.leaf("name", Some(name), &[])?;
            writer.leaf("value", Some(boolean(row.value)), &[])?;
            for restriction in &row.restrictions {
                if restriction.condition.is_empty() && restriction.fields.is_empty() {
                    continue;
                }
                let mut fields = BudgetedVec::try_with_capacity(
                    budget,
                    Category::Metadata,
                    restriction.fields.len(),
                )?;
                for name in restriction
                    .fields
                    .iter()
                    .filter_map(|path| path.rsplit('.').next())
                    .filter(|name| !name.is_empty())
                {
                    fields.try_push(name)?;
                }
                fields
                    .sort_unstable_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
                writer.open("restrictionByCondition", &[])?;
                let mut prior = None;
                for name in fields.iter() {
                    if prior != Some(*name) {
                        writer.leaf("field", Some(name), &[])?;
                        prior = Some(*name);
                    }
                }
                writer.leaf("condition", Some(&restriction.condition), &[])?;
                writer.close("restrictionByCondition")?;
            }
            writer.close("right")?;
        }
        writer.close("object")?;
    }
    for (name, condition) in &rights.templates {
        writer.open("restrictionTemplate", &[])?;
        writer.leaf("name", Some(name), &[])?;
        writer.leaf("condition", Some(condition), &[])?;
        writer.close("restrictionTemplate")?;
    }
    writer.finish().map(Some)
}
