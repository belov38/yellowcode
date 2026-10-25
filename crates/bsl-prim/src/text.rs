use std::rc::Rc;

#[derive(Clone, Default)]
pub struct Str(Rc<Vec<u16>>);

impl Str {
    pub fn new() -> Self {
        Str(Rc::new(Vec::new()))
    }

    pub fn from_units(units: &[u16]) -> Self {
        Str(Rc::new(units.to_vec()))
    }

    pub fn from_vec(v: Vec<u16>) -> Self {
        Str(Rc::new(v))
    }

    pub fn units(&self) -> &[u16] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn backing_capacity_units(&self) -> usize {
        self.0.capacity()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push_units(&mut self, units: &[u16]) {
        Rc::make_mut(&mut self.0).extend_from_slice(units);
    }

    pub fn concat(mut self, other: &[u16]) -> Str {
        self.push_units(other);
        self
    }

    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.0).to_string()
    }

    pub fn boost_hash(&self) -> i32 {
        crate::boost_hash::string(&self.0)
    }
}

impl From<&str> for Str {
    fn from(s: &str) -> Self {
        let units: Vec<u16> = s.encode_utf16().collect();
        Str(Rc::new(units))
    }
}

impl From<String> for Str {
    fn from(s: String) -> Self {
        let units: Vec<u16> = s.encode_utf16().collect();
        Str(Rc::new(units))
    }
}

impl PartialEq for Str {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_ref() == other.0.as_ref()
    }
}

impl Eq for Str {}

impl PartialOrd for Str {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Str {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_ref().cmp(other.0.as_ref())
    }
}

impl std::hash::Hash for Str {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.as_ref().hash(state);
    }
}

impl std::fmt::Debug for Str {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Str")
            .field("lossy", &self.to_string_lossy())
            .finish()
    }
}

pub fn fold_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(x), None) => x,
                _ => c,
            }
        })
        .collect()
}
