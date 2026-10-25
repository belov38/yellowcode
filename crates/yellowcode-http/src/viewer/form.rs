use form_layout::Size;
use serde_json::json;
use ui_style::{FONT_FILES, PicFormat, std_picture};

use super::http::{Request, Response};
use super::state::State;
use yellowcode_core::viewer::form::html::{Resources, document};
use yellowcode_core::viewer::form::{self, Preview};
use yellowcode_core::viewer::path;

const FORM_CSP: &str =
    "default-src 'none'; img-src 'self'; font-src 'self'; style-src 'unsafe-inline'";
const SVG_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'";
const IMMUTABLE: &str = "max-age=31536000, immutable";
const DEFAULT_W: u32 = 1440;
const DEFAULT_H: u32 = 900;

fn json_reply(status: u16, body: &serde_json::Value) -> Response {
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::to_vec(body).unwrap_or_else(|_| b"null".to_vec()),
        ..Default::default()
    }
}

fn error(status: u16, msg: &str) -> Response {
    json_reply(status, &json!({ "error": msg }))
}

fn param<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.query
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn dim(
    req: &Request,
    name: &str,
    default: u32,
    range: std::ops::RangeInclusive<u32>,
) -> Option<u32> {
    match param(req, name) {
        None => Some(default),
        Some(s) => s.parse().ok().filter(|v| range.contains(v)),
    }
}

fn build(state: &State, req: &Request) -> Result<Preview, Response> {
    let Some(p) = param(req, "path").filter(|p| !p.is_empty()) else {
        return Err(error(400, "path: required"));
    };
    let (Some(w), Some(h)) = (
        dim(req, "w", DEFAULT_W, 320..=3840),
        dim(req, "h", DEFAULT_H, 240..=2160),
    ) else {
        return Err(error(400, "w, h: expected 320-3840 by 240-2160"));
    };
    if let Err(e) = path::resolve(&state.cfg, p) {
        return Err(json_reply(
            404,
            &json!({ "error": e.to_string(), "suggestions": e.suggestions }),
        ));
    }
    let viewport = Size {
        w: w as f32,
        h: h as f32,
    };
    let built = match param(req, "generated") {
        Some(role) => {
            let kind = form::parse_role(role).map_err(|e| error(400, &e.to_string()))?;
            form::preview_generated(&state.cfg, p, kind, viewport)
        }
        None => form::preview_cfg(&state.cfg, p, viewport),
    };
    built.map_err(|e| error(422, &e.to_string()))
}

pub(super) fn message_page(status: u16, text: &str, refresh: bool) -> Response {
    let esc = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let meta = if refresh {
        "<meta http-equiv=\"refresh\" content=\"1\">"
    } else {
        ""
    };
    Response {
        status,
        content_type: "text/html; charset=utf-8",
        body: format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\">{meta}<title>{esc}</title></head>\
             <body style=\"font: 13px sans-serif; color: #555; margin: 16px\">{esc}</body></html>"
        )
        .into_bytes(),
        csp: Some(FORM_CSP),
        ..Default::default()
    }
}

fn binary(content_type: &'static str, body: Vec<u8>) -> Response {
    Response {
        status: 200,
        content_type,
        body,
        cache_control: Some(IMMUTABLE),
        ..Default::default()
    }
}

pub fn handle(state: &State, req: &Request) -> Option<Response> {
    let r = match req.path.as_str() {
        "/form" => match build(state, req) {
            Ok(p) => Response {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: document(
                    &p,
                    &Resources::Urls {
                        prefix: "/res".into(),
                    },
                )
                .into_bytes(),
                csp: Some(FORM_CSP),
                ..Default::default()
            },
            Err(e) => e,
        },
        "/api/form" => match build(state, req) {
            Ok(p) => json_reply(200, &form::preview_json(&p)),
            Err(e) => e,
        },
        "/api/form/roles" => {
            let Some(p) = param(req, "path").filter(|p| !p.is_empty()) else {
                return Some(error(400, "path: required"));
            };
            if let Err(e) = path::resolve(&state.cfg, p) {
                return Some(json_reply(
                    404,
                    &json!({ "error": e.to_string(), "suggestions": e.suggestions }),
                ));
            }
            match form::generated_roles(&state.cfg, p) {
                Ok(roles) => json_reply(200, &json!({ "roles": roles })),
                Err(e) => error(422, &e.to_string()),
            }
        }
        p => {
            if let Some(file) = p.strip_prefix("/res/font/") {
                match FONT_FILES.iter().find(|(n, _)| *n == file) {
                    Some((_, bytes)) => binary("font/ttf", bytes.to_vec()),
                    None => error(404, "no such font"),
                }
            } else if let Some(name) = p.strip_prefix("/res/cpic/") {
                match form::configuration_picture(&state.cfg, name) {
                    Some(pic) => picture_reply(pic),
                    None => error(404, "no such picture or supported variant"),
                }
            } else if let Some(name) = p.strip_prefix("/res/fpic/") {
                if !ui_style::package::valid_path(name) {
                    error(400, "invalid picture path")
                } else if let Some(form_path) = param(req, "path") {
                    match form_picture(state, form_path, name) {
                        Some(pic) => picture_reply(pic),
                        None => error(404, "no such form picture or supported variant"),
                    }
                } else {
                    error(400, "path: required")
                }
            } else {
                let id = p.strip_prefix("/res/pic/")?;
                match std_picture(id) {
                    Some(pic) => match pic.format {
                        PicFormat::Png => binary("image/png", pic.bytes.to_vec()),
                        PicFormat::Svg => Response {
                            csp: Some(SVG_CSP),
                            ..binary("image/svg+xml", pic.bytes.to_vec())
                        },
                    },
                    None => error(404, "no such picture"),
                }
            }
        }
    };
    Some(r)
}

fn picture_reply(pic: form::PictureResource) -> Response {
    let svg = pic.mime == "image/svg+xml";
    Response {
        csp: svg.then_some(SVG_CSP),
        cache_control: Some("no-cache"),
        ..binary(pic.mime, pic.bytes)
    }
}

fn form_picture(state: &State, form_path: &str, name: &str) -> Option<form::PictureResource> {
    use mdconf::{MdKind, SubKind};
    let id = match path::resolve(&state.cfg, form_path).ok()? {
        path::Target::Sub { sub, .. } if sub.kind == SubKind::Form => sub.id,
        path::Target::Object(object) if object.kind == MdKind::CommonForm => object.id,
        _ => return None,
    };
    let (_, pictures, _) = mdconf::forms::form_xml(&state.cfg, id)?;
    form::embedded_picture(&pictures, name)
}
