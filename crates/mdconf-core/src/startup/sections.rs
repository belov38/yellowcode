use super::{ManagedValueContext, collections::count, index::text, xml::Xml};
use crate::Value;
use brace::Node;
use memory_budget::{BudgetError, Charged};
macro_rules! present {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}
impl ManagedValueContext<'_> {
    pub fn standard_tabular_sections(
        &self,
        owner: (&str, &str),
        node: &Node,
    ) -> Result<Option<Charged<Value>>, BudgetError> {
        let [flag, body] = present!(node.as_list()) else {
            return Ok(None);
        };
        if flag.text() != Some("1") {
            return Ok(None);
        }
        let body = present!(body.as_list());
        let n = present!(body.get(1).and_then(count));
        if n > body.len().saturating_sub(2) / 2 {
            return Ok(None);
        }
        let mut root = Xml::new(self.memory_budget(), "StandardTabularSections", 0, n)?.expanded();
        for i in 0..n {
            let id = present!(body.get(2 + 2 * i).and_then(Node::text));
            let name = present!(crate::objects::overlay::std_section(owner.0, id));
            let section = present!(body.get(3 + 2 * i).and_then(Node::as_list));
            if section.get(4).and_then(Node::text) != Some("0") {
                return Ok(None);
            }
            let fill = match present!(section.get(3).and_then(Node::text)) {
                "0" => "DontCheck",
                "1" => "ShowError",
                _ => return Ok(None),
            };
            let kind = text(self.memory_budget(), &[owner.0, ".", name])?;
            let attrs = present!(self.std_attr_list(
                present!(section.get(5).and_then(Node::as_list)),
                kind.as_str(),
                Some(owner)
            )?)
            .map(Value::StandardAttributes);
            let synonym = present!(self.local_string(present!(section.get(1)))?).map(Value::LStr);
            let tip = present!(self.local_string(present!(section.get(6)))?).map(Value::LStr);
            let comment = super::values::description_text(
                self.memory_budget(),
                present!(section.get(2).and_then(Node::text)),
            )?;
            let child = Xml::new(self.memory_budget(), "xr:StandardTabularSection", 1, 5)?
                .attr(self.memory_budget(), "name", name)?
                .child(Xml::value(
                    self.memory_budget(),
                    "xr:Synonym",
                    &synonym,
                    true,
                )?)?
                .child(Xml::leaf(
                    self.memory_budget(),
                    "xr:Comment",
                    comment.as_str(),
                    0,
                )?)?
                .child(Xml::value(self.memory_budget(), "xr:ToolTip", &tip, true)?)?
                .child(Xml::leaf(self.memory_budget(), "xr:FillChecking", fill, 0)?)?
                .child(Xml::value(
                    self.memory_budget(),
                    "xr:StandardAttributes",
                    &attrs,
                    true,
                )?)?;
            root = root.child(child)?;
        }
        Ok(Some(root.into_charged().map(Value::Xml)))
    }
}
