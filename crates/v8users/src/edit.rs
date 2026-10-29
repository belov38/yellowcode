use crate::{
    BslDate, Error, Uuid16,
    layout::{Layout, MAX_COLLECTION, MAX_DATA_BYTES, MAX_DEPTH},
    user::{UserData, date_node},
};
use brace::Node;
use std::collections::BTreeSet;

#[derive(Default)]
pub struct PublicPatch {
    pub name: Option<String>,
    pub full_name: Option<String>,
    pub roles: Option<Vec<Uuid16>>,
    pub show_in_list: Option<bool>,
    pub rolesid: Option<u32>,
    pub changed_local: Option<BslDate>,
}

impl UserData {
    pub fn edit_public(&self, patch: &PublicPatch) -> Result<Self, Error> {
        let mut remaining = MAX_DATA_BYTES;
        bound_tree(&self.tree, 0, &mut remaining)?;
        let fields = self.tree.as_list().ok_or(Error::NotList)?;
        let layout = Layout::read(fields)?;
        for (logical, present) in [
            (1, patch.name.is_some()),
            (3, patch.full_name.is_some()),
            (5, patch.roles.is_some()),
            (8, patch.show_in_list.is_some()),
            (15, patch.rolesid.is_some()),
            (16, patch.changed_local.is_some()),
        ] {
            if present && layout.at(fields, logical).is_none() {
                return Err(Error::BadField(logical));
            }
        }
        if let Some(name) = &patch.name
            && (name.trim_end_matches(' ').is_empty() || name.encode_utf16().count() > 64)
        {
            return Err(Error::BadField(1));
        }
        if let Some(name) = &patch.full_name
            && name.encode_utf16().count() > 128
        {
            return Err(Error::BadField(3));
        }
        if let Some(roles) = &patch.roles {
            if roles.len() > MAX_COLLECTION || roles.contains(&Uuid16::NIL) {
                return Err(Error::BadField(5));
            }
            if roles.iter().collect::<BTreeSet<_>>().len() != roles.len() {
                return Err(Error::BadField(5));
            }
        }
        if patch
            .changed_local
            .is_some_and(|date| date.frac_ticks() != 0)
        {
            return Err(Error::FractionalChanged);
        }

        let mut edited = fields.to_vec();
        if let Some(name) = &patch.name {
            replace(&mut edited, &layout, 1, Node::str(name))?;
        }
        if let Some(name) = &patch.full_name {
            replace(&mut edited, &layout, 3, Node::str(name))?;
        }
        if let Some(roles) = &patch.roles {
            let mut items = Vec::with_capacity(roles.len() + 1);
            items.push(Node::int(roles.len() as i64));
            items.extend(roles.iter().copied().map(Node::guid));
            replace(&mut edited, &layout, 5, Node::list(items))?;
        }
        if let Some(show) = patch.show_in_list {
            replace(&mut edited, &layout, 8, Node::int(i64::from(show)))?;
        }
        if let Some(number) = patch.rolesid {
            replace(&mut edited, &layout, 15, Node::int(i64::from(number)))?;
        }
        if let Some(changed) = patch.changed_local {
            replace(&mut edited, &layout, 16, date_node(changed))?;
        }
        let edited = Node::list(edited);
        remaining = MAX_DATA_BYTES;
        bound_tree(&edited, 0, &mut remaining)?;
        let result = Self::parse(&brace::to_text(&edited))?;
        let result_fields = result.tree.as_list().ok_or(Error::NotList)?;
        let result_layout = Layout::read(result_fields)?;
        if layout.at(fields, 21) != result_layout.at(result_fields, 21) {
            return Err(Error::BadField(21));
        }
        Ok(result)
    }
}

fn bound_tree(node: &Node, depth: usize, remaining: &mut usize) -> Result<(), Error> {
    let cost = match node {
        Node::List(items) => {
            if depth >= MAX_DEPTH {
                return Err(Error::Limit);
            }
            let framing = 2
                + usize::from(depth > 0) * 2
                + usize::from(items.last().is_some_and(Node::is_list)) * 2;
            framing
                .checked_add(items.len().saturating_sub(1))
                .ok_or(Error::Limit)?
        }
        Node::Str(value) => {
            if value.len() > *remaining {
                return Err(Error::Limit);
            }
            value
                .len()
                .checked_add(2)
                .and_then(|size| size.checked_add(value.bytes().filter(|b| *b == b'"').count()))
                .ok_or(Error::Limit)?
        }
        Node::Atom(value) => value.len(),
    };
    *remaining = remaining.checked_sub(cost).ok_or(Error::Limit)?;
    if let Node::List(items) = node {
        for item in items {
            bound_tree(item, depth + 1, remaining)?;
        }
    }
    Ok(())
}

fn replace(fields: &mut [Node], layout: &Layout, logical: usize, value: Node) -> Result<(), Error> {
    let original = layout.at(fields, logical).ok_or(Error::BadField(logical))?;
    let index = fields
        .iter()
        .position(|field| std::ptr::eq(field, original))
        .ok_or(Error::BadField(logical))?;
    *fields.get_mut(index).ok_or(Error::BadField(logical))? = value;
    Ok(())
}
