use serde_json::{Value, json};

use super::http::{Request, Response};
use super::state::State;
use yellowcode_core::viewer::{
    definition, helper, object_window, predefined_view, rights_view, template_view,
};

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

pub fn handle(state: &State, req: &Request) -> Option<Response> {
    Some(match req.path.as_str() {
        "/api/definition" => find_definition(state, req),
        "/api/editor" => editor(),
        "/api/rights" => rights(state, req),
        "/api/object" => object(state, req),
        "/api/predefined" => predefined(state, req),
        "/api/exchange-membership" => exchange_membership(state, req),
        "/api/template" => template(state, req),
        "/api/form-structure" => form_structure(state, req),
        "/api/helper" => helper_find(state, req),
        "/api/helper/type" => helper_type(state, req),
        p if p.starts_with("/api/x/") => return super::x::handle(state, req),
        p if p.starts_with("/res/icon/") => icon(req),
        _ => return None,
    })
}

fn icon(req: &Request) -> Response {
    let key = req.path.strip_prefix("/res/icon/").unwrap_or_default();
    let Some((png, png2)) = yellowcode_core::viewer::icons::picture(key) else {
        return error(404, "no such icon");
    };
    let want2x = matches!(param(req, "2x"), Some("1" | "true"));
    Response {
        status: 200,
        content_type: "image/png",
        body: if want2x { png2.unwrap_or(png) } else { png }.to_vec(),
        cache_control: Some(
            if param(req, "v") == Some(yellowcode_core::viewer::icons::version()) {
                "max-age=31536000, immutable"
            } else {
                "no-cache"
            },
        ),
        ..Default::default()
    }
}

fn number(req: &Request, name: &str) -> Result<u32, Response> {
    param(req, name)
        .ok_or_else(|| error(400, &format!("{name} is required")))?
        .parse::<u32>()
        .map_err(|_| error(400, &format!("{name}: not a number")))
}

fn find_definition(state: &State, req: &Request) -> Response {
    let (Some(path), Some(module)) = (param(req, "path"), param(req, "module")) else {
        return error(400, "path and module are required");
    };
    let (line, col) = match (number(req, "line"), number(req, "col")) {
        (Ok(l), Ok(c)) => (l, c),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    match definition::definition(&state.cfg, path, module, line, col) {
        Some(loc) => reply(200, &serde_json::to_value(loc).unwrap_or(Value::Null)),
        None => error(404, "no definition"),
    }
}

fn editor() -> Response {
    let c = yellowcode_core::viewer::configurator::editor_colors();
    let e = yellowcode_core::viewer::configurator::editor_settings();
    let colors: serde_json::Map<String, Value> = c
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    reply(
        200,
        &json!({
            "colors": colors,
            "font": { "face": e.font_face, "pt": e.font_pt },
            "tab_size": e.tab_size,
        }),
    )
}

fn object(state: &State, req: &Request) -> Response {
    let Some(path) = param(req, "path") else {
        return error(400, "path is required");
    };
    match object_window::object_window(&state.cfg, path, state.lang) {
        Ok(v) => reply(200, &v),
        Err(e) => reply(
            404,
            &json!({ "error": e.to_string(), "suggestions": e.suggestions }),
        ),
    }
}

fn predefined(state: &State, req: &Request) -> Response {
    let Some(path) = param(req, "path") else {
        return error(400, "path is required");
    };
    match predefined_view::predefined(&state.cfg, path, state.lang) {
        Ok(v) => reply(200, &v),
        Err(predefined_view::PredefinedError::Path(e)) => reply(
            404,
            &json!({ "error": e.to_string(), "suggestions": e.suggestions }),
        ),
        Err(e) => error(404, &e.to_string()),
    }
}

fn exchange_membership(state: &State, req: &Request) -> Response {
    use mdconf::MdKind;
    use yellowcode_core::viewer::Lang;
    use yellowcode_core::viewer::names::kind_name;
    use yellowcode_core::viewer::path::{self, Target};

    const MAX_ITEMS: usize = 10_000;
    let Some(path) = param(req, "path").filter(|p| !p.trim().is_empty()) else {
        return error(400, "path is required");
    };
    let target = match path::resolve(&state.cfg, path) {
        Ok(Target::Object(o)) if state.cfg.objects.iter().any(|top| std::ptr::eq(top, o)) => o,
        Ok(_) => return error(400, "path must name a top-level metadata object"),
        Err(e) => {
            return reply(
                404,
                &json!({"error": e.to_string(), "suggestions": e.suggestions}),
            );
        }
    };
    let mut plans = Vec::new();
    let mut truncated = false;
    for plan in state
        .cfg
        .objects
        .iter()
        .filter(|o| o.kind == MdKind::ExchangePlan)
    {
        if plans.len() == MAX_ITEMS {
            truncated = true;
            break;
        }
        let content = plan.exchange_content();
        let member = content.and_then(|items| {
            items.iter().find(|item| match item.object.id {
                Some(id) => id == target.id,
                None => matches!(path::resolve(&state.cfg, &item.object.name),
                    Ok(Target::Object(o)) if std::ptr::eq(o, target)),
            })
        });
        plans.push(json!({
            "path": format!("{}.{}", kind_name(plan.kind, Lang::En), plan.name),
            "name": plan.name,
            "registered": content.map(|_| member.is_some()),
            "auto_record": member.map(|item| item.auto_record),
        }));
    }
    reply(200, &json!({"plans": plans, "truncated": truncated}))
}

fn rights(state: &State, req: &Request) -> Response {
    let Some(path) = param(req, "path") else {
        return error(400, "path is required");
    };
    let mut query = rights_view::RightsQuery {
        q: param(req, "q").unwrap_or("").trim().to_string(),
        only: matches!(param(req, "only"), Some("1" | "true")),
        templates: matches!(param(req, "templates"), Some("1" | "true")),
        ..Default::default()
    };
    for (name, slot) in [("from", &mut query.from), ("limit", &mut query.limit)] {
        if let Some(v) = param(req, name) {
            match v.parse::<usize>() {
                Ok(n) => *slot = n,
                Err(_) => return error(400, &format!("{name}: not a number")),
            }
        }
    }
    if query.limit == 0 {
        return error(400, "limit: must be positive");
    }
    match rights_view::rights_table(&state.cfg, path, state.lang, &query) {
        Some(t) => reply(200, &serde_json::to_value(t).unwrap_or(Value::Null)),
        None => error(404, "no rights for this path"),
    }
}

fn template(state: &State, req: &Request) -> Response {
    let Some(path) = param(req, "path") else {
        return error(400, "path is required");
    };
    let optional = |name: &str, default: u32| match param(req, name) {
        None => Ok(default),
        Some(v) => v
            .parse::<u32>()
            .map_err(|_| error(400, &format!("{name}: not a number"))),
    };
    let (from, rows) = match (
        optional("from", 0),
        optional("rows", template_view::DEFAULT_ROWS),
    ) {
        (Ok(f), Ok(r)) => (f, r),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    if rows == 0 {
        return error(400, "rows: must be positive");
    }
    match template_view::template_view_range(&state.cfg, path, from, rows) {
        Some(v) => reply(200, &serde_json::to_value(v).unwrap_or(Value::Null)),
        None => error(404, "no such template"),
    }
}

fn helper_find(state: &State, req: &Request) -> Response {
    let q = param(req, "q").unwrap_or("").trim();
    if q.is_empty() {
        return error(400, "q is required");
    }
    let limit = match param(req, "limit").map(str::parse::<usize>) {
        None => 50,
        Some(Ok(l)) => l.min(200),
        Some(Err(_)) => return error(400, "limit: not a number"),
    };
    let (hits, total) = helper::helper_find(q, state.lang, limit);
    reply(200, &json!({ "hits": hits, "total": total }))
}

fn helper_type(state: &State, req: &Request) -> Response {
    let Some(name) = param(req, "name") else {
        return error(400, "name is required");
    };
    match helper::helper_type(name, state.lang) {
        Some(t) => reply(200, &serde_json::to_value(t).unwrap_or(Value::Null)),
        None => error(404, "no such type"),
    }
}

const MAX_FORM_DEPTH: usize = 64;
const MAX_FORM_ITEMS: usize = 100_000;

fn item_json(
    it: &mdconf::forms::FormItem,
    depth: usize,
    left: &mut usize,
    cut: &mut bool,
) -> Value {
    *left = left.saturating_sub(1);
    let mut children = Vec::new();
    if depth < MAX_FORM_DEPTH {
        for c in &it.children {
            if *left == 0 {
                *cut = true;
                break;
            }
            children.push(item_json(c, depth + 1, left, cut));
        }
    } else if !it.children.is_empty() {
        *cut = true;
    }
    json!({ "id": it.id, "name": it.name, "tag": it.tag, "children": children })
}

fn form_structure(state: &State, req: &Request) -> Response {
    use mdconf::{MdKind, SubKind};
    use yellowcode_core::viewer::path::{self, Target};
    let Some(p) = param(req, "path").filter(|p| !p.is_empty()) else {
        return error(400, "path is required");
    };
    let target = match path::resolve(&state.cfg, p) {
        Ok(t) => t,
        Err(e) => {
            return reply(
                404,
                &json!({ "error": e.to_string(), "suggestions": e.suggestions }),
            );
        }
    };
    let form = match &target {
        Target::Sub { sub, .. } if sub.kind == SubKind::Form => sub.form.as_ref(),
        Target::Object(o) if o.kind == MdKind::CommonForm => o.form.as_ref(),
        _ => return error(404, "not a form"),
    };
    let Some(f) = form else {
        return error(
            404,
            "form structure unavailable: this build could not load a supported logical form",
        );
    };
    let mut left = MAX_FORM_ITEMS;
    let mut cut = false;
    let mut items = Vec::new();
    for i in &f.items {
        if left == 0 {
            cut = true;
            break;
        }
        items.push(item_json(i, 0, &mut left, &mut cut));
    }
    reply(
        200,
        &json!({
            "items": items,
            "truncated": cut,
            "serialization": mdconf::forms::serialization(f).map(|s| s.to_string()),
            "attributes": f.attributes.iter()
                .map(|a| json!({ "id": a.id, "name": a.name, "main": a.main })).collect::<Vec<_>>(),
            "commands": f.commands.iter()
                .map(|c| json!({ "id": c.id, "name": c.name, "action": c.action })).collect::<Vec<_>>(),
            "parameters": f.parameters.iter()
                .map(|q| json!({ "name": q.name, "key": q.key })).collect::<Vec<_>>(),
            "events": f.events.iter()
                .map(|(e, h)| json!({ "event": e, "handler": h })).collect::<Vec<_>>(),
        }),
    )
}
