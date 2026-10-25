use crate::index::Index;
use crate::model::Value;
use brace::{Guid, Node};
use mdschema::Ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandGroupEncodeError {
    WrongValue,
    UnknownGroup,
    AmbiguousGroup,
}

fn select(found: &mut Option<Guid>, id: Guid) -> Result<(), CommandGroupEncodeError> {
    if found.is_some_and(|previous| previous != id) {
        return Err(CommandGroupEncodeError::AmbiguousGroup);
    }
    *found = Some(id);
    Ok(())
}

pub(crate) fn encode_cmdgroup(
    ui: &Ui,
    index: &Index,
    value: &Value,
) -> Result<Node, CommandGroupEncodeError> {
    let Value::Text(text) = value else {
        return Err(CommandGroupEncodeError::WrongValue);
    };
    if text.is_empty() {
        return Err(CommandGroupEncodeError::UnknownGroup);
    }
    let mut found = None;
    for (key, name) in &ui.command_groups {
        if name != text {
            continue;
        }
        let Some(id) = Guid::parse(key) else {
            continue;
        };
        if id.to_string() == *key {
            select(&mut found, id)?;
        }
    }
    for (id, name) in &index.names {
        if name != text {
            continue;
        }
        if ui
            .command_groups
            .get(&id.to_string())
            .filter(|winner| !winner.is_empty())
            .is_some_and(|winner| winner != text)
        {
            continue;
        }
        select(&mut found, *id)?;
    }
    found
        .map(|id| Node::list(vec![Node::int(1), Node::guid(id)]))
        .ok_or(CommandGroupEncodeError::UnknownGroup)
}
