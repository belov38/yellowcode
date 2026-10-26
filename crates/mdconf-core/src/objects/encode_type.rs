use crate::model::{DateFractions, Value};
use brace::Node;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeError {
    WrongValue,
    UnsupportedType,
    InvalidShape,
}

pub(crate) fn encode_builtin_type(value: &Value) -> Result<Node, TypeError> {
    let Value::Type(description) = value else {
        return Err(TypeError::WrongValue);
    };
    let mut seen = [false; 5];
    let mut items = vec![Node::str("Pattern")];
    for reference in &description.types {
        if reference.set || reference.type_id.is_some() {
            return Err(TypeError::UnsupportedType);
        }
        let position = match reference
            .name
            .fixed_local(crate::FixedNamespace::Xs)
            .unwrap_or("")
        {
            "boolean" => 0,
            "string" => 1,
            "decimal" => 2,
            "dateTime" => 3,
            "base64Binary" => 4,
            _ => return Err(TypeError::UnsupportedType),
        };
        let slot = seen.get_mut(position).ok_or(TypeError::InvalidShape)?;
        if std::mem::replace(slot, true) {
            return Err(TypeError::InvalidShape);
        }
        let item = match position {
            0 => vec![Node::str("B")],
            1 => {
                let q = description.string.ok_or(TypeError::InvalidShape)?;
                if q.length == 0 && !q.fixed {
                    vec![Node::str("S")]
                } else {
                    vec![Node::str("S"), number(q.length), flag(!q.fixed)]
                }
            }
            2 => {
                let q = description.number.ok_or(TypeError::InvalidShape)?;
                vec![
                    Node::str("N"),
                    number(q.digits),
                    number(q.fraction),
                    flag(q.nonnegative),
                ]
            }
            3 => match description.date.ok_or(TypeError::InvalidShape)? {
                DateFractions::Date => vec![Node::str("D"), Node::str("D")],
                DateFractions::Time => vec![Node::str("D"), Node::str("T")],
                DateFractions::DateTime => vec![Node::str("D")],
            },
            4 => {
                let q = description.binary.ok_or(TypeError::InvalidShape)?;
                vec![Node::str("R"), number(q.length), flag(!q.fixed)]
            }
            _ => return Err(TypeError::InvalidShape),
        };
        items.push(Node::list(item));
    }
    if description.string.is_some() != seen[1]
        || description.number.is_some() != seen[2]
        || description.date.is_some() != seen[3]
        || description.binary.is_some() != seen[4]
    {
        return Err(TypeError::InvalidShape);
    }
    Ok(Node::list(items))
}

fn number(value: u32) -> Node {
    Node::atom(value.to_string())
}

fn flag(value: bool) -> Node {
    Node::int(i64::from(value))
}
