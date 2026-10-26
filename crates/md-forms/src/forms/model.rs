use brace::Node;
use md_model::forms::FormAttributeType;

use super::decode::{DecodedForm, Item, Rec, item_tag};
use super::{Form, FormAttribute, FormCommand, FormItem, FormParameter};

fn form_item(it: &Item, parent: Option<(&str, usize)>) -> FormItem {
    let tag = item_tag(it, parent.map(|p| p.0), parent.map(|p| p.1));
    let cls = it.class.unwrap_or("nested");
    let mut nested: Vec<&(usize, Item)> = it.nested.iter().collect();
    nested.sort_by_key(|(p, _)| *p);
    let mut children: Vec<FormItem> = nested
        .into_iter()
        .map(|(p, n)| form_item(n, Some((cls, *p))))
        .collect();
    children.extend(it.children.iter().map(|c| form_item(c, None)));
    FormItem {
        id: it.id_text.parse().unwrap_or(0),
        name: it.name().unwrap_or_default().to_string(),
        tag: tag.to_string(),
        children,
    }
}

fn flag(r: &Rec, i: usize) -> bool {
    r.raw.at(i).and_then(Node::as_bool).unwrap_or(false)
}

pub(crate) fn attribute_types(
    pattern: Option<&Node>,
) -> impl Iterator<Item = FormAttributeType> + '_ {
    pattern
        .filter(|p| p.at(0).and_then(Node::text) == Some("Pattern"))
        .and_then(Node::as_list)
        .and_then(|p| p.get(1..))
        .into_iter()
        .flatten()
        .map(|descriptor| match descriptor.at(0).and_then(Node::text) {
            Some("#") => descriptor
                .at(1)
                .and_then(Node::text)
                .and_then(brace::Guid::parse)
                .map_or(FormAttributeType::Invalid, FormAttributeType::Id),
            Some(_) => FormAttributeType::Other,
            None => FormAttributeType::Invalid,
        })
}

pub(crate) fn form_from_decoded(d: &DecodedForm) -> Form {
    let mut items = Vec::new();
    if let Some(a) = d.acb() {
        items.push(form_item(a, None));
    }
    items.extend(d.form.children.iter().map(|c| form_item(c, None)));
    Form {
        items,
        attributes: d
            .attrs
            .iter()
            .map(|r| FormAttribute {
                id: r.id.unwrap_or(0),
                name: r.name.clone(),
                main: flag(r, 10),
                types: attribute_types(r.raw.at(5)).collect(),
            })
            .collect(),
        commands: d
            .commands
            .iter()
            .map(|r| FormCommand {
                id: r.id.unwrap_or(0),
                name: r.name.clone(),
                action: r
                    .raw
                    .at(8)
                    .and_then(Node::text)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect(),
        parameters: d
            .params
            .iter()
            .map(|r| FormParameter {
                name: r.name.clone(),
                key: flag(r, 3),
            })
            .collect(),
        events: Vec::new(),
        source: None,
        shared: None,
    }
}
