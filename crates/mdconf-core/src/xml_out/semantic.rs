use super::{XmlWriter, emit::XmlEmit};
use memory_budget::BudgetError;

#[doc(hidden)]
pub trait SemanticEmit {
    fn depth(&self) -> usize;
    fn open_attributes(&mut self, tag: &str, attrs: &[(&str, &str)]) -> Result<(), BudgetError>;
    fn attribute_fragments<'a>(
        &mut self,
        tag: &str,
        attrs: &mut dyn Iterator<Item = (&'a str, [&'a str; 3])>,
    ) -> Result<(), BudgetError>;
    fn text_fragments(
        &mut self,
        tag: &str,
        text: &[&str],
        attrs: &[(&str, &str)],
    ) -> Result<(), BudgetError>;
    fn open_parts(&mut self, tag: &[&str]) -> Result<(), BudgetError>;
    fn close_parts(&mut self, tag: &[&str]) -> Result<(), BudgetError>;
    fn leaf_parts(&mut self, tag: &[&str], text: &str, normalize: bool) -> Result<(), BudgetError>;
    fn value(
        &mut self,
        tag: &str,
        text: Option<&str>,
        normalize: bool,
        attrs: &[(&str, &str)],
    ) -> Result<(), BudgetError>;
}
impl SemanticEmit for XmlWriter {
    fn depth(&self) -> usize {
        XmlWriter::depth(self)
    }
    fn open_attributes(&mut self, tag: &str, attrs: &[(&str, &str)]) -> Result<(), BudgetError> {
        self.open(tag, attrs);
        Ok(())
    }
    fn attribute_fragments<'a>(
        &mut self,
        tag: &str,
        attrs: &mut dyn Iterator<Item = (&'a str, [&'a str; 3])>,
    ) -> Result<(), BudgetError> {
        let attrs: Vec<_> = attrs.map(|(key, value)| (key, value.concat())).collect();
        let borrowed: Vec<_> = attrs
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        self.leaf(tag, None, &borrowed);
        Ok(())
    }
    fn text_fragments(
        &mut self,
        tag: &str,
        text: &[&str],
        attrs: &[(&str, &str)],
    ) -> Result<(), BudgetError> {
        self.leaf(tag, Some(&text.concat()), attrs);
        Ok(())
    }
    fn open_parts(&mut self, tag: &[&str]) -> Result<(), BudgetError> {
        self.open(&tag.concat(), &[]);
        Ok(())
    }
    fn close_parts(&mut self, tag: &[&str]) -> Result<(), BudgetError> {
        self.close(&tag.concat());
        Ok(())
    }
    fn leaf_parts(&mut self, tag: &[&str], text: &str, normalize: bool) -> Result<(), BudgetError> {
        let tag = tag.concat();
        if normalize {
            self.leaf(&tag, Some(&text.replace("\r\n", "\n")), &[]);
        } else {
            self.leaf(&tag, Some(text), &[]);
        }
        Ok(())
    }
    fn value(
        &mut self,
        tag: &str,
        text: Option<&str>,
        normalize: bool,
        attrs: &[(&str, &str)],
    ) -> Result<(), BudgetError> {
        match text {
            Some(text) if normalize => self.leaf(tag, Some(&text.replace("\r\n", "\n")), attrs),
            _ => self.leaf(tag, text, attrs),
        }
        Ok(())
    }
}
impl SemanticEmit for XmlEmit<'_> {
    fn depth(&self) -> usize {
        XmlEmit::depth(self)
    }
    fn open_attributes(&mut self, tag: &str, attrs: &[(&str, &str)]) -> Result<(), BudgetError> {
        self.open(tag, attrs.iter().copied())
    }
    fn attribute_fragments<'a>(
        &mut self,
        tag: &str,
        attrs: &mut dyn Iterator<Item = (&'a str, [&'a str; 3])>,
    ) -> Result<(), BudgetError> {
        self.leaf_attribute_fragments(tag, attrs)
    }
    fn text_fragments(
        &mut self,
        tag: &str,
        text: &[&str],
        attrs: &[(&str, &str)],
    ) -> Result<(), BudgetError> {
        self.leaf_text_fragments(tag, text, attrs.iter().copied())
    }
    fn open_parts(&mut self, tag: &[&str]) -> Result<(), BudgetError> {
        self.open_fragments(tag)
    }
    fn close_parts(&mut self, tag: &[&str]) -> Result<(), BudgetError> {
        self.close_fragments(tag)
    }
    fn leaf_parts(&mut self, tag: &[&str], text: &str, normalize: bool) -> Result<(), BudgetError> {
        self.leaf_fragments(tag, text, normalize)
    }
    fn value(
        &mut self,
        tag: &str,
        text: Option<&str>,
        normalize: bool,
        attrs: &[(&str, &str)],
    ) -> Result<(), BudgetError> {
        self.leaf_normalized(tag, text, normalize, attrs.iter().copied())
    }
}
