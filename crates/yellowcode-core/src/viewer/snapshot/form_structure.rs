use super::{ManagedSnapshot, NamesCancellation, SelectedFormError};
use crate::viewer::{
    analysis_parent::AnalysisError,
    body::{self, ViewerBody},
    path::{self, ManagedTarget},
};
use mdconf::{
    ManagedEntrySource, MdKind, SubKind,
    forms::{Form, FormAttribute, FormCommand, FormItem, FormParameter},
};
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use std::{cell::Cell, time::Instant};
const MAX_DEPTH: usize = 64;
const MAX_ITEMS: usize = 100_000;

#[derive(Debug)]
pub enum FormStructureProjection {
    NotForm,
    Unavailable,
    Body(ViewerBody),
}
#[derive(Clone, Copy)]
enum Stop {
    Canceled,
    Timeout,
}
struct Visit<'a> {
    control: &'a dyn NamesCancellation,
    stop: Cell<Option<Stop>>,
    left: Cell<usize>,
    cut: Cell<bool>,
}
impl<'a> Visit<'a> {
    fn new(control: &'a dyn NamesCancellation) -> Self {
        Self {
            control,
            stop: Cell::new(None),
            left: Cell::new(MAX_ITEMS),
            cut: Cell::new(false),
        }
    }
    fn live(&self) -> bool {
        if self.stop.get().is_none() {
            if self.control.is_canceled() {
                self.stop.set(Some(Stop::Canceled));
            } else if self
                .control
                .deadline()
                .is_some_and(|at| Instant::now() >= at)
            {
                self.stop.set(Some(Stop::Timeout));
            }
        }
        self.stop.get().is_none()
    }
    fn check(&self) -> Result<(), AnalysisError> {
        self.live();
        match self.stop.get() {
            Some(Stop::Canceled) => Err(AnalysisError::Canceled),
            Some(Stop::Timeout) => Err(AnalysisError::Timeout),
            None => Ok(()),
        }
    }
}
impl<S: ManagedEntrySource> ManagedSnapshot<S> {
    pub fn form_structure_body(
        &self,
        path: &str,
        wire_limit: u64,
        control: &dyn NamesCancellation,
    ) -> Result<FormStructureProjection, SelectedFormError<S::Error>> {
        let visit = Visit::new(control);
        visit.check()?;
        let target = path::resolve_managed(self.open(), &self.names(), path);
        visit.check()?;
        let identity = match target? {
            ManagedTarget::Object { object, .. } if object.kind == MdKind::CommonForm => {
                Some((object.id, None))
            }
            ManagedTarget::Sub { owner, sub, .. } if sub.kind == SubKind::Form => {
                Some((owner.id, Some(sub.id)))
            }
            _ => None,
        };
        visit.check()?;
        let Some((owner, child)) = identity else {
            return Ok(FormStructureProjection::NotForm);
        };
        let selected = mdconf::startup::read_selected_form(self.open().context(), owner, child);
        visit.check()?;
        let selected = selected?;
        let Some(form) = selected.as_ref().and_then(|part| part.value.as_ref()) else {
            visit.check()?;
            return Ok(FormStructureProjection::Unavailable);
        };
        let body = project(self.catalog().memory_budget(), form, wire_limit, &visit);
        visit.check()?;
        Ok(FormStructureProjection::Body(body?))
    }
}
fn project(
    budget: &memory_budget::MemoryBudget,
    form: &Form,
    limit: u64,
    visit: &Visit<'_>,
) -> Result<ViewerBody, AnalysisError> {
    let body = body::json(budget, limit, &Summary { form, visit });
    visit.check()?;
    Ok(body?)
}
struct Summary<'a> {
    form: &'a Form,
    visit: &'a Visit<'a>,
}
impl Serialize for Summary<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(7))?;
        map.serialize_entry(
            "items",
            &Items {
                rows: &self.form.items,
                depth: 0,
                visit: self.visit,
            },
        )?;
        map.serialize_entry("truncated", &self.visit.cut.get())?;
        map.serialize_entry(
            "serialization",
            &mdconf::forms::serialization(self.form).map(Marker),
        )?;
        map.serialize_entry(
            "attributes",
            &Rows {
                rows: RowSource::Attributes(&self.form.attributes),
                visit: self.visit,
            },
        )?;
        map.serialize_entry(
            "commands",
            &Rows {
                rows: RowSource::Commands(&self.form.commands),
                visit: self.visit,
            },
        )?;
        map.serialize_entry(
            "parameters",
            &Rows {
                rows: RowSource::Parameters(&self.form.parameters),
                visit: self.visit,
            },
        )?;
        map.serialize_entry(
            "events",
            &Rows {
                rows: RowSource::Events(&self.form.events),
                visit: self.visit,
            },
        )?;
        map.end()
    }
}
struct Marker(mdconf::forms::Serialization);
impl Serialize for Marker {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}
struct Items<'a> {
    rows: &'a [FormItem],
    depth: usize,
    visit: &'a Visit<'a>,
}
impl Serialize for Items<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        for item in self.rows {
            if !self.visit.live() {
                break;
            }
            if self.visit.left.get() == 0 {
                self.visit.cut.set(true);
                break;
            }
            self.visit.left.set(self.visit.left.get() - 1);
            seq.serialize_element(&Item {
                item,
                depth: self.depth,
                visit: self.visit,
            })?;
        }
        seq.end()
    }
}
struct Item<'a> {
    item: &'a FormItem,
    depth: usize,
    visit: &'a Visit<'a>,
}
impl Serialize for Item<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("id", &self.item.id)?;
        map.serialize_entry("name", &self.item.name)?;
        map.serialize_entry("tag", &self.item.tag)?;
        let children = if self.depth < MAX_DEPTH {
            self.item.children.as_slice()
        } else {
            if !self.item.children.is_empty() {
                self.visit.cut.set(true);
            }
            &[]
        };
        map.serialize_entry(
            "children",
            &Items {
                rows: children,
                depth: self.depth + 1,
                visit: self.visit,
            },
        )?;
        map.end()
    }
}
enum RowSource<'a> {
    Attributes(&'a [FormAttribute]),
    Commands(&'a [FormCommand]),
    Parameters(&'a [FormParameter]),
    Events(&'a [(String, String)]),
}
struct Rows<'a> {
    rows: RowSource<'a>,
    visit: &'a Visit<'a>,
}
enum Row<'a> {
    Attribute(&'a FormAttribute),
    Command(&'a FormCommand),
    Parameter(&'a FormParameter),
    Event(&'a (String, String)),
}
impl Serialize for Rows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        macro_rules! emit {
            ($rows:expr, $variant:ident) => {
                for row in $rows {
                    if !self.visit.live() {
                        break;
                    }
                    seq.serialize_element(&Row::$variant(row))?;
                }
            };
        }
        match self.rows {
            RowSource::Attributes(rows) => {
                emit!(rows, Attribute);
            }
            RowSource::Commands(rows) => {
                emit!(rows, Command);
            }
            RowSource::Parameters(rows) => {
                emit!(rows, Parameter);
            }
            RowSource::Events(rows) => {
                emit!(rows, Event);
            }
        }
        seq.end()
    }
}
impl Serialize for Row<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(match self {
            Self::Attribute(_) | Self::Command(_) => 3,
            _ => 2,
        }))?;
        match self {
            Self::Attribute(row) => {
                map.serialize_entry("id", &row.id)?;
                map.serialize_entry("name", &row.name)?;
                map.serialize_entry("main", &row.main)?;
            }
            Self::Command(row) => {
                map.serialize_entry("id", &row.id)?;
                map.serialize_entry("name", &row.name)?;
                map.serialize_entry("action", &row.action)?;
            }
            Self::Parameter(row) => {
                map.serialize_entry("name", &row.name)?;
                map.serialize_entry("key", &row.key)?;
            }
            Self::Event(row) => {
                map.serialize_entry("event", &row.0)?;
                map.serialize_entry("handler", &row.1)?;
            }
        }
        map.end()
    }
}
