use super::index::text;
use crate::XmlElem;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};
const CATEGORY: Category = Category::Metadata;
pub(super) struct Xml(Charged<XmlElem>);
impl Xml {
    pub(super) fn new(
        budget: &MemoryBudget,
        name: &str,
        attrs: usize,
        children: usize,
    ) -> Result<Self, BudgetError> {
        let name = text(budget, &[name])?.into_string()?;
        let attrs = BudgetedVec::try_with_capacity(budget, CATEGORY, attrs)?.into_charged_vec();
        let children =
            BudgetedVec::try_with_capacity(budget, CATEGORY, children)?.into_charged_vec();
        Ok(Self(name.try_zip(attrs)?.try_zip(children)?.map(
            |((name, attrs), children)| XmlElem {
                name,
                attrs,
                text: None,
                children,
                expanded: false,
            },
        )))
    }
    pub(super) fn leaf(
        budget: &MemoryBudget,
        name: &str,
        value: &str,
        attrs: usize,
    ) -> Result<Self, BudgetError> {
        Self::new(budget, name, attrs, 0)?.text(budget, value)
    }
    pub(super) fn text(self, budget: &MemoryBudget, value: &str) -> Result<Self, BudgetError> {
        if self.0.text.is_some() {
            return Err(BudgetError::Overflow);
        }
        let value = text(budget, &[value])?.into_string()?;
        Ok(Self(self.0.try_zip(value)?.map(|(mut node, value)| {
            node.text = Some(value);
            node
        })))
    }
    pub(super) fn attr(
        self,
        budget: &MemoryBudget,
        key: &str,
        value: &str,
    ) -> Result<Self, BudgetError> {
        if self.0.attrs.len() == self.0.attrs.capacity() {
            return Err(BudgetError::Overflow);
        }
        let key = text(budget, &[key])?.into_string()?;
        let value = text(budget, &[value])?.into_string()?;
        Ok(Self(self.0.try_zip(key.try_zip(value)?)?.map(
            |(mut node, attr)| {
                node.attrs.push(attr);
                node
            },
        )))
    }
    pub(super) fn child(self, child: Self) -> Result<Self, BudgetError> {
        if self.0.children.len() == self.0.children.capacity() {
            return Err(BudgetError::Overflow);
        }
        Ok(Self(self.0.try_zip(child.0)?.map(|(mut node, child)| {
            node.children.push(child);
            node
        })))
    }
    pub(super) fn expanded(self) -> Self {
        Self(self.0.map(|mut node| {
            node.expanded = true;
            node
        }))
    }
    pub(super) fn into_charged(self) -> Charged<XmlElem> {
        self.0
    }
}

impl Xml {
    pub(super) fn typed(
        budget: &MemoryBudget,
        tag: &str,
        value: &crate::TypedValue,
    ) -> Result<Self, BudgetError> {
        use crate::TypedValue as T;
        struct Frame<'a> {
            items: &'a [T],
            next: usize,
            root: Xml,
        }
        let mut stack = BudgetedVec::new(budget, CATEGORY)?;
        let mut next = (tag, value);
        loop {
            let (tag, value) = next;
            let mut built = match value {
                T::FixedArray(items) => {
                    let root = Self::new(budget, tag, 1, items.len())?
                        .attr(budget, "xsi:type", "v8:FixedArray")?
                        .expanded();
                    if let Some(first) = items.first() {
                        stack.try_push(Frame {
                            items,
                            next: 1,
                            root,
                        })?;
                        next = ("v8:Value", first);
                        continue;
                    }
                    root
                }
                T::Nil => Self::new(budget, tag, 1, 0)?.attr(budget, "xsi:nil", "true")?,
                T::EmptyTypeDescription => {
                    Self::new(budget, tag, 1, 0)?.attr(budget, "xsi:type", "v8:TypeDescription")?
                }
                _ => {
                    let (kind, text) = match value {
                        T::Str(text) => ("xs:string", text.as_str()),
                        T::Number(text) => ("xs:decimal", text.as_str()),
                        T::Bool(value) => ("xs:boolean", if *value { "true" } else { "false" }),
                        T::Date(text) => ("xs:dateTime", text.as_str()),
                        T::DesignTimeRef(text) => ("xr:DesignTimeRef", text.as_str()),
                        _ => return Err(BudgetError::Overflow),
                    };
                    Self::leaf(budget, tag, text, 1)?.attr(budget, "xsi:type", kind)?
                }
            };
            loop {
                let Some(mut frame) = stack.pop() else {
                    return Ok(built);
                };
                frame.root = frame.root.child(built)?;
                if let Some(child) = frame.items.get(frame.next) {
                    frame.next += 1;
                    next = ("v8:Value", child);
                    stack.try_push(frame)?;
                    break;
                }
                built = frame.root;
            }
        }
    }
}

struct ValueFrame<'a> {
    attrs: &'a [crate::StandardAttribute],
    attr: usize,
    prop: usize,
    root: Xml,
    current: Option<Xml>,
}
enum ValueProgress<'a> {
    Next(ValueFrame<'a>, &'a crate::Prop),
    Done(Xml),
}
impl<'a> ValueFrame<'a> {
    fn advance(
        mut self,
        budget: &MemoryBudget,
        built: Option<Xml>,
    ) -> Result<ValueProgress<'a>, BudgetError> {
        if let Some(built) = built {
            self.current = Some(
                self.current
                    .take()
                    .ok_or(BudgetError::Overflow)?
                    .child(built)?,
            );
            self.prop = self.prop.checked_add(1).ok_or(BudgetError::Overflow)?;
        }
        loop {
            let Some(attr) = self.attrs.get(self.attr) else {
                return Ok(ValueProgress::Done(self.root));
            };
            if self.current.is_none() {
                self.current = Some(
                    Xml::new(budget, "xr:StandardAttribute", 1, attr.props.len())?
                        .attr(budget, "name", &attr.name)?,
                );
            }
            if let Some(prop) = attr.props.get(self.prop) {
                return Ok(ValueProgress::Next(self, prop));
            }
            self.root = self
                .root
                .child(self.current.take().ok_or(BudgetError::Overflow)?)?;
            self.attr = self.attr.checked_add(1).ok_or(BudgetError::Overflow)?;
            self.prop = 0;
        }
    }
}
impl Xml {
    pub(super) fn value(
        budget: &MemoryBudget,
        tag: &str,
        value: &crate::Value,
        description: bool,
    ) -> Result<Self, BudgetError> {
        use crate::Value as V;
        enum Tag<'a> {
            Plain(&'a str),
            Property(&'a str),
        }
        let mut stack = BudgetedVec::new(budget, CATEGORY)?;
        let mut next = (Tag::Plain(tag), value);
        loop {
            let (tag, value) = next;
            let name = match tag {
                Tag::Plain(name) => text(budget, &[name])?,
                Tag::Property(name) => text(budget, &["xr:", name])?,
            };
            let tag = name.as_str();
            let mut built = match value {
                V::Text(value) => Self::description_leaf(budget, tag, value, description)?,
                V::Bool(value) => {
                    Self::leaf(budget, tag, if *value { "true" } else { "false" }, 0)?
                }
                V::Ref(value) => Self::leaf(budget, tag, &value.name, 0)?,
                V::Typed(value) => Self::typed(budget, tag, value)?,
                V::Type(value) => Self::type_description(budget, tag, value)?,
                V::LStr(value) => {
                    let mut root = Self::new(budget, tag, 0, value.0.len())?;
                    for (lang, value) in &value.0 {
                        root = root.child(
                            Self::new(budget, "v8:item", 0, 2)?
                                .child(Self::leaf(budget, "v8:lang", lang, 0)?)?
                                .child(Self::description_leaf(
                                    budget,
                                    "v8:content",
                                    value,
                                    description,
                                )?)?,
                        )?;
                    }
                    root
                }
                V::RefList(values) => {
                    let mut root = Self::new(budget, tag, 0, values.len())?;
                    for value in values {
                        root = root.child(Self::leaf(budget, "xr:Item", &value.name, 1)?.attr(
                            budget,
                            "xsi:type",
                            "xr:MDObjectRef",
                        )?)?
                    }
                    root
                }
                V::FieldList(values) => {
                    let mut root = Self::new(budget, tag, 0, values.len())?;
                    for value in values {
                        root = root.child(Self::leaf(budget, "xr:Field", value, 0)?)?
                    }
                    root
                }
                V::Xml(value) => Self::borrowed(budget, tag, value)?,
                V::StandardAttributes(attrs) => {
                    let frame = ValueFrame {
                        attrs,
                        attr: 0,
                        prop: 0,
                        root: Self::new(budget, tag, 0, attrs.len())?.expanded(),
                        current: None,
                    };
                    match frame.advance(budget, None)? {
                        ValueProgress::Done(root) => root,
                        ValueProgress::Next(frame, prop) => {
                            next = (Tag::Property(&prop.name), &prop.value);
                            stack.try_push(frame)?;
                            continue;
                        }
                    }
                }
            };
            loop {
                let Some(frame) = stack.pop() else {
                    return Ok(built);
                };
                match frame.advance(budget, Some(built))? {
                    ValueProgress::Done(root) => built = root,
                    ValueProgress::Next(frame, prop) => {
                        next = (Tag::Property(&prop.name), &prop.value);
                        stack.try_push(frame)?;
                        break;
                    }
                }
            }
        }
    }
    fn description_leaf(
        budget: &MemoryBudget,
        tag: &str,
        value: &str,
        description: bool,
    ) -> Result<Self, BudgetError> {
        let value = if description {
            super::values::description_text(budget, value)?
        } else {
            text(budget, &[value])?
        };
        Self::leaf(budget, tag, value.as_str(), 0)
    }
    fn borrowed(budget: &MemoryBudget, tag: &str, source: &XmlElem) -> Result<Self, BudgetError> {
        struct Frame<'a> {
            children: &'a [XmlElem],
            next: usize,
            root: Xml,
        }
        let mut stack = BudgetedVec::new(budget, CATEGORY)?;
        let mut next = (tag, source);
        loop {
            let (tag, source) = next;
            let mut root = Self::new(budget, tag, source.attrs.len(), source.children.len())?;
            for (key, value) in &source.attrs {
                root = root.attr(budget, key, value)?
            }
            if let Some(value) = &source.text {
                root = root.text(budget, value)?
            }
            if source.expanded {
                root = root.expanded()
            }
            if let Some(first) = source.children.first() {
                next = (&first.name, first);
                stack.try_push(Frame {
                    children: &source.children,
                    next: 1,
                    root,
                })?;
                continue;
            }
            let mut built = root;
            loop {
                let Some(mut frame) = stack.pop() else {
                    return Ok(built);
                };
                frame.root = frame.root.child(built)?;
                if let Some(child) = frame.children.get(frame.next) {
                    frame.next += 1;
                    next = (&child.name, child);
                    stack.try_push(frame)?;
                    break;
                }
                built = frame.root;
            }
        }
    }
    fn type_description(
        budget: &MemoryBudget,
        tag: &str,
        source: &crate::TypeDescription,
    ) -> Result<Self, BudgetError> {
        let qualifiers = if source.types.is_empty() {
            0
        } else {
            usize::from(source.number.is_some())
                + usize::from(source.string.is_some())
                + usize::from(source.date.is_some())
                + usize::from(source.binary.is_some())
        };
        let capacity = source
            .types
            .len()
            .checked_add(qualifiers)
            .ok_or(BudgetError::Overflow)?;
        let mut root = Self::new(budget, tag, 0, capacity)?;
        for set in [false, true] {
            for item in source.types.iter().filter(|r| r.set == set) {
                root = root.child(Self::leaf(
                    budget,
                    if set { "v8:TypeSet" } else { "v8:Type" },
                    &item.qname,
                    0,
                )?)?
            }
        }
        if source.types.is_empty() {
            return Ok(root);
        }
        let decimal_leaf = |tag: &str, value: u32| -> Result<Self, BudgetError> {
            let mut buffer = [0; 24];
            Self::leaf(
                budget,
                tag,
                super::leaf::decimal(i64::from(value), &mut buffer)?,
                0,
            )
        };
        let length = |fixed| if fixed { "Fixed" } else { "Variable" };
        if let Some(n) = &source.number {
            root = root.child(
                Self::new(budget, "v8:NumberQualifiers", 0, 3)?
                    .child(decimal_leaf("v8:Digits", n.digits)?)?
                    .child(decimal_leaf("v8:FractionDigits", n.fraction)?)?
                    .child(Self::leaf(
                        budget,
                        "v8:AllowedSign",
                        if n.nonnegative { "Nonnegative" } else { "Any" },
                        0,
                    )?)?,
            )?;
        }
        if let Some(s) = &source.string {
            root = root.child(
                Self::new(budget, "v8:StringQualifiers", 0, 2)?
                    .child(decimal_leaf("v8:Length", s.length)?)?
                    .child(Self::leaf(budget, "v8:AllowedLength", length(s.fixed), 0)?)?,
            )?;
        }
        if let Some(d) = &source.date {
            root = root.child(Self::new(budget, "v8:DateQualifiers", 0, 1)?.child(
                Self::leaf(
                    budget,
                    "v8:DateFractions",
                    match d {
                        crate::DateFractions::Date => "Date",
                        crate::DateFractions::Time => "Time",
                        crate::DateFractions::DateTime => "DateTime",
                    },
                    0,
                )?,
            )?)?;
        }
        if let Some(b) = &source.binary {
            root = root.child(
                Self::new(budget, "v8:BinaryDataQualifiers", 0, 2)?
                    .child(decimal_leaf("v8:Length", b.length)?)?
                    .child(Self::leaf(budget, "v8:AllowedLength", length(b.fixed), 0)?)?,
            )?;
        }
        Ok(root)
    }
}
