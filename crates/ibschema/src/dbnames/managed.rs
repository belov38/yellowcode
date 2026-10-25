use super::{DbName, DbNames, Lookup, WHAT, entry_key, entry_number};
use crate::{Result, managed::Build};
use memory_budget::{BudgetedVec, Category, Charged, MemoryBudget};

impl DbNames {
    pub fn parse_budgeted(text: &[u8], budget: &MemoryBudget) -> Result<Charged<Self>> {
        let root = brace::parse_bytes_budgeted(text, budget, Category::Metadata)?;
        let build = Build { budget, what: WHAT };
        let [counter, body] = build.list(&root)? else {
            return Err(build.bad("expected {counter,{n,...}}"));
        };
        let counter = build.uint(counter)?;
        let rows = build.counted(body)?;
        let entries = build.vector(rows, |node| {
            let [uuid, purpose, number] = build.list(node)? else {
                return Err(build.bad("expected {uuid,purpose,number}"));
            };
            let uuid = uuid.as_guid().ok_or_else(|| build.bad("invalid uuid"))?;
            let number = build.uint(number)?;
            Ok(build.string(purpose)?.map(|purpose| DbName {
                uuid,
                purpose,
                number,
            }))
        })?;
        let mut keys = BudgetedVec::try_with_capacity(budget, Category::Metadata, entries.len())?;
        let mut numbers =
            BudgetedVec::try_with_capacity(budget, Category::Metadata, entries.len())?;
        for i in 0..entries.len() {
            keys.try_push(i)?;
            numbers.try_push(i)?;
        }
        keys.as_mut_slice()
            .sort_unstable_by(|&a, &b| entry_key(&entries, a).cmp(&entry_key(&entries, b)));
        numbers
            .as_mut_slice()
            .sort_unstable_by_key(|&i| entry_number(&entries, i));
        if keys.windows(2).any(|pair| {
            pair.first()
                .zip(pair.get(1))
                .is_some_and(|(&a, &b)| entry_key(&entries, a) == entry_key(&entries, b))
        }) {
            return Err(build.bad("duplicate DBNames key"));
        }
        if numbers.windows(2).any(|pair| {
            pair.first()
                .zip(pair.get(1))
                .is_some_and(|(&a, &b)| entry_number(&entries, a) == entry_number(&entries, b))
        }) {
            return Err(build.bad("duplicate DBNames number"));
        }
        let top = entries.iter().map(|entry| entry.number).max().unwrap_or(0);
        Ok(entries
            .try_zip(keys.into_charged_vec())?
            .try_zip(numbers.into_charged_vec())?
            .map(|((entries, by_key), by_num)| Self {
                counter,
                entries,
                lookup: Lookup::Managed { by_key, by_num },
                top,
            }))
    }
}
