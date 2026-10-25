use super::{
    index::text,
    values::{append, empty},
};
use crate::EntryData;
use crate::forms::variant::{self, Layout, NoLayout};
use crate::forms::{
    Form, FormAttribute, FormCommand, FormItem, FormParameter, FormSource, Serialization, decode,
};
use brace::Node;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget, Shared};
const CATEGORY: Category = Category::Metadata;
macro_rules! present {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}
#[derive(Debug)]
pub struct ManagedForm {
    pub form: Option<Charged<Form>>,
    pub failed: bool,
    pub serialization: Option<Serialization>,
    pub supported: bool,
}
impl ManagedForm {
    pub fn decode(data: &Shared<EntryData>, keep_source: bool) -> Result<Self, BudgetError> {
        let absent = Self {
            form: None,
            failed: false,
            serialization: None,
            supported: false,
        };
        let EntryData::Brace(doc) = &**data else {
            return Ok(absent);
        };
        if !doc.bom || !doc.lead.is_empty() {
            return Ok(absent);
        }
        let budget = data.reservation().budget();
        let serialization = Serialization::of(&doc.root);
        let layout = match variant::detect(&doc.root) {
            Ok(layout) => layout,
            Err(no) => {
                return Ok(Self {
                    form: None,
                    failed: true,
                    serialization,
                    supported: !matches!(no, NoLayout::Unsupported(_)),
                });
            }
        };
        let Some(form) = summary(budget, &doc.root, layout)? else {
            return Ok(Self {
                form: None,
                failed: true,
                serialization,
                supported: true,
            });
        };
        let source = if keep_source {
            FormSource::ManagedDoc(data.clone())
        } else {
            FormSource::ManagedText(
                brace::write_doc_budgeted(doc, budget, CATEGORY)?.try_into_shared()?,
            )
        };
        Ok(Self {
            form: Some(form.map(|mut form| {
                form.source = Some(source);
                form
            })),
            failed: false,
            serialization,
            supported: true,
        })
    }
}
fn vector<T>(budget: &MemoryBudget, count: usize) -> Result<Charged<Vec<T>>, BudgetError> {
    Ok(BudgetedVec::try_with_capacity(budget, CATEGORY, count)?.into_charged_vec())
}
fn string(budget: &MemoryBudget, value: &str) -> Result<Charged<String>, BudgetError> {
    text(budget, &[value])?.into_string()
}
#[derive(Clone, Copy)]
struct Spec<'a> {
    raw: &'a Node,
    class: Option<&'static str>,
    parent: Option<(&'static str, usize)>,
    layout: &'static Layout,
}
struct Header {
    id: i64,
    name: String,
    tag: String,
}
struct Shape<'a> {
    header: Charged<Header>,
    children: BudgetedVec<Spec<'a>>,
}
fn remove<T>(values: &mut BudgetedVec<T>, start: usize, end: usize) -> Option<()> {
    let length = values.len();
    let count = end.checked_sub(start)?;
    if end > length {
        return None;
    }
    values.get_mut(start..)?.rotate_left(count);
    values.truncate(length.checked_sub(count)?);
    Some(())
}
fn shape<'a>(budget: &MemoryBudget, spec: Spec<'a>) -> Result<Option<Shape<'a>>, BudgetError> {
    let raw = present!(spec.raw.as_list());
    let version = present!(raw.first().and_then(Node::text));
    let id = present!(raw.get(1).and_then(|n| n.at(0)).and_then(Node::text))
        .parse()
        .unwrap_or(0);
    let mut body = BudgetedVec::try_with_capacity(budget, CATEGORY, raw.len())?;
    let mut nested = BudgetedVec::new(budget, CATEGORY)?;
    let mut inline = BudgetedVec::new(budget, CATEGORY)?;
    let mut at = 0;
    while let Some(node) = raw.get(at) {
        if let Some(class) = node.text().and_then(decode::class_of)
            && let Some(next) = raw.get(at + 1).filter(|node| decode::is_item(node))
        {
            present!(body.len().checked_sub(1));
            inline.try_push(Spec {
                raw: next,
                class: Some(class),
                parent: None,
                layout: spec.layout,
            })?;
            at += 2;
        } else if decode::is_item(node) {
            nested.try_push((body.len(), nested.len(), node))?;
            body.try_push(None)?;
            at += 1;
        } else {
            body.try_push(Some(node))?;
            at += 1;
        }
    }
    let mut shift = |start: usize, delta: isize| {
        for (position, _, _) in nested.as_mut_slice() {
            if *position >= start {
                *position = position.checked_add_signed(delta).unwrap_or(*position);
            }
        }
    };
    let uv = if spec.class == Some("Button") { 3 } else { 4 };
    if body.get(uv).copied().flatten().and_then(Node::text) == Some("1")
        && body
            .get(uv + 1)
            .copied()
            .flatten()
            .is_some_and(Node::is_list)
    {
        present!(remove(&mut body, uv + 1, uv + 2));
        shift(uv + 1, -1);
    }
    if let Some(position) = spec.layout.bag_at(spec.class, version)
        && let Some(count) = body.get(position)
    {
        let count = present!(
            count
                .and_then(Node::as_i64)
                .and_then(|n| usize::try_from(n).ok())
        );
        if count > body.len() {
            return Ok(None);
        }
        let cells = present!(count.checked_mul(2));
        let end = present!(position.checked_add(1).and_then(|n| n.checked_add(cells)));
        for offset in 0..count {
            present!(
                body.get(position + 1 + 2 * offset)
                    .and_then(|node| { node.map_or(Some(decode::NESTED), Node::text) })
            );
            present!(body.get(position + 2 + 2 * offset));
        }
        present!(remove(&mut body, position + 1, end));
        shift(position + 1, -present!(isize::try_from(cells).ok()));
    }
    let name = body
        .get(2..12.min(body.len()))
        .unwrap_or_default()
        .iter()
        .filter_map(|node| *node)
        .find_map(Node::as_str)
        .unwrap_or_default();
    if !spec.layout.accepts_item(spec.class, version, body.len()) {
        return Ok(None);
    }
    let tag = match spec.class {
        Some(class) if decode::in_kind(class) => {
            let code = body
                .get(5)
                .copied()
                .flatten()
                .and_then(Node::text)
                .unwrap_or("");
            match decode::kind(class, code) {
                Some(tag) => tag,
                None if spec.layout.strict => return Ok(None),
                None => class,
            }
        }
        Some(class) => class,
        None => spec
            .parent
            .and_then(|(class, position)| {
                decode::nested_at(if class == "nested" { "Addition" } else { class }, position)
            })
            .unwrap_or("AutoCommandBar"),
    };
    let header = string(budget, name)?
        .try_zip(string(budget, tag)?)?
        .map(|(name, tag)| Header { id, name, tag });
    nested.sort_unstable_by_key(|(position, order, _)| (*position, *order));
    let count = nested
        .len()
        .checked_add(inline.len())
        .ok_or(BudgetError::Overflow)?;
    let mut children = BudgetedVec::try_with_capacity(budget, CATEGORY, count)?;
    for (position, _, raw) in nested.as_slice() {
        children.try_push(Spec {
            raw,
            class: None,
            parent: Some((spec.class.unwrap_or("nested"), *position)),
            layout: spec.layout,
        })?;
    }
    for child in inline.as_slice() {
        children.try_push(Spec {
            layout: spec.layout,
            ..*child
        })?;
    }
    Ok(Some(Shape { header, children }))
}
struct Frame<'a> {
    shape: Shape<'a>,
    depth: usize,
    next: usize,
    children: Option<Charged<Vec<FormItem>>>,
}
fn item(budget: &MemoryBudget, root: Spec<'_>) -> Result<Option<Charged<FormItem>>, BudgetError> {
    let mut stack = BudgetedVec::new(budget, CATEGORY)?;
    let mut pending = Some((root, 0));
    loop {
        if let Some((spec, depth)) = pending.take() {
            if depth > 256 {
                return Ok(None);
            }
            let shape = present!(shape(budget, spec)?);
            let children = Some(vector(budget, shape.children.len())?);
            stack.try_push(Frame {
                shape,
                depth,
                next: 0,
                children,
            })?;
        }
        let frame = stack.last_mut().ok_or(BudgetError::Overflow)?;
        if let Some(spec) = frame.shape.children.get(frame.next).copied() {
            frame.next += 1;
            pending = Some((spec, frame.depth + 1));
            continue;
        }
        let frame = stack.pop().ok_or(BudgetError::Overflow)?;
        let children = frame.children.ok_or(BudgetError::Overflow)?;
        let value = frame
            .shape
            .header
            .try_zip(children)?
            .map(|(head, children)| FormItem {
                id: head.id,
                name: head.name,
                tag: head.tag,
                children,
            });
        if let Some(parent) = stack.last_mut() {
            parent.children = Some(append(
                parent.children.take().ok_or(BudgetError::Overflow)?,
                value,
            )?);
        } else {
            return Ok(Some(value));
        }
    }
}
fn section(node: &Node) -> Option<&[Node]> {
    let values = node.as_list()?;
    let count = usize::try_from(values.get(1)?.as_i64()?).ok()?;
    let rows = values.get(2..)?;
    rows.get(..count.min(rows.len()))
}
fn record_id(node: &Node) -> Option<i64> {
    node.at(1)?.at(0)?.text()?.parse().ok()
}
fn summary(
    budget: &MemoryBudget,
    root: &Node,
    layout: &'static Layout,
) -> Result<Option<Charged<Form>>, BudgetError> {
    let top = present!(root.as_list());
    let raw = present!(top.get(1).and_then(Node::as_list));
    let extensions = present!(
        raw.get(18)
            .and_then(Node::as_i64)
            .and_then(|n| usize::try_from(n).ok())
    );
    if extensions > raw.len() {
        return Ok(None);
    }
    let start = present!(extensions.checked_mul(2).and_then(|n| n.checked_add(19)));
    let acb = present!(start.checked_add(3).and_then(|n| raw.get(n)));
    let count = present!(
        start
            .checked_add(4)
            .and_then(|n| raw.get(n))
            .and_then(Node::as_i64)
            .and_then(|n| usize::try_from(n).ok())
    );
    if count > raw.len() {
        return Ok(None);
    }
    present!(top.get(2).and_then(Node::text));
    let mut items = vector(budget, count.checked_add(1).ok_or(BudgetError::Overflow)?)?;
    items = append(
        items,
        present!(item(
            budget,
            Spec {
                raw: acb,
                class: None,
                parent: None,
                layout,
            }
        )?),
    )?;
    for offset in 0..count {
        let position = present!(
            offset
                .checked_mul(2)
                .and_then(|n| start.checked_add(5)?.checked_add(n))
        );
        let class = present!(
            raw.get(position)
                .and_then(Node::text)
                .and_then(decode::class_of)
        );
        let node = present!(position.checked_add(1).and_then(|n| raw.get(n)));
        items = append(
            items,
            present!(item(
                budget,
                Spec {
                    raw: node,
                    class: Some(class),
                    parent: None,
                    layout,
                }
            )?),
        )?;
    }
    let tail = present!(
        count
            .checked_mul(2)
            .and_then(|n| start.checked_add(5)?.checked_add(n))
    );
    if !layout.accepts_tail(raw.len().saturating_sub(tail)) || tail > raw.len() {
        return Ok(None);
    }
    present!(decode::strict_sections(layout, top));
    let attrs = present!(top.get(3).and_then(section));
    let mut attributes = vector(budget, attrs.len())?;
    for row in attrs {
        let values = present!(row.as_list());
        let columns = present!(
            values
                .get(13)
                .and_then(Node::as_i64)
                .and_then(|n| usize::try_from(n).ok())
        );
        let children = present!(values.get(14..));
        for column in present!(children.get(..columns.min(children.len()))) {
            let cells = present!(column.as_list());
            present!(
                cells
                    .get(1)
                    .and_then(Node::text)
                    .and_then(|n| n.parse::<i64>().ok())
            );
            present!(cells.get(3).and_then(Node::text));
        }
        let id = present!(record_id(row));
        let name = present!(values.get(3).and_then(Node::text));
        let main = values.get(10).and_then(Node::as_bool).unwrap_or(false);
        attributes = append(
            attributes,
            string(budget, name)?.map(|name| FormAttribute { id, name, main }),
        )?;
    }
    let rows = present!(top.get(4).and_then(section));
    let mut parameters = vector(budget, rows.len())?;
    for row in rows {
        let values = present!(row.as_list());
        let name = present!(values.get(1).and_then(Node::text));
        let key = values.get(3).and_then(Node::as_bool).unwrap_or(false);
        parameters = append(
            parameters,
            string(budget, name)?.map(|name| FormParameter { name, key }),
        )?;
    }
    let rows = present!(top.get(5).and_then(section));
    let mut commands = vector(budget, rows.len())?;
    for row in rows {
        let values = present!(row.as_list());
        let id = present!(record_id(row));
        let name = present!(values.get(2).and_then(Node::text));
        let action = values.get(8).and_then(Node::text).unwrap_or_default();
        commands = append(
            commands,
            string(budget, name)?
                .try_zip(string(budget, action)?)?
                .map(|(name, action)| FormCommand { id, name, action }),
        )?;
    }
    let events = present!(events(budget, raw.get(start), layout.strict)?);
    Ok(Some(
        empty(budget, Form::default())?
            .try_zip(items)?
            .try_zip(attributes)?
            .try_zip(parameters)?
            .try_zip(commands)?
            .try_zip(events)?
            .map(
                |(((((mut form, items), attributes), parameters), commands), events)| {
                    form.items = items;
                    form.attributes = attributes;
                    form.parameters = parameters;
                    form.commands = commands;
                    form.events = events;
                    form
                },
            ),
    ))
}
type Events = Charged<Vec<(String, String)>>;

fn events(
    budget: &MemoryBudget,
    node: Option<&Node>,
    strict: bool,
) -> Result<Option<Events>, BudgetError> {
    let none = |budget: &MemoryBudget| -> Result<Option<Events>, BudgetError> {
        if strict {
            Ok(None)
        } else {
            vector(budget, 0).map(Some)
        }
    };
    let Some(values) = node.and_then(Node::as_list) else {
        return none(budget);
    };
    let Some(text) = values
        .first()
        .and_then(Node::text)
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    else {
        return none(budget);
    };
    let Ok(count) = text.parse::<usize>() else {
        return none(budget);
    };
    if count > values.len() / 2 {
        return none(budget);
    }
    let names = crate::forms::tables::TableView::embedded(false)
        .events
        .get("names");
    let event = |index: usize| {
        let name = names?.get(values.get(index * 2 + 1)?.text()?)?.as_str()?;
        let handler = values.get(index * 2 + 2)?.as_str()?;
        Some((name, handler))
    };
    if (0..count).any(|index| event(index).is_none()) {
        return none(budget);
    }
    let mut output = vector(budget, count)?;
    for index in 0..count {
        let (name, handler) = event(index).ok_or(BudgetError::Overflow)?;
        output = append(
            output,
            string(budget, name)?.try_zip(string(budget, handler)?)?,
        )?;
    }
    Ok(Some(output))
}
