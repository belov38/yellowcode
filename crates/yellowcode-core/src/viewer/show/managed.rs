use crate::viewer::{
    Lang, body::ViewerBody, configurator::borrowed::palette_prop, names::ManagedNames,
};
use mdconf::{DateFractions, MdRef, Prop, TypeDescription, TypeRef, TypedValue, Value, XmlElem};
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, Charged, LimitReason, MemoryBudget,
    MemoryLimit,
};
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};

type Result<T> = std::result::Result<T, BudgetError>;
pub struct ManagedProperties<'a> {
    rows: BudgetedVec<Property<'a>>,
    budget: &'a MemoryBudget,
}
pub fn properties_managed<'a, T>(
    owner: &'a Charged<T>,
    select: impl FnOnce(&'a T) -> &'a [Prop],
    names: &'a ManagedNames<'_>,
    type_en: &str,
    context: &str,
    lang: Lang,
) -> Result<ManagedProperties<'a>> {
    let budget = owner.reservation().budget();
    if !budget.same_scope(names.memory_budget()) {
        return Err(BudgetError::Limit(MemoryLimit {
            category: Category::Metadata,
            requested: 0,
            reason: LimitReason::InvalidSize,
        }));
    }
    Ok(ManagedProperties {
        rows: project(select(owner), names, type_en, context, lang, budget)?,
        budget,
    })
}
impl<'a> ManagedProperties<'a> {
    pub fn to_body(&self, wire_limit: u64) -> Result<ViewerBody> {
        crate::viewer::body::json(self.budget, wire_limit, self)
    }
    pub(crate) fn rows(&self) -> &[Property<'a>] {
        self.rows.as_slice()
    }
}

impl Serialize for ManagedProperties<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.rows.as_slice().serialize(serializer)
    }
}
pub(crate) struct Property<'a> {
    name: &'a str,
    title: &'a str,
    value: View<'a>,
    text: BudgetedText,
    group: Option<&'static str>,
    view: Option<&'static str>,
    tooltip: Option<&'static str>,
}
impl Property<'_> {
    pub(crate) fn name(&self) -> &str {
        self.name
    }
}
impl Serialize for Property<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(
            4 + usize::from(self.group.is_some())
                + usize::from(self.view.is_some())
                + usize::from(self.tooltip.is_some()),
        ))?;
        map.serialize_entry("name", self.name)?;
        map.serialize_entry("title", self.title)?;
        map.serialize_entry("value", &self.value)?;
        map.serialize_entry("text", self.text.as_str())?;
        if let Some(group) = self.group {
            map.serialize_entry("group", group)?;
        }
        if let Some(view) = self.view {
            map.serialize_entry("view", view)?;
        }
        if let Some(tooltip) = self.tooltip {
            map.serialize_entry("tooltip", tooltip)?;
        }
        map.end()
    }
}
struct Standard<'a> {
    name: &'a str,
    rows: BudgetedVec<Property<'a>>,
}
impl Serialize for Standard<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("name", self.name)?;
        map.serialize_entry("props", self.rows.as_slice())?;
        map.end()
    }
}
enum View<'a> {
    Text(&'a str),
    Bool(bool),
    Local(BudgetedVec<(&'a str, &'a str)>),
    Ref(&'a str),
    Type(&'a TypeDescription),
    Typed(&'a TypedValue),
    Refs(&'a [MdRef]),
    Fields(&'a [String]),
    Standards(BudgetedVec<Standard<'a>>),
    Xml(BudgetedText),
}
impl Serialize for View<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Text(text) => text.serialize(serializer),
            Self::Bool(value) => value.serialize(serializer),
            Self::Local(rows) => {
                let mut map = serializer.serialize_map(None)?;
                for (lang, text) in rows.iter() {
                    map.serialize_entry(lang, text)?;
                }
                map.end()
            }
            Self::Type(value) => TypeJson(value).serialize(serializer),
            Self::Ref(text) => single(serializer, "ref", text),
            Self::Typed(value) => single(serializer, "typed", &TypedJson(value)),
            Self::Refs(rows) => single(serializer, "refs", &References(rows)),
            Self::Fields(rows) => single(serializer, "fields", rows),
            Self::Standards(rows) => single(serializer, "standard_attributes", rows.as_slice()),
            Self::Xml(text) => single(serializer, "xml", text.as_str()),
        }
    }
}
fn single<S: Serializer, T: Serialize + ?Sized>(
    serializer: S,
    key: &str,
    value: &T,
) -> std::result::Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(1))?;
    map.serialize_entry(key, value)?;
    map.end()
}
struct References<'a>(&'a [MdRef]);
impl Serialize for References<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for reference in self.0 {
            seq.serialize_element(&reference.name)?;
        }
        seq.end()
    }
}
struct TypeNames<'a>(&'a [TypeRef]);
impl Serialize for TypeNames<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for reference in self.0 {
            seq.serialize_element(&format_args!("{}", reference.name))?;
        }
        seq.end()
    }
}
struct TypeJson<'a>(&'a TypeDescription);
#[derive(Serialize)]
struct Bound {
    length: u32,
    fixed: bool,
}
#[derive(Serialize)]
struct Number {
    digits: u32,
    fraction: u32,
    nonnegative: bool,
}
impl Serialize for TypeJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let value = self.0;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("types", &TypeNames(&value.types))?;
        if let Some(q) = value.number {
            map.serialize_entry(
                "number",
                &Number {
                    digits: q.digits,
                    fraction: q.fraction,
                    nonnegative: q.nonnegative,
                },
            )?;
        }
        if let Some(q) = value.string {
            map.serialize_entry(
                "string",
                &Bound {
                    length: q.length,
                    fixed: q.fixed,
                },
            )?;
        }
        if let Some(q) = value.date {
            map.serialize_entry(
                "date",
                match q {
                    DateFractions::Date => "Date",
                    DateFractions::Time => "Time",
                    DateFractions::DateTime => "DateTime",
                },
            )?;
        }
        if let Some(q) = value.binary {
            map.serialize_entry(
                "binary",
                &Bound {
                    length: q.length,
                    fixed: q.fixed,
                },
            )?;
        }
        map.end()
    }
}
struct TypedJson<'a>(&'a TypedValue);
impl Serialize for TypedJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            TypedValue::Nil | TypedValue::EmptyTypeDescription => serializer.serialize_none(),
            TypedValue::Str(s)
            | TypedValue::Number(s)
            | TypedValue::Date(s)
            | TypedValue::DesignTimeRef(s) => s.serialize(serializer),
            TypedValue::Bool(value) => value.serialize(serializer),
            TypedValue::FixedArray(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(&TypedJson(value))?;
                }
                seq.end()
            }
        }
    }
}
fn project<'a>(
    props: &'a [Prop],
    names: &'a ManagedNames<'_>,
    type_en: &str,
    context: &str,
    lang: Lang,
    budget: &MemoryBudget,
) -> Result<BudgetedVec<Property<'a>>> {
    let mut rows = BudgetedVec::try_with_capacity(budget, Category::Metadata, props.len())?;
    for prop in props {
        let mut text = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
        let value = project_value(&prop.value, names, lang, budget, &mut text)?;
        let palette = palette_prop(context, &prop.name);
        rows.try_push(Property {
            name: &prop.name,
            title: palette
                .map(|q| q.caption(lang))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| names.property_name_of(type_en, &prop.name, lang)),
            value,
            text,
            group: palette.and_then(|q| q.group(lang)),
            view: palette.map(|q| q.view).filter(|s| !s.is_empty()),
            tooltip: palette
                .filter(|_| lang == Lang::Ru)
                .map(|q| q.tooltip_ru)
                .filter(|s| !s.is_empty()),
        })?;
    }
    Ok(rows)
}
fn yes_no(value: bool, lang: Lang) -> &'static str {
    match (value, lang) {
        (true, Lang::Ru) => "Да",
        (false, Lang::Ru) => "Нет",
        (true, Lang::En) => "Yes",
        (false, Lang::En) => "No",
    }
}
fn separated<'a>(out: &mut BudgetedText, values: impl Iterator<Item = &'a str>) -> Result<()> {
    for (at, value) in values.enumerate() {
        if at != 0 {
            out.try_push_str(", ")?;
        }
        out.try_push_str(value)?;
    }
    Ok(())
}
fn typed_text(value: &TypedValue, lang: Lang, out: &mut BudgetedText) -> Result<()> {
    match value {
        TypedValue::Nil | TypedValue::EmptyTypeDescription => Ok(()),
        TypedValue::Str(s)
        | TypedValue::Number(s)
        | TypedValue::Date(s)
        | TypedValue::DesignTimeRef(s) => out.try_push_str(s),
        TypedValue::Bool(value) => out.try_push_str(yes_no(*value, lang)),
        TypedValue::FixedArray(values) => {
            for (at, value) in values.iter().enumerate() {
                if at != 0 {
                    out.try_push_str(", ")?;
                }
                typed_text(value, lang, out)?;
            }
            Ok(())
        }
    }
}
fn escaped(text: &str, out: &mut BudgetedText) -> Result<()> {
    for c in text.chars() {
        match c {
            '&' => out.try_push_str("&amp;")?,
            '<' => out.try_push_str("&lt;")?,
            '>' => out.try_push_str("&gt;")?,
            '"' => out.try_push_str("&quot;")?,
            _ => out.try_push(c)?,
        }
    }
    Ok(())
}
fn xml(value: &XmlElem, out: &mut BudgetedText) -> Result<()> {
    out.try_push_str("<")?;
    out.try_push_str(&value.name)?;
    for (name, text) in &value.attrs {
        out.try_push_str(" ")?;
        out.try_push_str(name)?;
        out.try_push_str("=\"")?;
        escaped(text, out)?;
        out.try_push_str("\"")?;
    }
    if value.children.is_empty() && value.text.is_none() {
        out.try_push_str("/>")?;
        return Ok(());
    }
    out.try_push_str(">")?;
    if let Some(text) = &value.text {
        escaped(text, out)?;
    }
    for child in &value.children {
        xml(child, out)?;
    }
    out.try_push_str("</")?;
    out.try_push_str(&value.name)?;
    out.try_push_str(">")
}
fn project_value<'a>(
    value: &'a Value,
    names: &'a ManagedNames<'_>,
    lang: Lang,
    budget: &MemoryBudget,
    text: &mut BudgetedText,
) -> Result<View<'a>> {
    Ok(match value {
        Value::Text(value) => {
            text.try_push_str(value)?;
            View::Text(value)
        }
        Value::Bool(value) => {
            text.try_push_str(yes_no(*value, lang))?;
            View::Bool(*value)
        }
        Value::LStr(value) => {
            let mut rows = BudgetedVec::<(&str, &str)>::try_with_capacity(
                budget,
                Category::Metadata,
                value.0.len(),
            )?;
            for (language, value) in &value.0 {
                if let Some(row) = rows.as_mut_slice().iter_mut().find(|row| row.0 == language) {
                    row.1 = value.as_str();
                } else {
                    rows.try_push((language.as_str(), value.as_str()))?;
                }
            }
            let code = if lang == Lang::Ru { "ru" } else { "en" };
            if let Some((_, value)) = value
                .0
                .iter()
                .find(|(language, _)| language == code)
                .or_else(|| value.0.first())
            {
                text.try_push_str(value)?;
            }
            View::Local(rows)
        }
        Value::Ref(value) => {
            text.try_push_str(&value.name)?;
            View::Ref(&value.name)
        }
        Value::Type(value) => {
            names.write_type_text(value, lang, text)?;
            View::Type(value)
        }
        Value::Typed(value) => {
            typed_text(value, lang, text)?;
            View::Typed(value)
        }
        Value::RefList(values) => {
            separated(text, values.iter().map(|value| value.name.as_str()))?;
            View::Refs(values)
        }
        Value::FieldList(values) => {
            separated(text, values.iter().map(String::as_str))?;
            View::Fields(values)
        }
        Value::StandardAttributes(values) => {
            let mut rows =
                BudgetedVec::try_with_capacity(budget, Category::Metadata, values.len())?;
            for value in values {
                rows.try_push(Standard {
                    name: &value.name,
                    rows: project(
                        &value.props,
                        names,
                        "StandardAttribute",
                        "StandardAttribute",
                        lang,
                        budget,
                    )?,
                })?;
            }
            separated(text, values.iter().map(|value| value.name.as_str()))?;
            View::Standards(rows)
        }
        Value::Xml(value) => {
            let mut encoded = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
            xml(value, &mut encoded)?;
            if let Some(value) = &value.text {
                text.try_push_str(value)?;
            } else {
                text.try_push_str("<")?;
                text.try_push_str(&value.name)?;
                text.try_push_str(">")?;
            }
            View::Xml(encoded)
        }
    })
}
