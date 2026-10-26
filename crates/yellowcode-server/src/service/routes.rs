use crate::process::ProcessError;
use appobj::model::Uuid16;
use md_model::{ChildTag, Configuration, MdKind, Value};

type Parameters = Vec<(String, String)>;
type Match = (u8, usize, Parameters);
type Candidate<'a> = (&'a TemplateDef, (u8, usize), Parameters);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodDef {
    pub id: Uuid16,
    pub name: String,
    pub verb: String,
    pub handler: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateDef {
    pub id: Uuid16,
    pub name: String,
    pub pattern: String,
    pub methods: Vec<MethodDef>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceDef {
    pub id: Uuid16,
    pub name: String,
    pub root: String,
    pub reuse: String,
    pub session_max_age: u64,
    pub templates: Vec<TemplateDef>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceTable {
    pub services: Vec<ServiceDef>,
}
#[derive(Debug)]
pub struct Route<'a> {
    pub service: &'a ServiceDef,
    pub template: &'a TemplateDef,
    pub method: &'a MethodDef,
    pub route_prefix: String,
    pub relative_url: String,
    pub params: Vec<(String, String)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    #[error("unknown service root")]
    UnknownRoot,
    #[error("no matching template")]
    NotFound,
    #[error("no matching method")]
    Method,
    #[error("ambiguous service templates")]
    Ambiguous,
    #[error("invalid service template")]
    InvalidTemplate,
}
impl RouteError {
    pub fn status(&self) -> u16 {
        match self {
            Self::UnknownRoot => 503,
            Self::NotFound => 404,
            Self::Method => 405,
            Self::Ambiguous | Self::InvalidTemplate => 500,
        }
    }
}
fn text(value: Option<&Value>) -> Result<String, ProcessError> {
    match value {
        Some(Value::Text(s)) => Ok(s.clone()),
        _ => Err(ProcessError::Open),
    }
}
impl ServiceTable {
    pub fn new(services: Vec<ServiceDef>) -> Self {
        Self { services }
    }
    pub fn from_config(config: &Configuration) -> Result<Self, ProcessError> {
        let mut services = Vec::new();
        for service in config.of_kind(MdKind::HTTPService) {
            let mut templates = Vec::new();
            for template in service
                .children
                .iter()
                .filter(|c| c.tag == ChildTag::URLTemplate)
            {
                let mut methods = Vec::new();
                for method in template
                    .children
                    .iter()
                    .filter(|c| c.tag == ChildTag::Method)
                {
                    methods.push(MethodDef {
                        id: method.id,
                        name: method.name.clone(),
                        verb: text(method.prop("HTTPMethod"))?,
                        handler: text(method.prop("Handler"))?,
                    });
                }
                templates.push(TemplateDef {
                    id: template.id,
                    name: template.name.clone(),
                    pattern: text(template.prop("Template"))?,
                    methods,
                });
            }
            services.push(ServiceDef {
                id: service.id,
                name: service.name.clone(),
                root: text(service.prop("RootURL"))?,
                reuse: text(service.prop("ReuseSessions"))?,
                session_max_age: text(service.prop("SessionMaxAge"))?
                    .parse()
                    .map_err(|_| ProcessError::Open)?,
                templates,
            });
        }
        Ok(Self::new(services))
    }
    pub fn service<'a>(&'a self, path: &str) -> Result<&'a ServiceDef, RouteError> {
        let tail = path.strip_prefix("/hs/").ok_or(RouteError::UnknownRoot)?;
        let root = tail.split('/').next().ok_or(RouteError::UnknownRoot)?;
        self.services
            .iter()
            .find(|s| s.root == root)
            .ok_or(RouteError::UnknownRoot)
    }
    pub fn route<'a>(&'a self, verb: &str, path: &str) -> Result<Route<'a>, RouteError> {
        let service = self.service(path)?;
        let route_prefix = format!("/hs/{}", service.root);
        let relative_url = path
            .strip_prefix(&route_prefix)
            .ok_or(RouteError::NotFound)?
            .trim_end_matches('/');
        if service
            .templates
            .iter()
            .any(|t| t.pattern.trim_end_matches('/').is_empty())
        {
            return Err(RouteError::InvalidTemplate);
        }
        if relative_url.is_empty() {
            return Err(RouteError::NotFound);
        }
        let mut best: Option<Candidate<'_>> = None;
        for template in &service.templates {
            if let Some((class, segments, params)) = matches(&template.pattern, relative_url) {
                let score = (class, segments);
                if let Some((_, previous, _)) = &best {
                    if score == *previous {
                        return Err(RouteError::Ambiguous);
                    }
                    if score < *previous {
                        continue;
                    }
                }
                best = Some((template, score, params));
            }
        }
        let (template, _, params) = best.ok_or(RouteError::NotFound)?;
        let method = template
            .methods
            .iter()
            .find(|m| m.verb == verb)
            .or_else(|| template.methods.iter().find(|m| m.verb == "Any"))
            .ok_or(RouteError::Method)?;
        Ok(Route {
            service,
            template,
            method,
            route_prefix,
            relative_url: relative_url.into(),
            params,
        })
    }
}
fn put(params: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some((_, old)) = params.iter_mut().find(|(k, _)| k == key) {
        *old = value;
    } else {
        params.push((key.into(), value));
    }
}
fn matches(pattern: &str, path: &str) -> Option<Match> {
    let pattern: Vec<_> = pattern
        .trim_end_matches('/')
        .strip_prefix('/')?
        .split('/')
        .collect();
    let path: Vec<_> = path.strip_prefix('/')?.split('/').collect();
    let mut class = 2;
    let mut params = Vec::new();
    for (i, segment) in pattern.iter().enumerate() {
        let input = path.get(i).copied();
        if input == Some(*segment) {
            continue;
        }
        if *segment == "*" {
            if i + 1 == pattern.len() {
                let tail = path.get(i..).unwrap_or_default();
                let value = if tail.is_empty() {
                    String::new()
                } else {
                    format!("/{}", tail.join("/"))
                };
                put(&mut params, "*", value);
                if !tail.is_empty() {
                    class = 0;
                }
                return Some((class, pattern.len(), params));
            }
            return (input.is_some() && pattern.len() == path.len()).then_some((
                0,
                pattern.len(),
                params,
            ));
        }
        let input = input.filter(|s| !s.is_empty())?;
        let name = segment
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))?;
        class = class.min(1);
        put(&mut params, name, input.into());
    }
    (pattern.len() == path.len()).then_some((class, pattern.len(), params))
}
