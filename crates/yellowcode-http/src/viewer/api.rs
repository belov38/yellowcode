use std::sync::{Arc, Mutex, MutexGuard};

use mdconf::Value as MdValue;
use serde_json::{Value, json};

use super::http::{Request, Response};
use super::state::State;
use yellowcode_core::viewer::check::{self, Ctx, Run};
use yellowcode_core::viewer::names::{child_tag_en, kind_name, module_name, sub_kind_en};
use yellowcode_core::viewer::path::{self, Target};
use yellowcode_core::viewer::{Lang, blocks, find, highlight, identity, module, outline, tree};

const MAX_HITS: usize = 500;

fn reply(status: u16, body: &Value) -> Response {
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::to_vec(body).unwrap_or_else(|_| b"null".to_vec()),
        ..Default::default()
    }
}

fn error(status: u16, msg: &str) -> Response {
    reply(status, &json!({ "error": msg }))
}

fn param<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.query
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn flag(req: &Request, name: &str) -> bool {
    matches!(param(req, name), Some("1" | "true"))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn handle(state: &State, req: &Request) -> Option<Response> {
    let r = match req.path.as_str() {
        "/api/info" => info(state),
        p if p == "/api/users" || p.starts_with("/api/users/") => super::users::handle(state, req),
        "/api/tree" => tree_children(state, req),
        "/api/show" => show(state, req),
        "/api/module" => module_text(state, req),
        "/api/check" => check_module(state, req),
        "/api/find" => find_hits(state, req),
        "/api/stats" => reply(200, &state.stats),
        p if p.starts_with("/api/") || p.starts_with("/res/icon/") => {
            match super::api_ext::handle(state, req) {
                Some(r) => r,
                None => error(404, "no such endpoint"),
            }
        }
        _ => return None,
    };
    Some(r)
}

fn synonym_of_config(state: &State) -> Option<String> {
    let code = if state.lang == Lang::Ru { "ru" } else { "en" };
    state
        .cfg
        .props
        .iter()
        .find(|p| p.name == "Synonym")
        .and_then(|p| match &p.value {
            MdValue::LStr(l) => {
                l.0.iter()
                    .find(|(c, _)| c == code)
                    .or_else(|| l.0.first())
                    .map(|(_, t)| t.clone())
                    .filter(|t| !t.is_empty())
            }
            _ => None,
        })
}

fn info(state: &State) -> Response {
    reply(
        200,
        &json!({
            "name": tree::config_name(&state.cfg),
            "synonym": synonym_of_config(state),
            "yellowcode_version": yellowcode_core::VERSION,
            "configuration_version": state.cfg.props.iter().find(|p| p.name == "Version").and_then(|p| match &p.value {
                MdValue::Text(value) => Some(value.as_str()),
                _ => None,
            }),
            "source": state.source.display().to_string(),
            "lang": if state.lang == Lang::Ru { "ru" } else { "en" },
            "stats": state.stats,
            "icons_v": yellowcode_core::viewer::icons::version(),
        }),
    )
}

fn tree_children(state: &State, req: &Request) -> Response {
    let id = match param(req, "id") {
        None => 0,
        Some(s) => match s.parse::<usize>() {
            Ok(n) => n,
            Err(_) => return error(400, "id: not a number"),
        },
    };
    let Some(parent) = state.nodes.get(id) else {
        return error(404, "no such node");
    };
    let kids: Vec<Value> = parent
        .children
        .iter()
        .filter_map(|&c| Some((c, state.nodes.get(c)?)))
        .map(|(c, f)| {
            json!({
                "id": c,
                "label": f.node.label,
                "path": f.node.path,
                "module": f.node.module,
                "synonym": f.node.synonym,
                "type_text": f.node.type_text,
                "icon": f.node.icon,
                "children": f.children.len(),
            })
        })
        .collect();
    reply(200, &Value::Array(kids))
}

fn not_found(e: &path::PathError) -> Response {
    reply(
        404,
        &json!({ "error": e.to_string(), "suggestions": e.suggestions }),
    )
}

pub(super) fn module_names(t: &Target) -> Vec<String> {
    module::modules_of(t)
        .iter()
        .map(|m| module_name(m.kind, Lang::En))
        .collect()
}

fn object_lists(o: &mdconf::MdObject) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let owner = format!("{}.{}", kind_name(o.kind, Lang::En), o.name);
    let sub = |s: &mdconf::SubObject| json!({ "name": s.name, "path": format!("{owner}.{}.{}", sub_kind_en(s.kind), s.name) });
    let forms = o.forms.iter().map(sub).collect();
    let templates = o.templates.iter().map(sub).collect();
    let commands = o
        .commands()
        .map(|c| {
            json!({ "name": c.name, "path": format!("{owner}.{}.{}", child_tag_en(c.tag), c.name) })
        })
        .collect();
    (forms, templates, commands)
}

fn show(state: &State, req: &Request) -> Response {
    let Some(p) = param(req, "path") else {
        return error(400, "path is required");
    };
    let target = match path::resolve(&state.cfg, p) {
        Ok(t) => t,
        Err(e) => return not_found(&e),
    };
    let props = yellowcode_core::viewer::show::show(&state.cfg, &target, state.lang);
    let (kind, name) = identity(&target, state.lang);
    let (forms, templates, commands) = match &target {
        Target::Object(o) => object_lists(o),
        _ => Default::default(),
    };
    reply(
        200,
        &json!({
            "kind": kind,
            "name": name,
            "properties": props,
            "groups": yellowcode_core::viewer::configurator::categories()
                .iter()
                .filter_map(|(en, _)| yellowcode_core::viewer::configurator::category_title(en, state.lang))
                .collect::<Vec<_>>(),
            "forms": forms,
            "templates": templates,
            "modules": module_names(&target),
            "commands": commands,
        }),
    )
}

fn module_text(state: &State, req: &Request) -> Response {
    let Some(p) = param(req, "path") else {
        return error(400, "path is required");
    };
    let target = match path::resolve(&state.cfg, p) {
        Ok(t) => t,
        Err(e) => return not_found(&e),
    };
    let which = param(req, "module")
        .map(str::to_string)
        .or_else(|| module_names(&target).into_iter().next());
    let m = match module::module_of(&state.cfg, &target, which.as_deref()) {
        Ok(m) => m,
        Err(e) => return error(404, &e.to_string()),
    };
    let text = m.text();
    let (items, syntax_error) = outline::outline(&text);
    let plain = param(req, "plain") == Some("1");
    let source_lines = module::split_lines(text.strip_prefix('\u{feff}').unwrap_or(&text));
    let line_count = source_lines.len();
    let source_text = plain.then(|| source_lines.join("\n"));
    let lines = (!plain).then(|| highlight::classified_lines(&text));
    reply(
        200,
        &json!({
            "name": module_name(m.kind, state.lang),
            "lines": lines,
            "text": source_text,
            "line_count": line_count,
            "blocks": blocks::blocks_with(&text, &items, syntax_error.as_deref()),
            "outline": items,
            "syntax_error": syntax_error,
            "modules": module_names(&target),
        }),
    )
}

fn names_dir(state: &State) -> yellowcode_core::Result<(std::path::PathBuf, Vec<String>)> {
    state
        .check
        .names_dir(&state.cfg, &state.source, state.names.as_deref())
}

fn check_module(state: &State, req: &Request) -> Response {
    let Some(p) = param(req, "path") else {
        return error(400, "path is required");
    };
    let target = match path::resolve(&state.cfg, p) {
        Ok(t) => t,
        Err(e) => return not_found(&e),
    };
    let which = param(req, "module")
        .map(str::to_string)
        .or_else(|| module_names(&target).into_iter().next());
    let m = match module::module_of(&state.cfg, &target, which.as_deref()) {
        Ok(m) => m,
        Err(e) => return error(404, &e.to_string()),
    };
    let item = check::items(&state.cfg)
        .into_iter()
        .find(|i| std::ptr::eq(i.module, m));
    let Some(item) = item else {
        return reply(
            200,
            &json!({ "diagnostics": [], "warnings": ["the module is not checked"] }),
        );
    };
    let key = item.name.clone();
    if let Some(a) = lock(&state.check.answers).get(&key) {
        return reply(200, a);
    }
    let answer = (|| -> yellowcode_core::Result<Value> {
        let (dir, mut warnings) = names_dir(state)?;
        let compat = check::compat_of(state.cfg.settings().compatibility_mode.0);
        let run = Run {
            ctxs: Ctx::Server,
            own_contexts: true,
            lang: state.lang,
        };
        let rep = check::check_items(compat, &dir, &[item], run)?;
        warnings.extend(rep.warnings);
        Ok(json!({ "diagnostics": rep.diags, "warnings": warnings }))
    })();
    match answer {
        Ok(v) => {
            let v = Arc::new(v);
            lock(&state.check.answers).insert(key, v.clone());
            reply(200, &v)
        }
        Err(e) => error(500, &e.to_string()),
    }
}

fn find_hits(state: &State, req: &Request) -> Response {
    let q = param(req, "q").unwrap_or_default();
    if q.is_empty() {
        return error(400, "q is required");
    }
    match find::find_limited(
        &state.cfg,
        q,
        flag(req, "regex"),
        flag(req, "names"),
        if flag(req, "names") && flag(req, "tree") {
            usize::MAX
        } else {
            MAX_HITS
        },
    ) {
        Ok((hits, total)) => reply(200, &json!({ "hits": hits, "total": total })),
        Err(e) => error(400, &e.to_string()),
    }
}
