use crate::{DUMP_VERSION, NS_DECL};
use md_model::{DateFractions, TypeDescription, TypedValue, Value, XmlElem, XmlTypeName};
use mdschema::Codec;
use memory_budget::{BudgetError, BudgetedText, Category, Charged, MemoryBudget, Reservation};
use std::fmt::Write;

type Result<T> = std::result::Result<T, BudgetError>;

pub struct ManagedXmlWriter {
    out: BudgetedText,
    depth: usize,
    root: &'static str,
    _control: Reservation,
}
impl ManagedXmlWriter {
    pub fn document(budget: &MemoryBudget, limit: u64) -> Result<Self> {
        let control = budget.try_reserve(
            Category::Metadata,
            u64::try_from(std::mem::size_of::<Self>() - std::mem::size_of::<BudgetedText>())
                .map_err(|_| BudgetError::Overflow)?,
        )?;
        let mut out = BudgetedText::new(budget, Category::Metadata, limit)?;
        for text in [
            "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<MetaDataObject ",
            NS_DECL,
            " version=\"",
            DUMP_VERSION,
            "\">",
        ] {
            out.try_push_str(text)?;
        }
        Ok(Self {
            out,
            depth: 1,
            root: "MetaDataObject",
            _control: control,
        })
    }
    pub(crate) fn part_document(
        budget: &MemoryBudget,
        limit: u64,
        root: &'static str,
        attrs: &[(&str, &str)],
    ) -> Result<Self> {
        let control = budget.try_reserve(
            Category::Metadata,
            u64::try_from(std::mem::size_of::<Self>() - std::mem::size_of::<BudgetedText>())
                .map_err(|_| BudgetError::Overflow)?,
        )?;
        let mut out = BudgetedText::new(budget, Category::Metadata, limit)?;
        out.try_push_str(crate::codec::parts::XML_HEAD)?;
        let mut writer = Self {
            out,
            depth: 0,
            root,
            _control: control,
        };
        writer.open(root, attrs)?;
        Ok(writer)
    }
    pub(crate) fn empty_body_line(&mut self) -> Result<()> {
        self.out.try_push_str("\r\n")
    }
    fn line(&mut self) -> Result<()> {
        self.out.try_push_str("\r\n")?;
        for _ in 0..self.depth {
            self.out.try_push('\t')?;
        }
        Ok(())
    }
    fn escaped(&mut self, text: &str, attribute: bool) -> Result<()> {
        let mut start = 0;
        for (at, ch) in text.char_indices() {
            let replacement = match ch {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' if attribute => "&quot;",
                _ => continue,
            };
            self.out
                .try_push_str(text.get(start..at).ok_or(BudgetError::Overflow)?)?;
            self.out.try_push_str(replacement)?;
            start = at.checked_add(ch.len_utf8()).ok_or(BudgetError::Overflow)?;
        }
        self.out
            .try_push_str(text.get(start..).ok_or(BudgetError::Overflow)?)
    }
    fn start<'a>(
        &mut self,
        tag: &str,
        attrs: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<()> {
        self.line()?;
        self.out.try_push('<')?;
        self.out.try_push_str(tag)?;
        for (name, value) in attrs {
            self.out.try_push(' ')?;
            self.out.try_push_str(name)?;
            self.out.try_push_str("=\"")?;
            self.escaped(value, true)?;
            self.out.try_push('"')?;
        }
        Ok(())
    }
    pub fn open(&mut self, tag: &str, attrs: &[(&str, &str)]) -> Result<()> {
        self.start(tag, attrs.iter().copied())?;
        self.out.try_push('>')?;
        self.depth = self.depth.checked_add(1).ok_or(BudgetError::Overflow)?;
        Ok(())
    }
    pub fn close(&mut self, tag: &str) -> Result<()> {
        self.depth = self.depth.checked_sub(1).ok_or(BudgetError::Overflow)?;
        self.line()?;
        self.out.try_push_str("</")?;
        self.out.try_push_str(tag)?;
        self.out.try_push('>')
    }
    pub fn leaf(&mut self, tag: &str, text: Option<&str>, attrs: &[(&str, &str)]) -> Result<()> {
        self.start(tag, attrs.iter().copied())?;
        self.end_leaf(tag, text)
    }
    fn end_leaf(&mut self, tag: &str, text: Option<&str>) -> Result<()> {
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            self.out.try_push('>')?;
            self.escaped(text, false)?;
            self.out.try_push_str("</")?;
            self.out.try_push_str(tag)?;
            self.out.try_push('>')
        } else {
            self.out.try_push_str("/>")
        }
    }
    fn number(&mut self, tag: &str, number: u32) -> Result<()> {
        self.start(tag, [])?;
        self.out.try_push('>')?;
        write!(&mut self.out, "{number}")
            .map_err(|_| self.out.error().copied().unwrap_or(BudgetError::Overflow))?;
        self.out.try_push_str("</")?;
        self.out.try_push_str(tag)?;
        self.out.try_push('>')
    }
    fn elem_as(&mut self, tag: &str, elem: &XmlElem) -> Result<()> {
        self.start(
            tag,
            elem.attrs
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )?;
        if elem.children.is_empty() && !elem.expanded {
            return self.end_leaf(tag, elem.text.as_deref());
        }
        self.out.try_push('>')?;
        self.depth = self.depth.checked_add(1).ok_or(BudgetError::Overflow)?;
        for child in &elem.children {
            self.elem_as(&child.name, child)?;
        }
        self.close(tag)
    }
    pub fn property(&mut self, tag: &str, codec: Codec, value: &Value) -> Result<()> {
        match value {
            Value::Text(text) => self.leaf(tag, Some(text), &[]),
            Value::Bool(value) => self.leaf(tag, Some(boolean(*value)), &[]),
            Value::Ref(value) => self.leaf(tag, Some(&value.name), &[]),
            Value::LStr(value) => {
                if value.0.is_empty() {
                    return self.leaf(tag, None, &[]);
                }
                self.open(tag, &[])?;
                for (lang, text) in &value.0 {
                    self.open("v8:item", &[])?;
                    self.leaf("v8:lang", Some(lang), &[])?;
                    self.leaf("v8:content", Some(text), &[])?;
                    self.close("v8:item")?;
                }
                self.close(tag)
            }
            Value::Type(value) => self.type_description(tag, value),
            Value::Typed(value) => self.typed(tag, value),
            Value::RefList(values) => {
                if values.is_empty() {
                    return self.leaf(tag, None, &[]);
                }
                self.open(tag, &[])?;
                let (item, attrs): (&str, &[(&str, &str)]) = if codec == Codec::FoContent {
                    ("xr:Object", &[])
                } else {
                    ("xr:Item", &[("xsi:type", "xr:MDObjectRef")])
                };
                for value in values {
                    self.leaf(item, Some(&value.name), attrs)?;
                }
                self.close(tag)
            }
            Value::FieldList(values) => {
                if values.is_empty() {
                    return self.leaf(tag, None, &[]);
                }
                self.open(tag, &[])?;
                for value in values {
                    self.leaf("xr:Field", Some(value), &[])?;
                }
                self.close(tag)
            }
            Value::StandardAttributes(values) => {
                self.open(tag, &[])?;
                for value in values {
                    self.open("xr:StandardAttribute", &[("name", &value.name)])?;
                    for prop in &value.props {
                        let mut name = BudgetedText::new(
                            self.out.memory_budget(),
                            Category::Metadata,
                            u64::MAX,
                        )?;
                        name.try_push_str("xr:")?;
                        name.try_push_str(&prop.name)?;
                        self.property(name.as_str(), prop.codec, &prop.value)?;
                    }
                    self.close("xr:StandardAttribute")?;
                }
                self.close(tag)
            }
            Value::Xml(value) => self.elem_as(tag, value),
        }
    }
    pub(crate) fn predefined_type(&mut self, value: &TypeDescription) -> Result<()> {
        self.write_type("Type", value, true)
    }
    fn type_description(&mut self, tag: &str, value: &TypeDescription) -> Result<()> {
        self.write_type(tag, value, false)
    }
    fn type_member(&mut self, tag: &str, name: &XmlTypeName, predefined: bool) -> Result<()> {
        let uri = name.namespace_uri();
        match mdschema::FixedNamespace::from_uri(uri) {
            Some(ns) if !(predefined && ns == mdschema::FixedNamespace::Cfg) => {
                let dcsset =
                    (ns == mdschema::FixedNamespace::Dcsset).then_some(("xmlns:dcsset", uri));
                self.start(tag, dcsset)?;
                self.out.try_push('>')?;
                self.out.try_push_str(ns.prefix())?;
                self.out.try_push(':')?;
            }
            _ => {
                let level = self.depth.checked_add(1).ok_or(BudgetError::Overflow)?;
                self.start(tag, [])?;
                write!(&mut self.out, " xmlns:d{level}p1=\"")
                    .map_err(|_| self.out.error().copied().unwrap_or(BudgetError::Overflow))?;
                self.escaped(uri, true)?;
                write!(&mut self.out, "\">d{level}p1:")
                    .map_err(|_| self.out.error().copied().unwrap_or(BudgetError::Overflow))?;
            }
        }
        self.escaped(&name.local, false)?;
        self.out.try_push_str("</")?;
        self.out.try_push_str(tag)?;
        self.out.try_push('>')
    }
    fn write_type(&mut self, tag: &str, value: &TypeDescription, predefined: bool) -> Result<()> {
        if value.types.is_empty() {
            return self.leaf(tag, None, &[]);
        }
        if value.types.iter().any(|item| {
            mdschema::ExpandedTypeName::new(item.name.namespace_uri(), &item.name.local).is_none()
        }) {
            return Ok(());
        }
        self.open(tag, &[])?;
        for (set, name) in [(false, "v8:Type"), (true, "v8:TypeSet")] {
            for item in value.types.iter().filter(|item| item.set == set) {
                self.type_member(name, &item.name, predefined)?;
            }
        }
        if let Some(number) = &value.number {
            self.open("v8:NumberQualifiers", &[])?;
            self.number("v8:Digits", number.digits)?;
            self.number("v8:FractionDigits", number.fraction)?;
            self.leaf(
                "v8:AllowedSign",
                Some(if number.nonnegative {
                    "Nonnegative"
                } else {
                    "Any"
                }),
                &[],
            )?;
            self.close("v8:NumberQualifiers")?;
        }
        if let Some(string) = &value.string {
            self.open("v8:StringQualifiers", &[])?;
            self.number("v8:Length", string.length)?;
            self.leaf("v8:AllowedLength", Some(allowed(string.fixed)), &[])?;
            self.close("v8:StringQualifiers")?;
        }
        if let Some(date) = &value.date {
            self.open("v8:DateQualifiers", &[])?;
            self.leaf(
                "v8:DateFractions",
                Some(match date {
                    DateFractions::Date => "Date",
                    DateFractions::Time => "Time",
                    DateFractions::DateTime => "DateTime",
                }),
                &[],
            )?;
            self.close("v8:DateQualifiers")?;
        }
        if let Some(binary) = &value.binary {
            self.open("v8:BinaryDataQualifiers", &[])?;
            self.number("v8:Length", binary.length)?;
            self.leaf("v8:AllowedLength", Some(allowed(binary.fixed)), &[])?;
            self.close("v8:BinaryDataQualifiers")?;
        }
        self.close(tag)
    }
    fn typed(&mut self, tag: &str, value: &TypedValue) -> Result<()> {
        let (kind, text) = match value {
            TypedValue::Nil => return self.leaf(tag, None, &[("xsi:nil", "true")]),
            TypedValue::EmptyTypeDescription => {
                return self.leaf(tag, None, &[("xsi:type", "v8:TypeDescription")]);
            }
            TypedValue::FixedArray(items) => {
                self.open(tag, &[("xsi:type", "v8:FixedArray")])?;
                for item in items {
                    self.typed("v8:Value", item)?;
                }
                return self.close(tag);
            }
            TypedValue::Str(text) => ("xs:string", text.as_str()),
            TypedValue::Number(text) => ("xs:decimal", text.as_str()),
            TypedValue::Bool(value) => ("xs:boolean", boolean(*value)),
            TypedValue::Date(text) => ("xs:dateTime", text.as_str()),
            TypedValue::DesignTimeRef(text) => ("xr:DesignTimeRef", text.as_str()),
        };
        self.leaf(tag, Some(text), &[("xsi:type", kind)])
    }
    pub fn finish(mut self) -> Result<Charged<String>> {
        if self.depth != 1 {
            return Err(BudgetError::Overflow);
        }
        self.close(self.root)?;
        self.out.into_string()
    }
}
fn boolean(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
fn allowed(fixed: bool) -> &'static str {
    if fixed { "Fixed" } else { "Variable" }
}
