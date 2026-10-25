use crate::model::{Configuration, GeneratedTypeRef};
use brace::Guid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadProfile {
    #[default]
    Full,
    Query,
}

impl LoadProfile {
    pub const fn stages(self) -> Stages {
        match self {
            LoadProfile::Full => Stages::ALL,
            LoadProfile::Query => Stages::NONE,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Stages(u16);

impl Stages {
    pub const NONE: Stages = Stages(0);
    pub const MODULES: Stages = Stages(1);
    pub const FORMS: Stages = Stages(1 << 1);
    pub const TEMPLATES: Stages = Stages(1 << 2);
    pub const FLOWCHARTS: Stages = Stages(1 << 3);
    pub const RIGHTS: Stages = Stages(1 << 4);
    pub const COMMAND_INTERFACE: Stages = Stages(1 << 5);
    pub const CONFIG_DUMP: Stages = Stages(1 << 6);
    pub const PICTURES: Stages = Stages(1 << 7);
    pub const HELP: Stages = Stages(1 << 8);
    pub const ALL: Stages = Stages((1 << 9) - 1);

    pub const EACH: [(Stages, &'static str); 9] = [
        (Stages::MODULES, "modules"),
        (Stages::FORMS, "forms"),
        (Stages::RIGHTS, "rights"),
        (Stages::COMMAND_INTERFACE, "command interface"),
        (Stages::CONFIG_DUMP, "configuration dump info"),
        (Stages::PICTURES, "pictures"),
        (Stages::HELP, "help"),
        (Stages::TEMPLATES, "templates"),
        (Stages::FLOWCHARTS, "flowcharts"),
    ];

    pub const fn contains(self, other: Stages) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Stages) -> Stages {
        Stages(self.0 | other.0)
    }

    pub const fn minus(self, other: Stages) -> Stages {
        Stages(self.0 & !other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Debug for Stages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = Stages::EACH
            .iter()
            .filter(|(s, _)| self.contains(*s))
            .map(|(_, n)| *n)
            .collect();
        write!(f, "Stages[{}]", names.join(", "))
    }
}

impl Configuration {
    pub fn full_name(&self, id: Guid) -> Option<&str> {
        self.index.names.get(&id).map(String::as_str)
    }

    pub fn generated_type(&self, type_id: Guid) -> Option<GeneratedTypeRef<'_>> {
        let owner = self.index.type_owners.get(&type_id)?;
        let name = self.index.types.get(&type_id)?;
        let object = self.object(owner.object)?;
        let child = owner.child.and_then(|c| find_child(&object.children, c));
        let recalculation = owner
            .recalc
            .and_then(|r| object.recalculations.iter().find(|x| x.id == r));
        Some(GeneratedTypeRef {
            object,
            child,
            category: &owner.category,
            recalculation,
            name: name.clone(),
        })
    }
}

fn find_child(children: &[crate::model::Child], id: Guid) -> Option<&crate::model::Child> {
    let mut stack: Vec<&crate::model::Child> = children.iter().rev().collect();
    while let Some(c) = stack.pop() {
        if c.id == id {
            return Some(c);
        }
        stack.extend(c.children.iter().rev());
    }
    None
}
