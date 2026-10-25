use super::http::{Request, Response};

const INDEX: &str = include_str!("front/index.html");

const FILES: &[(&str, &str, &str)] = &[
    (
        "app.css",
        "text/css; charset=utf-8",
        include_str!("front/app.css"),
    ),
    (
        "commands.json",
        "application/json",
        include_str!("front/commands.json"),
    ),
    (
        "dialogs.json",
        "application/json",
        include_str!("front/dialogs.json"),
    ),
    (
        "NOTICE",
        "text/plain; charset=utf-8",
        include_str!("../../../../NOTICE"),
    ),
    ("js/dialogs.js", JS, include_str!("front/js/dialogs.js")),
    ("js/dcs.js", JS, include_str!("front/js/dcs.js")),
    (
        "js/dcs_settings.js",
        JS,
        include_str!("front/js/dcs_settings.js"),
    ),
    (
        "js/dcs_widgets.js",
        JS,
        include_str!("front/js/dcs_widgets.js"),
    ),
    ("js/commands.js", JS, include_str!("front/js/commands.js")),
    ("js/skin.js", JS, include_str!("front/js/skin.js")),
    (
        "editor.css",
        "text/css; charset=utf-8",
        include_str!("front/editor.css"),
    ),
    (
        "vendor/codemirror.js",
        JS,
        include_str!("front/vendor/codemirror.js"),
    ),
    ("js/editor.js", JS, include_str!("front/js/editor.js")),
    ("js/bsl-lang.js", JS, include_str!("front/js/bsl-lang.js")),
    ("js/api.js", JS, include_str!("front/js/api.js")),
    ("js/convert.js", JS, include_str!("front/js/convert.js")),
    ("js/desktop.js", JS, include_str!("front/js/desktop.js")),
    ("js/dom.js", JS, include_str!("front/js/dom.js")),
    ("js/form.js", JS, include_str!("front/js/form.js")),
    ("js/genform.js", JS, include_str!("front/js/genform.js")),
    ("js/helper.js", JS, include_str!("front/js/helper.js")),
    ("js/icon.js", JS, include_str!("front/js/icon.js")),
    ("js/i18n.js", JS, include_str!("front/js/i18n.js")),
    ("js/main.js", JS, include_str!("front/js/main.js")),
    ("js/menu.js", JS, include_str!("front/js/menu.js")),
    ("js/module.js", JS, include_str!("front/js/module.js")),
    ("js/props.js", JS, include_str!("front/js/props.js")),
    (
        "js/metadata_open.js",
        JS,
        include_str!("front/js/metadata_open.js"),
    ),
    (
        "js/predefined.js",
        JS,
        include_str!("front/js/predefined.js"),
    ),
    (
        "js/metadata_data.js",
        JS,
        include_str!("front/js/metadata_data.js"),
    ),
    (
        "js/metadata_widgets.js",
        JS,
        include_str!("front/js/metadata_widgets.js"),
    ),
    (
        "js/metadata_tabs.js",
        JS,
        include_str!("front/js/metadata_tabs.js"),
    ),
    ("js/rights.js", JS, include_str!("front/js/rights.js")),
    ("js/search.js", JS, include_str!("front/js/search.js")),
    ("js/tabs.js", JS, include_str!("front/js/tabs.js")),
    (
        "js/window_geometry.js",
        JS,
        include_str!("front/js/window_geometry.js"),
    ),
    ("js/template.js", JS, include_str!("front/js/template.js")),
    ("js/tree.js", JS, include_str!("front/js/tree.js")),
];

const JS: &str = "text/javascript; charset=utf-8";

pub fn handle(req: &Request) -> Option<Response> {
    let (content_type, text) = if req.path == "/" {
        ("text/html; charset=utf-8", INDEX)
    } else {
        let name = req.path.strip_prefix("/front/")?;
        let (_, ct, text) = FILES
            .iter()
            .chain(super::x::FILES)
            .find(|(n, _, _)| *n == name)?;
        (*ct, *text)
    };
    Some(Response {
        status: 200,
        content_type,
        body: text.as_bytes().to_vec(),
        ..Default::default()
    })
}
