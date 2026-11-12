use super::QuerySchema;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

#[derive(Clone)]
pub struct SchemaOwner {
    backing: Backing,
}

#[derive(Clone)]
enum Backing {
    Legacy(Arc<QuerySchema>),
}

impl SchemaOwner {
    pub fn from_legacy(schema: Arc<QuerySchema>) -> Self {
        Self {
            backing: Backing::Legacy(schema),
        }
    }

    pub fn as_schema(&self) -> &QuerySchema {
        match &self.backing {
            Backing::Legacy(schema) => schema,
        }
    }

    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        match (&a.backing, &b.backing) {
            (Backing::Legacy(a), Backing::Legacy(b)) => Arc::ptr_eq(a, b),
        }
    }
}

impl AsRef<QuerySchema> for SchemaOwner {
    fn as_ref(&self) -> &QuerySchema {
        self.as_schema()
    }
}

impl Deref for SchemaOwner {
    type Target = QuerySchema;

    fn deref(&self) -> &QuerySchema {
        self.as_schema()
    }
}

impl fmt::Debug for SchemaOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match &self.backing {
            Backing::Legacy(_) => "Legacy",
        };
        f.debug_struct("SchemaOwner")
            .field("backing", &kind)
            .field("version", &self.as_schema().version())
            .finish()
    }
}
