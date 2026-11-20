use super::decode::Item;
use mdschema::StaticValue;

type Result<T> = std::result::Result<T, &'static str>;

pub(crate) struct TablePolicy<'a> {
    category: &'a str,
    mode: i64,
    source: &'static StaticValue,
}

fn integer(source: &StaticValue, key: &str) -> Result<i64> {
    source
        .get(key)
        .and_then(StaticValue::as_i64)
        .ok_or("invalid command policy integer")
}

pub(crate) fn for_table<'a>(
    item: &Item,
    category: &'a str,
    selection: Option<i64>,
) -> Result<Option<TablePolicy<'a>>> {
    let source = mdschema::static_schema()
        .table("forms/misc.json")
        .and_then(|table| table.get("external_table_commands"))
        .ok_or("missing external command policy")?;
    let categories = source
        .get("categories")
        .and_then(StaticValue::as_array)
        .ok_or("invalid external command categories")?;
    if !categories
        .iter()
        .any(|value| value.as_str() == Some(category))
    {
        return Ok(None);
    }
    if item.class != source.get("class").and_then(StaticValue::as_str)
        || Some(item.version.as_str()) != source.get("version").and_then(StaticValue::as_str)
        || usize::try_from(integer(source, "length")?).ok() != Some(item.body.len())
    {
        return Err("unqualified table command layout");
    }
    let slot = usize::try_from(integer(source, "selection_slot")?)
        .map_err(|_| "invalid selection slot")?;
    let mode = selection
        .or_else(|| item.body.get(slot).and_then(brace::Node::as_i64))
        .ok_or("missing table selection mode")?;
    if mode != integer(source, "single")? && mode != integer(source, "multi")? {
        return Err("unqualified table selection mode");
    }
    Ok(Some(TablePolicy {
        category,
        mode,
        source,
    }))
}
impl TablePolicy<'_> {
    pub fn unavailable(&self, id: &str, name: &str) -> Result<bool> {
        let names = self
            .source
            .get("unavailable")
            .ok_or("missing unavailable commands")?;
        Ok(self.mode == integer(self.source, "single")?
            && names.get(id).and_then(StaticValue::as_str) == Some(name))
    }
    pub fn omits(&self, id: &str) -> Result<bool> {
        let omissions = self
            .source
            .get("omissions")
            .and_then(StaticValue::as_array)
            .ok_or("missing command omissions")?;
        Ok(omissions.iter().any(|rule| {
            rule.get("category").and_then(StaticValue::as_str) == Some(self.category)
                && rule.get("mode").and_then(StaticValue::as_i64) == Some(self.mode)
                && rule.get("guid").and_then(StaticValue::as_str) == Some(id)
        }))
    }
}

fn form_resource() -> Result<&'static StaticValue> {
    mdschema::static_schema()
        .table("forms/misc.json")
        .and_then(|table| table.get("external_form_commands"))
        .ok_or("missing external form command policy")
}

pub(crate) fn form_unavailable(id: &str, name: &str, flag: i64) -> Result<bool> {
    let rule = form_resource()?
        .get("unavailable")
        .ok_or("missing form command availability")?;
    Ok(rule.get("guid").and_then(StaticValue::as_str) == Some(id)
        && rule.get("name").and_then(StaticValue::as_str) == Some(name)
        && integer(rule, "flag")? == flag)
}

pub(crate) struct FormPolicy<'a> {
    source: super::Serialization,
    category: &'a str,
    flag: i64,
    rules: &'static StaticValue,
}

pub(crate) fn for_form<'a>(
    form: &super::decode::DecodedForm,
    source: super::Serialization,
    category: &'a str,
) -> Result<FormPolicy<'a>> {
    let rules = form_resource()?;
    let layout = rules
        .get("normalized")
        .ok_or("missing form command layout")?;
    let actual = form.layout.serialization;
    if i64::from(actual.outer) != integer(layout, "outer")?
        || i64::from(actual.form) != integer(layout, "form")?
        || form.form.version != actual.form.to_string()
        || usize::try_from(integer(layout, "head")?).ok() != Some(form.head().len())
        || usize::try_from(integer(layout, "tail")?).ok() != Some(form.tail().len())
    {
        return Err("unqualified form command layout");
    }
    let slot = usize::try_from(integer(rules, "customizable_slot")?)
        .map_err(|_| "invalid customizable slot")?;
    let flag = form
        .head()
        .get(slot)
        .and_then(brace::Node::as_i64)
        .ok_or("invalid customizable value")?;
    if !matches!(flag, 0 | 1) {
        return Err("invalid customizable value");
    }
    Ok(FormPolicy {
        source,
        category,
        flag,
        rules,
    })
}
impl FormPolicy<'_> {
    pub fn unavailable(&self, id: &str, name: &str) -> Result<bool> {
        let sources = self
            .rules
            .get("known_sources")
            .and_then(StaticValue::as_array)
            .ok_or("missing known command sources")?;
        let qualified = sources.iter().any(|rule| {
            rule.get("outer").and_then(StaticValue::as_i64) == Some(i64::from(self.source.outer))
                && rule.get("form").and_then(StaticValue::as_i64)
                    == Some(i64::from(self.source.form))
        });
        Ok(qualified && form_unavailable(id, name, self.flag)?)
    }
    pub fn omits(&self, id: &str) -> Result<bool> {
        let rule = self
            .rules
            .get("omission")
            .ok_or("missing form command omission")?;
        Ok(integer(rule, "outer")? == i64::from(self.source.outer)
            && integer(rule, "form")? == i64::from(self.source.form)
            && rule.get("category").and_then(StaticValue::as_str) == Some(self.category)
            && integer(rule, "flag")? == self.flag
            && rule.get("guid").and_then(StaticValue::as_str) == Some(id))
    }
}
