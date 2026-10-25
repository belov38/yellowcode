use super::{
    gschema::{Route, Task},
    read::{R, Read},
    scalar::Text,
};
use crate::templates::allocation::Vector;
use brace::Guid;
use std::fmt::Write;

pub(crate) trait TaskAccess {
    fn name(&self) -> &str;
    fn len(&self) -> usize;
    fn attr(&self, index: usize) -> Option<(&str, &str)>;
}
impl TaskAccess for Task {
    fn name(&self) -> &str {
        &self.name
    }
    fn len(&self) -> usize {
        self.attrs.len()
    }
    fn attr(&self, index: usize) -> Option<(&str, &str)> {
        self.attrs
            .get(index)
            .map(|(id, name)| (id.as_str(), name.as_str()))
    }
}
pub(crate) trait RouteAccess {
    fn task(&self, read: Read<'_>) -> R<Option<&dyn TaskAccess>>;
}
impl RouteAccess for Route {
    fn task(&self, read: Read<'_>) -> R<Option<&dyn TaskAccess>> {
        match &self.task {
            Ok(task) => Ok(task.as_ref().map(|task| task as &dyn TaskAccess)),
            Err(error) => Err(read.semantic("route task", || error.0.clone())),
        }
    }
}
pub(crate) struct TaskAttribute<'a> {
    id: Text<64>,
    name: &'a str,
}
impl<'a> TaskAttribute<'a> {
    pub(crate) fn new(id: Guid, name: &'a str, read: Read<'_>) -> R<Self> {
        let mut text = Text::new();
        write!(&mut text, "{id}").map_err(|_| read.invalid("task attribute UUID"))?;
        Ok(Self { id: text, name })
    }
}
pub(crate) struct TaskFacts<'a> {
    pub(crate) name: &'a str,
    pub(crate) attrs: Vector<TaskAttribute<'a>>,
}
impl TaskAccess for TaskFacts<'_> {
    fn name(&self) -> &str {
        self.name
    }
    fn len(&self) -> usize {
        self.attrs.len()
    }
    fn attr(&self, index: usize) -> Option<(&str, &str)> {
        self.attrs
            .get(index)
            .map(|attr| (attr.id.as_str(), attr.name))
    }
}
pub(crate) enum RouteFacts<'a> {
    Ready(Option<TaskFacts<'a>>),
    Deferred(&'static str),
}
impl RouteAccess for RouteFacts<'_> {
    fn task(&self, read: Read<'_>) -> R<Option<&dyn TaskAccess>> {
        match self {
            Self::Ready(task) => Ok(task.as_ref().map(|task| task as &dyn TaskAccess)),
            Self::Deferred(code) => Err(read.invalid(code)),
        }
    }
}
