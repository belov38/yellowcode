use crate::{Error, Result};
use brace::{Guid, Node};
use memory_budget::{Category, LimitReason, MemoryLimit};

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeUse {
    Description,
    AllowedTypes,
    FormNames,
}
fn overflow() -> Error {
    Error::Memory(MemoryLimit {
        category: Category::Metadata,
        requested: u64::MAX,
        reason: LimitReason::Overflow,
    })
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(overflow)
}
fn qualified_buffer(local: u64) -> Result<u64> {
    add(local, 4)?.checked_mul(2).ok_or_else(overflow)
}

#[doc(hidden)]
pub fn type_use_bytes<'a>(
    pattern: &Node,
    resolve: &dyn Fn(Guid) -> Option<&'a str>,
    consumer: TypeUse,
    check: &dyn Fn() -> Result<()>,
) -> Result<u64> {
    check()?;
    let Some(items) = pattern
        .as_list()
        .filter(|v| v.first().and_then(Node::text) == Some("Pattern"))
    else {
        return Ok(0);
    };
    let mut total = 0;
    for descriptor in items.get(1..).unwrap_or_default() {
        check()?;
        if descriptor.at(0).and_then(Node::text) != Some("#") {
            continue;
        }
        let Some(name) = descriptor
            .at(1)
            .and_then(Node::text)
            .and_then(Guid::parse)
            .and_then(resolve)
        else {
            continue;
        };
        let local = name.len() as u64;
        let amount = match consumer {
            TypeUse::Description => local,
            TypeUse::FormNames => qualified_buffer(local)?,
            TypeUse::AllowedTypes => {
                let description = local;
                let expansion_input_clone = local;
                let qualified_output = if super::super::objects::types::SET_CATEGORIES
                    .contains(&name.split('.').next().unwrap_or_default())
                {
                    0
                } else {
                    qualified_buffer(local)?
                };
                add(add(description, expansion_input_clone)?, qualified_output)?
            }
        };
        total = add(total, amount)?;
    }
    Ok(total)
}
