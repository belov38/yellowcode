use brace::Node;
use mdschema::{StaticKindSpec, StaticValue, static_schema};

#[path = "overlay_data.rs"]
mod data;

pub fn kind_spec(name: &str) -> Option<StaticKindSpec> {
    data::KINDS
        .binary_search_by_key(&name, |(key, _)| *key)
        .ok()
        .and_then(|at| data::KINDS.get(at))
        .and_then(|(_, value)| StaticKindSpec::from_static(value))
        .or_else(|| static_schema().kind(name))
}

#[derive(Debug, Clone, Copy)]
pub enum ObservedOrder {
    Learned(&'static [&'static str]),
    Schema(&'static [StaticValue]),
}
impl ObservedOrder {
    pub fn iter(&self) -> impl Iterator<Item = &'static str> {
        let (learned, schema) = match self {
            Self::Learned(values) => (*values, &[][..]),
            Self::Schema(values) => (&[][..], *values),
        };
        learned
            .iter()
            .copied()
            .chain(schema.iter().filter_map(StaticValue::as_str))
    }
}
fn learned_order(tag: &str) -> Option<&'static [&'static str]> {
    let at = data::ORDERS
        .binary_search_by_key(&tag, |(key, _)| *key)
        .ok()?;
    data::ORDERS.get(at).map(|(_, values)| *values)
}
pub fn observed_order(tag: &str) -> Option<ObservedOrder> {
    if let Some(values) = learned_order(tag) {
        return Some(ObservedOrder::Learned(values));
    }
    let orders = static_schema().table("order.json")?.get("order")?;
    let values = orders.get(tag).or_else(|| {
        tag.ends_with(".TabularSection.Attribute")
            .then(|| orders.get("Document.TabularSection.Attribute"))
            .flatten()
    })?;
    Some(ObservedOrder::Schema(values.as_array()?))
}
pub fn order_count(context: &str) -> u32 {
    if learned_order(context).is_some()
        || [
            "AccountingRegister.Dimension",
            "AccountingRegister.Resource",
            "AccountingRegister.Attribute",
        ]
        .contains(&context)
        || context.ends_with(".TabularSection.Attribute")
    {
        return 10;
    }
    static_schema()
        .table("order.json")
        .and_then(|value| value.get("counts"))
        .and_then(|value| value.get(context))
        .and_then(StaticValue::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0)
}

pub fn modules(kind: &str) -> impl Iterator<Item = (&'static str, &'static str)> {
    let values = static_schema()
        .table("modules.json")
        .and_then(|value| value.get("modules"))
        .and_then(|value| value.get(kind))
        .and_then(StaticValue::as_object)
        .unwrap_or(&[]);
    values
        .iter()
        .filter_map(|(suffix, file)| Some((*suffix, file.as_str()?)))
}

pub fn special_prop_text<'a>(kind: &str, name: &str, raw: &'a Node) -> Option<&'a str> {
    if kind != "AccountingRegister" || name != "PeriodAdjustmentLength" {
        return None;
    }
    let body = raw.get(&[1])?.as_list()?;
    match body.first()?.as_i64()? {
        21 => Some("0"),
        22 => body.last().filter(|_| body.len() > 1)?.text(),
        _ => None,
    }
}
