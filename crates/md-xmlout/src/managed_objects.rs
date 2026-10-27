use crate::{
    ManagedXmlWriter,
    codec::objects::{child_generated_kind, child_tag_name},
};
use brace::Guid;
use md_model::{Child, GeneratedType, MdObject, Prop, Recalculation, SubKind, SubObject};
use mdschema::{StaticKindSpec, StaticValue, static_schema};
use memory_budget::{BudgetError, BudgetedText, Category, Charged, MemoryBudget};
use std::fmt::Write;

type Result<T> = std::result::Result<T, BudgetError>;
fn text(budget: &MemoryBudget, parts: &[&str]) -> Result<BudgetedText> {
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for part in parts {
        out.try_push_str(part)?;
    }
    Ok(out)
}
fn guid(budget: &MemoryBudget, value: Guid) -> Result<BudgetedText> {
    let mut out = BudgetedText::try_with_capacity(budget, Category::Metadata, 36, 36)?;
    write!(&mut out, "{value}")
        .map_err(|_| out.error().copied().unwrap_or(BudgetError::Overflow))?;
    Ok(out)
}
fn props(writer: &mut ManagedXmlWriter, values: &[Prop]) -> Result<()> {
    writer.open("Properties", &[])?;
    for prop in values {
        writer.property(&prop.name, prop.codec, &prop.value)?;
    }
    writer.close("Properties")
}
fn xdto(name: &str) -> Option<&'static [StaticValue]> {
    static_schema().table("xdto_md.json")?.get(name)?.as_array()
}
fn object(value: Option<&'static StaticValue>) -> &'static [(&'static str, StaticValue)] {
    value.and_then(StaticValue::as_object).unwrap_or(&[])
}
fn internal_info(
    writer: &mut ManagedXmlWriter,
    budget: &MemoryBudget,
    kind: &str,
    fullname: &str,
    this_node: Option<Guid>,
    types: &[GeneratedType],
) -> Result<()> {
    if types.is_empty() && this_node.is_none() {
        return Ok(());
    }
    writer.open("InternalInfo", &[])?;
    if let Some(value) = this_node {
        writer.leaf("xr:ThisNode", Some(guid(budget, value)?.as_str()), &[])?;
    }
    for value in types {
        let prefix = static_schema()
            .table("gen.json")
            .and_then(|table| table.get("gt_name"))
            .and_then(|table| table.get(&value.category))
            .and_then(StaticValue::as_str)
            .filter(|name| !name.is_empty());
        let name = match prefix {
            Some(prefix) => text(budget, &[prefix, ".", fullname])?,
            None => text(budget, &[kind, &value.category, ".", fullname])?,
        };
        writer.open(
            "xr:GeneratedType",
            &[("name", name.as_str()), ("category", &value.category)],
        )?;
        writer.leaf(
            "xr:TypeId",
            Some(guid(budget, value.type_id)?.as_str()),
            &[],
        )?;
        writer.leaf(
            "xr:ValueId",
            Some(guid(budget, value.value_id)?.as_str()),
            &[],
        )?;
        writer.close("xr:GeneratedType")?;
    }
    writer.close("InternalInfo")
}
pub fn managed_configuration_xml<'a>(
    budget: &MemoryBudget,
    id: Guid,
    values: &[Prop],
    contained: &[(Guid, Option<Guid>)],
    names: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    limit: u64,
) -> Result<Option<Charged<String>>> {
    if values.is_empty() {
        return Ok(None);
    }
    let mut writer = ManagedXmlWriter::document(budget, limit)?;
    writer.open("Configuration", &[("uuid", guid(budget, id)?.as_str())])?;
    writer.open("InternalInfo", &[])?;
    for (class, value) in contained {
        writer.open("xr:ContainedObject", &[])?;
        writer.leaf("xr:ClassId", Some(guid(budget, *class)?.as_str()), &[])?;
        let value = value.map(|value| guid(budget, value)).transpose()?;
        writer.leaf("xr:ObjectId", value.as_ref().map(BudgetedText::as_str), &[])?;
        writer.close("xr:ContainedObject")?;
    }
    writer.close("InternalInfo")?;
    props(&mut writer, values)?;
    writer.open("ChildObjects", &[])?;
    for prop in xdto("ConfigurationChildObjects").unwrap_or(&[]) {
        let tag = prop
            .at(0)
            .and_then(StaticValue::as_str)
            .ok_or(BudgetError::Overflow)?;
        for (_, name) in names.clone().filter(|(kind, _)| *kind == tag) {
            writer.leaf(tag, Some(name), &[])?;
        }
    }
    writer.close("ChildObjects")?;
    writer.close("Configuration")?;
    writer.finish().map(Some)
}
struct Named<'a, I> {
    forms: &'a [SubObject],
    templates: &'a [SubObject],
    recalculations: &'a [Recalculation],
    nested: I,
}
fn child_spec(
    specs: &'static [(&'static str, StaticValue)],
    tag: &str,
) -> Option<&'static StaticValue> {
    specs
        .iter()
        .rev()
        .find(|(_, spec)| spec.get("tag").and_then(StaticValue::as_str) == Some(tag))
        .map(|(_, spec)| spec)
}
fn child(
    writer: &mut ManagedXmlWriter,
    budget: &MemoryBudget,
    kind: &str,
    owner: &str,
    spec: &'static StaticValue,
    value: &Child,
) -> Result<()> {
    let tag = child_tag_name(value.tag);
    writer.open(tag, &[("uuid", guid(budget, value.id)?.as_str())])?;
    let name = text(budget, &[owner, ".", &value.name])?;
    let spec_tag = spec
        .get("tag")
        .and_then(StaticValue::as_str)
        .ok_or(BudgetError::Overflow)?;
    internal_info(
        writer,
        budget,
        child_generated_kind(kind, spec_tag),
        name.as_str(),
        None,
        &value.generated_types,
    )?;
    props(writer, &value.props)?;
    let specs = object(spec.get("children"));
    if !specs.is_empty() || tag == "TabularSection" {
        let co = text(budget, &[tag, "ChildObjects"])?;
        children(
            writer,
            budget,
            kind,
            owner,
            co.as_str(),
            specs,
            &value.children,
            &[],
            &Named {
                forms: &[],
                templates: &[],
                recalculations: &[],
                nested: std::iter::empty(),
            },
        )?;
    }
    writer.close(tag)
}
#[allow(
    clippy::too_many_arguments,
    reason = "borrowed document context and selected groups share one lifetime"
)]
fn children<'a>(
    writer: &mut ManagedXmlWriter,
    budget: &MemoryBudget,
    kind: &str,
    owner: &str,
    co: &str,
    specs: &'static [(&'static str, StaticValue)],
    values: &[Child],
    subs: &'static [(&'static str, StaticValue)],
    named: &Named<'_, impl Iterator<Item = &'a str> + Clone>,
) -> Result<()> {
    let groups = xdto(co).unwrap_or(&[]);
    let mut any = false;
    for prop in groups {
        let tag = prop
            .at(0)
            .and_then(StaticValue::as_str)
            .ok_or(BudgetError::Overflow)?;
        let found = if child_spec(specs, tag).is_some() {
            values.iter().any(|value| child_tag_name(value.tag) == tag)
        } else if tag == "Recalculation" {
            !named.recalculations.is_empty()
        } else if subs.iter().any(|(name, _)| *name == tag) {
            if tag == kind {
                named.nested.clone().next().is_some()
            } else if tag == "Form" {
                !named.forms.is_empty()
            } else if tag == "Template" {
                !named.templates.is_empty()
            } else {
                false
            }
        } else {
            false
        };
        any |= found;
    }
    if !any {
        return writer.leaf("ChildObjects", None, &[]);
    }
    writer.open("ChildObjects", &[])?;
    for prop in groups {
        let tag = prop
            .at(0)
            .and_then(StaticValue::as_str)
            .ok_or(BudgetError::Overflow)?;
        if let Some(spec) = child_spec(specs, tag) {
            for value in values
                .iter()
                .filter(|value| child_tag_name(value.tag) == tag)
            {
                child(writer, budget, kind, owner, spec, value)?;
            }
        } else if tag == "Recalculation" {
            for value in named.recalculations {
                writer.leaf(tag, Some(&value.name), &[])?;
            }
        } else if subs.iter().any(|(name, _)| *name == tag) {
            if tag == kind {
                for name in named.nested.clone() {
                    writer.leaf(tag, Some(name), &[])?;
                }
            } else if tag == "Form" {
                for value in named.forms {
                    writer.leaf(tag, Some(&value.name), &[])?;
                }
            } else if tag == "Template" {
                for value in named.templates {
                    writer.leaf(tag, Some(&value.name), &[])?;
                }
            }
        }
    }
    writer.close("ChildObjects")
}
pub fn managed_object_xml<'a>(
    budget: &MemoryBudget,
    value: &MdObject,
    spec: StaticKindSpec,
    nested: impl Iterator<Item = &'a str> + Clone,
    limit: u64,
) -> Result<Option<Charged<String>>> {
    if spec.name() != value.kind.as_str() {
        return Err(BudgetError::Overflow);
    }
    if spec.header().is_none() {
        return Ok(None);
    }
    let kind = value.kind.as_str();
    let mut writer = ManagedXmlWriter::document(budget, limit)?;
    writer.open(kind, &[("uuid", guid(budget, value.id)?.as_str())])?;
    internal_info(
        &mut writer,
        budget,
        kind,
        &value.name,
        value.this_node,
        &value.generated_types,
    )?;
    props(&mut writer, &value.props)?;
    let co = text(budget, &[kind, "ChildObjects"])?;
    if xdto(co.as_str()).is_some() {
        children(
            &mut writer,
            budget,
            kind,
            &value.name,
            co.as_str(),
            object(spec.field("children")),
            &value.children,
            object(spec.field("subobjects")),
            &Named {
                forms: &value.forms,
                templates: &value.templates,
                recalculations: &value.recalculations,
                nested,
            },
        )?;
    }
    writer.close(kind)?;
    writer.finish().map(Some)
}
pub fn managed_subobject_xml(
    budget: &MemoryBudget,
    value: &SubObject,
    limit: u64,
) -> Result<Option<Charged<String>>> {
    let tag = match value.kind {
        SubKind::Form => "Form",
        SubKind::Template => "Template",
    };
    if static_schema().kind(tag).is_none() {
        return Ok(None);
    }
    let mut writer = ManagedXmlWriter::document(budget, limit)?;
    writer.open(tag, &[("uuid", guid(budget, value.id)?.as_str())])?;
    props(&mut writer, &value.props)?;
    writer.close(tag)?;
    writer.finish().map(Some)
}
pub fn managed_recalculation_xml(
    budget: &MemoryBudget,
    register_name: &str,
    value: &Recalculation,
    limit: u64,
) -> Result<Charged<String>> {
    let mut writer = ManagedXmlWriter::document(budget, limit)?;
    writer.open(
        "Recalculation",
        &[("uuid", guid(budget, value.id)?.as_str())],
    )?;
    let full = text(budget, &[register_name, ".", &value.name])?;
    internal_info(
        &mut writer,
        budget,
        "Recalculation",
        full.as_str(),
        None,
        &value.generated_types,
    )?;
    props(&mut writer, &value.props)?;
    if value.dimensions.is_empty() {
        writer.leaf("ChildObjects", None, &[])?;
    } else {
        writer.open("ChildObjects", &[])?;
        for dimension in &value.dimensions {
            writer.open(
                "Dimension",
                &[("uuid", guid(budget, dimension.id)?.as_str())],
            )?;
            props(&mut writer, &dimension.props)?;
            writer.close("Dimension")?;
        }
        writer.close("ChildObjects")?;
    }
    writer.close("Recalculation")?;
    writer.finish()
}
