use bsl_prim::fold_name;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Sym {
    pub folded: Arc<str>,
    pub name: Arc<str>,
}

impl PartialEq for Sym {
    fn eq(&self, other: &Sym) -> bool {
        self.folded == other.folded
    }
}

impl Eq for Sym {}

impl Hash for Sym {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.folded.hash(state);
    }
}

#[derive(Default)]
pub struct Interner {
    map: HashMap<Arc<str>, Sym>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sym(&mut self, name: &str) -> Sym {
        let folded = fold_name(name);
        if let Some(s) = self.map.get(folded.as_str()) {
            return s.clone();
        }
        let folded: Arc<str> = folded.into();
        let sym = Sym {
            folded: folded.clone(),
            name: name.into(),
        };
        self.map.insert(folded, sym.clone());
        sym
    }
}
