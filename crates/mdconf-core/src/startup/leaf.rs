use super::{ManagedValueContext, index::text, values::empty};
use crate::Value;
use brace::Node;
use mdschema::{Codec, StaticPropSpec};
use memory_budget::{BudgetError, BudgetedText, Charged};
macro_rules! present {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}
impl ManagedValueContext<'_> {
    pub fn leaf_value(
        &self,
        spec: StaticPropSpec,
        node: &Node,
        path2: Option<&str>,
    ) -> Result<Option<Charged<Value>>, BudgetError> {
        let value = match spec.codec() {
            Codec::Bool => empty(self.memory_budget(), Value::Bool(node.text() == Some("1")))?,
            Codec::Str | Codec::Int => self.python_text(node)?.into_string()?.map(Value::Text),
            Codec::Enum => {
                let name = present!(
                    spec.field("map")
                        .and_then(|map| map.get(node.text()?))
                        .and_then(|v| v.as_str())
                );
                text(self.memory_budget(), &[name])?
                    .into_string()?
                    .map(Value::Text)
            }
            Codec::LStr => present!(self.local_string(node)?).map(Value::LStr),
            Codec::Ref => present!(self.metadata_ref(node)?).map(Value::Ref),
            Codec::Type => present!(self.type_desc(node)?).map(Value::Type),
            Codec::TValue => present!(self.typed_value(node)?).map(Value::Typed),
            Codec::Event => {
                let raw = self.python_text(node)?;
                let name = present!(raw.as_str().split('_').next());
                text(self.memory_budget(), &[name])?
                    .into_string()?
                    .map(Value::Text)
            }
            Codec::Method => {
                let id = present!(node.as_guid());
                let name = present!(self.index.names.get(&id));
                text(self.memory_budget(), &[name.as_str(), ".", present!(path2)])?
                    .into_string()?
                    .map(Value::Text)
            }
            Codec::CmdGroup => {
                let raw = present!(if node.as_list().is_some() {
                    node.at(1).and_then(Node::text)
                } else {
                    node.text()
                });
                let name = if let Some(name) = ui("command_groups", raw).filter(|s| !s.is_empty()) {
                    name
                } else {
                    let id = present!(brace::Guid::parse(raw));
                    present!(self.index.names.get(&id)).as_str()
                };
                text(self.memory_budget(), &[name])?
                    .into_string()?
                    .map(Value::Text)
            }
            Codec::Shortcut => present!(self.shortcut(node)?)
                .into_string()?
                .map(Value::Text),
            Codec::Color => present!(self.color(node)?).into_string()?.map(Value::Text),
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
    fn shortcut(&self, node: &Node) -> Result<Option<BudgetedText>, BudgetError> {
        let key = present!(node.at(1).and_then(int));
        let mods = present!(node.at(2).and_then(int));
        if key == 0 {
            return Ok(Some(text(self.memory_budget(), &[""])?));
        }
        let mut buffer = [0; 24];
        let mut out = BudgetedText::new(
            self.memory_budget(),
            memory_budget::Category::Metadata,
            u64::MAX,
        )?;
        for (bit, name) in [(8, "Ctrl+"), (16, "Alt+"), (4, "Shift+")] {
            if mods & bit != 0 {
                out.try_push_str(name)?;
            }
        }
        if (112..=135).contains(&key) {
            out.try_push_str("F")?;
            out.try_push_str(decimal(key - 111, &mut buffer)?)?;
        } else if (48..=57).contains(&key) || (65..=90).contains(&key) {
            let key = u8::try_from(key).map_err(|_| BudgetError::Overflow)?;
            out.try_push(char::from(key))?;
        } else {
            let key = decimal(key, &mut buffer)?;
            out.try_push_str(present!(ui("keys", key)))?;
        }
        Ok(Some(out))
    }
    pub(super) fn color(&self, node: &Node) -> Result<Option<BudgetedText>, BudgetError> {
        let kind = present!(node.at(1).and_then(Node::text));
        let value = match present!(node.at(2)) {
            Node::List(nodes) => nodes.first().and_then(Node::text).unwrap_or(""),
            _ => "",
        };
        match kind {
            "4" => {
                if node.at(3).and_then(Node::text) == Some("5") {
                    let (_, name) = present!(
                        crate::objects::ui::PALETTE
                            .iter()
                            .find(|(id, _)| *id == value)
                    );
                    Ok(Some(text(self.memory_budget(), &["pal:", name])?))
                } else {
                    Ok(Some(text(self.memory_budget(), &["auto"])?))
                }
            }
            "0" => {
                let value = present!(value.trim().parse::<i64>().ok());
                const HEX: &[u8] = b"0123456789ABCDEF";
                let mut out = [b'#'; 7];
                for i in 0..3 {
                    let byte = (value >> (8 * i)) & 255;
                    *out.get_mut(1 + i * 2).ok_or(BudgetError::Overflow)? = *HEX
                        .get(usize::try_from(byte >> 4).map_err(|_| BudgetError::Overflow)?)
                        .ok_or(BudgetError::Overflow)?;
                    *out.get_mut(2 + i * 2).ok_or(BudgetError::Overflow)? = *HEX
                        .get(usize::try_from(byte & 15).map_err(|_| BudgetError::Overflow)?)
                        .ok_or(BudgetError::Overflow)?;
                }
                let out = std::str::from_utf8(&out).map_err(|_| BudgetError::Overflow)?;
                Ok(Some(text(self.memory_budget(), &[out])?))
            }
            "1" | "2" | "3" => {
                let (prefix, table) = match kind {
                    "1" => ("win:", "win_colors"),
                    "2" => ("web:", "web_colors"),
                    _ => ("style:", "style_colors"),
                };
                let name = present!(ui(table, value));
                Ok(Some(text(self.memory_budget(), &[prefix, name])?))
            }
            _ => Ok(None),
        }
    }
}
fn ui(table: &str, key: &str) -> Option<&'static str> {
    mdschema::static_schema()
        .table("ui.json")?
        .get(table)?
        .get(key)?
        .as_str()
}
fn int(node: &Node) -> Option<i64> {
    node.text()?.trim().parse().ok()
}
pub(super) fn decimal(value: i64, buffer: &mut [u8; 24]) -> Result<&str, BudgetError> {
    let negative = value < 0;
    let mut value = value.unsigned_abs();
    let mut at = buffer.len();
    loop {
        at = at.checked_sub(1).ok_or(BudgetError::Overflow)?;
        *buffer.get_mut(at).ok_or(BudgetError::Overflow)? =
            b'0' + u8::try_from(value % 10).map_err(|_| BudgetError::Overflow)?;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    if negative {
        at = at.checked_sub(1).ok_or(BudgetError::Overflow)?;
        *buffer.get_mut(at).ok_or(BudgetError::Overflow)? = b'-';
    }
    std::str::from_utf8(buffer.get(at..).ok_or(BudgetError::Overflow)?)
        .map_err(|_| BudgetError::Overflow)
}
