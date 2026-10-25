use crate::sym::Sym;
use bsl_prim::fold_name;
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct Scope {
    slots: Vec<Sym>,
    names: HashMap<Sym, u32>,
}

impl Scope {
    pub(crate) fn get(&self, name: &Sym) -> Option<u32> {
        self.names.get(name).copied()
    }

    pub(crate) fn declare(&mut self, name: Sym) -> u32 {
        if let Some(&i) = self.names.get(&name) {
            return i;
        }
        let i = self.push(name.clone());
        self.names.insert(name, i);
        i
    }

    pub(crate) fn hidden(&mut self, empty: Sym) -> u32 {
        self.push(empty)
    }

    fn push(&mut self, name: Sym) -> u32 {
        let i = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
        self.slots.push(name);
        i
    }

    pub(crate) fn into_slots(self) -> Vec<Sym> {
        self.slots
    }
}

#[derive(Default)]
pub(crate) struct Labels(HashMap<String, u32>);

impl Labels {
    pub(crate) fn id(&mut self, name: &str) -> u32 {
        let next = u32::try_from(self.0.len()).unwrap_or(u32::MAX);
        *self.0.entry(fold_name(name)).or_insert(next)
    }
}
