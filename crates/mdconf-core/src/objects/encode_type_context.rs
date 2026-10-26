use super::encode_type::encode_builtin_type;
use crate::index::Index;
use crate::model::{TypeRef, Value};
use brace::{Guid, Node};
use mdschema::Schema;
use std::collections::{HashMap, HashSet};

const TYPE_DESCRIPTION: &str = "f5c65050-3bbb-11d5-b988-0050bae0a95d";
type Key = (crate::XmlTypeName, bool, Option<Guid>);
type Result<T> = std::result::Result<T, TypeContextError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeContextError {
    WrongValue,
    InvalidShape,
    Unresolved,
    InvalidContext,
}

pub(crate) struct TypeContext {
    by_meaning: HashMap<Key, Guid>,
}

impl TypeContext {
    pub(crate) fn new(index: &Index, schema: &Schema) -> Result<Self> {
        Self::from_authorities(
            index.types.iter().map(|(id, name)| (*id, name.as_str())),
            schema
                .platform_types()
                .map(|(id, ty)| (id, ty.name(), ty.set)),
        )
    }

    fn from_authorities<'a>(
        generated: impl IntoIterator<Item = (Guid, &'a str)>,
        platform: impl IntoIterator<Item = (&'a str, mdschema::ExpandedTypeName<'a>, bool)>,
    ) -> Result<Self> {
        let mut effective = HashMap::new();
        for (text, name, set) in platform {
            let id = Guid::parse(text).ok_or(TypeContextError::InvalidContext)?;
            if id.to_string() != text {
                return Err(TypeContextError::InvalidContext);
            }
            let meaning = (
                crate::XmlTypeName::new(name.namespace_uri, name.local)
                    .ok_or(TypeContextError::InvalidContext)?,
                set,
                None,
            );
            if let Some(previous) = effective.insert(id, meaning.clone())
                && previous != meaning
            {
                return Err(TypeContextError::InvalidContext);
            }
        }
        let special = Guid::parse(TYPE_DESCRIPTION).ok_or(TypeContextError::InvalidContext)?;
        effective.insert(
            special,
            (
                crate::XmlTypeName::from_qname("v8:TypeDescription")
                    .ok_or(TypeContextError::InvalidContext)?,
                false,
                None,
            ),
        );
        for (id, name) in generated {
            let set = matches!(
                name.split('.').next(),
                Some("DefinedType" | "Characteristic")
            );
            effective.remove(&id);
            if let Some(name) = crate::XmlTypeName::new(mdschema::FixedNamespace::Cfg.uri(), name) {
                effective.insert(id, (name, set, Some(id)));
            }
        }
        let mut by_meaning = HashMap::new();
        for (id, meaning) in effective {
            if let Some(previous) = by_meaning.insert(meaning, id)
                && previous != id
            {
                return Err(TypeContextError::InvalidContext);
            }
        }
        Ok(Self { by_meaning })
    }

    pub(crate) fn encode(&self, value: &Value) -> Result<Node> {
        let Value::Type(description) = value else {
            return Err(TypeContextError::WrongValue);
        };
        let mut seen = HashSet::new();
        for reference in &description.types {
            if !seen.insert(&reference.name) {
                return Err(TypeContextError::InvalidShape);
            }
        }
        let mut builtins = description.clone();
        builtins.types.retain(builtin);
        let encoded = encode_builtin_type(&Value::Type(builtins))
            .map_err(|_| TypeContextError::InvalidShape)?;
        let mut builtin_items = encoded
            .as_list()
            .ok_or(TypeContextError::InvalidShape)?
            .iter()
            .skip(1);
        let mut out = vec![Node::str("Pattern")];
        for reference in &description.types {
            if builtin(reference) {
                out.push(
                    builtin_items
                        .next()
                        .ok_or(TypeContextError::InvalidShape)?
                        .clone(),
                );
            } else {
                let key = (reference.name.clone(), reference.set, reference.type_id);
                let id = *self
                    .by_meaning
                    .get(&key)
                    .ok_or(TypeContextError::Unresolved)?;
                out.push(Node::list(vec![Node::str("#"), Node::guid(id)]));
            }
        }
        Ok(Node::list(out))
    }
}

fn builtin(reference: &TypeRef) -> bool {
    reference
        .name
        .fixed_local(mdschema::FixedNamespace::Xs)
        .is_some_and(|local| {
            matches!(
                local,
                "boolean" | "string" | "decimal" | "dateTime" | "base64Binary"
            )
        })
}
