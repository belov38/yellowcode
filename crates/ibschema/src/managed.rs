use crate::{Error, Result};
use brace::Node;
use memory_budget::{BudgetedVec, Category, Charged, MemoryBudget};

pub(crate) struct Build<'a> {
    pub budget: &'a MemoryBudget,
    pub what: &'static str,
}
impl Build<'_> {
    pub fn bad(&self, why: &'static str) -> Error {
        Error::ParseStatic {
            what: self.what,
            why,
        }
    }
    pub fn list<'a>(&self, node: &'a Node) -> Result<&'a [Node]> {
        node.as_list().ok_or_else(|| self.bad("expected a list"))
    }
    pub fn integer(&self, node: &Node) -> Result<i64> {
        node.as_atom()
            .and_then(crate::canonical_i64)
            .ok_or_else(|| self.bad("expected a canonical integer"))
    }
    pub fn uint(&self, node: &Node) -> Result<u32> {
        u32::try_from(self.integer(node)?).map_err(|_| self.bad("integer is outside u32 range"))
    }
    pub fn flag(&self, node: &Node) -> Result<bool> {
        match self.integer(node)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.bad("flag is not 0 or 1")),
        }
    }
    pub fn counted<'a>(&self, node: &'a Node) -> Result<&'a [Node]> {
        let (count, rows) = self
            .list(node)?
            .split_first()
            .ok_or_else(|| self.bad("empty counted list"))?;
        if usize::try_from(self.integer(count)?).ok() != Some(rows.len()) {
            return Err(self.bad("list count differs from item count"));
        }
        Ok(rows)
    }
    pub fn str<'a>(&self, node: &'a Node) -> Result<&'a str> {
        node.as_str()
            .ok_or_else(|| self.bad("expected a quoted string"))
    }
    pub fn text(&self, text: &str) -> Result<Charged<String>> {
        let mut output =
            BudgetedVec::try_with_capacity(self.budget, Category::Metadata, text.len())?;
        output.try_extend_from_slice(text.as_bytes())?;
        output
            .into_charged_vec()
            .try_map(String::from_utf8)
            .map_err(|_| self.bad("invalid string encoding"))
    }
    pub fn string(&self, node: &Node) -> Result<Charged<String>> {
        self.text(self.str(node)?)
    }
    pub fn vector<T>(
        &self,
        rows: &[Node],
        mut decode: impl FnMut(&Node) -> Result<Charged<T>>,
    ) -> Result<Charged<Vec<T>>> {
        let mut output =
            BudgetedVec::<T>::try_with_capacity(self.budget, Category::Metadata, rows.len())?
                .into_charged_vec();
        for node in rows {
            let value = decode(node)?;
            output = output.try_zip(value)?.map(|(mut output, value)| {
                output.push(value);
                output
            });
        }
        Ok(output)
    }
    pub fn integers(&self, rows: &[Node]) -> Result<Charged<Vec<i64>>> {
        let mut output =
            BudgetedVec::try_with_capacity(self.budget, Category::Metadata, rows.len())?;
        for node in rows {
            output.try_push(self.integer(node)?)?;
        }
        Ok(output.into_charged_vec())
    }
}
