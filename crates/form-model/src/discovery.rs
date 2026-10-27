use crate::{FormDesc, Item, Resolved, TypeResolver, stdcmd};
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalTypeRequest {
    pub owner: String,
    pub path: Vec<String>,
}

#[derive(Default)]
struct Collector(RefCell<Vec<ExternalTypeRequest>>);
impl TypeResolver for Collector {
    fn resolve(&self, owner: &str, path: &[&str]) -> Option<Resolved> {
        let mut requests = self.0.borrow_mut();
        if !requests.iter().any(|row| {
            row.owner == owner && row.path.iter().map(String::as_str).eq(path.iter().copied())
        }) {
            requests.push(ExternalTypeRequest {
                owner: owner.to_owned(),
                path: path.iter().map(|part| (*part).to_owned()).collect(),
            });
        }
        None
    }
}

pub fn discover_type_requests(form: &FormDesc) -> Vec<ExternalTypeRequest> {
    fn visit(form: &FormDesc, item: &Item, collector: &Collector) {
        if let Some(path) = &item.props.data_path {
            let _ = form.resolve(path, collector);
        }
        for child in item.children.iter().chain(&item.parts) {
            visit(form, child, collector);
        }
    }
    let collector = Collector::default();
    for item in form.auto_command_bar.iter().chain(&form.items) {
        visit(form, item, &collector);
    }
    let _ = stdcmd::form_role_with(form, &collector);
    collector.0.into_inner()
}
