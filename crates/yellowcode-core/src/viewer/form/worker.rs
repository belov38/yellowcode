use super::super::Lang;
use form_model::{
    autoform::{GeneratedKind, facts, generate_view},
    preview_wire::{self, ErrorKind},
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Object,
    Group,
    List,
    Choice,
    Record,
}
impl Kind {
    fn native(self) -> GeneratedKind {
        match self {
            Self::Object => GeneratedKind::Object,
            Self::Group => GeneratedKind::Group,
            Self::List => GeneratedKind::List,
            Self::Choice => GeneratedKind::Choice,
            Self::Record => GeneratedKind::Record,
        }
    }
}
fn language(lang: Lang) -> mdconf::ScriptVariant {
    match lang {
        Lang::Ru => mdconf::ScriptVariant::Russian,
        Lang::En => mdconf::ScriptVariant::English,
    }
}
pub(crate) fn prepare(source: &[u8], kind: Option<Kind>, lang: Lang) -> Result<Vec<u8>, ()> {
    let form = load_form(source, kind, lang)?;
    let mut output = Vec::new();
    let sink = |chunk: &[u8]| {
        output.extend_from_slice(chunk);
        Ok::<_, std::convert::Infallible>(())
    };
    match form {
        Ok(form) => preview_wire::encode_requests(&form_model::discover_type_requests(&form), sink),
        Err((kind, message)) => preview_wire::encode_error(kind, &message, sink),
    }
    .map_err(|_| ())?;
    Ok(output)
}
type NativeForm = Result<form_model::FormDesc, (ErrorKind, String)>;
fn load_form(source: &[u8], kind: Option<Kind>, lang: Lang) -> Result<NativeForm, ()> {
    Ok(if let Some(kind) = kind {
        let facts = facts::decode(source).map_err(|_| ())?;
        generate_view(language(lang), &facts, kind.native())
            .map_err(|error| (ErrorKind::Autoform, error.to_string()))
    } else {
        let xml = std::str::from_utf8(source).map_err(|_| ())?;
        form_model::parse_form_xml(xml.trim_start_matches('\u{feff}'))
            .map_err(|error| (ErrorKind::Model, error.to_string()))
    })
}
pub(crate) fn roles(source: &[u8], lang: Lang) -> Result<Vec<u8>, ()> {
    let facts = facts::decode(source).map_err(|_| ())?;
    let roles: Vec<_> = [
        ("object", GeneratedKind::Object),
        ("group", GeneratedKind::Group),
        ("list", GeneratedKind::List),
        ("choice", GeneratedKind::Choice),
        ("record", GeneratedKind::Record),
    ]
    .into_iter()
    .filter(|(_, kind)| generate_view(language(lang), &facts, *kind).is_ok())
    .map(|(name, _)| name)
    .collect();
    serde_json::to_vec(&roles).map_err(|_| ())
}

pub(crate) fn layout(
    source: &[u8],
    kind: Option<Kind>,
    lang: Lang,
    name: &str,
    viewport: [u32; 2],
) -> Result<Vec<u8>, ()> {
    use form_model::{preview_answers, preview_layout as wire};
    if !(320..=3840).contains(&viewport[0]) || !(240..=2160).contains(&viewport[1]) {
        return Err(());
    }
    let input = wire::decode_input_checked(source, || Ok::<_, ()>(())).map_err(|_| ())?;
    let generated = kind.is_some();
    let form = load_form(input.source, kind, lang)?;
    let mut output = Vec::new();
    let mut sink = |bytes: &[u8]| {
        output.extend_from_slice(bytes);
        Ok::<_, ()>(())
    };
    match form {
        Err((kind, message)) => wire::encode_error(
            match kind {
                ErrorKind::Model => wire::ErrorKind::Model,
                ErrorKind::Autoform => wire::ErrorKind::Autoform,
            },
            &message,
            &mut sink,
        )
        .map_err(|_| ())?,
        Ok(form) => {
            let preview_wire::Prepared::Requests(requests) =
                preview_wire::decode_result(input.requests).map_err(|_| ())?
            else {
                return Err(());
            };
            let answers = preview_answers::decode(input.answers, requests).map_err(|_| ())?;
            let floating =
                generated && form_model::stdcmd::floating(form_model::stdcmd::form_role(&form));
            let preview = super::build(
                form,
                name,
                &answers,
                form_layout::Size {
                    w: viewport[0] as f32,
                    h: viewport[1] as f32,
                },
                floating,
                std::time::Instant::now(),
            );
            if answers.has_unknown_request() {
                return Err(());
            }
            match preview {
                Err(error) => {
                    wire::encode_error(wire::ErrorKind::Layout, &error.to_string(), &mut sink)
                        .map_err(|_| ())?
                }
                Ok(preview) => {
                    let pictures: Vec<_> = if generated {
                        Vec::new()
                    } else {
                        preview
                            .scene
                            .nodes
                            .iter()
                            .filter_map(|node| match &node.prim {
                                form_layout::scene::Prim::Picture { reference } => {
                                    Some(match reference {
                                        form_model::PictureRef::Std(s) => wire::Picture::Std(s),
                                        form_model::PictureRef::Config(s) => {
                                            wire::Picture::Config(s)
                                        }
                                        form_model::PictureRef::Embedded(s) => {
                                            wire::Picture::Embedded(s)
                                        }
                                    })
                                }
                                _ => None,
                            })
                            .collect()
                    };
                    wire::encode_pictures(&pictures, &mut sink).map_err(|_| ())?;
                }
            }
        }
    }
    Ok(output)
}

pub(crate) fn picture(source: &[u8], ext: &str) -> Result<Vec<u8>, ()> {
    use form_model::preview_picture::{self as wire, Image, Mime};
    let resource = super::picture_resource(ext, source);
    let image = match &resource {
        Some(resource) => Some(Image {
            mime: Mime::from_mime(resource.mime).ok_or(())?,
            bytes: &resource.bytes,
        }),
        None => None,
    };
    let mut output = Vec::new();
    wire::encode(image, |bytes| {
        output.extend_from_slice(bytes);
        Ok::<_, ()>(())
    })
    .map_err(|_| ())?;
    Ok(output)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RenderFormat {
    HtmlInline,
    HtmlUrls,
    Json,
    Text,
}
pub(crate) struct RenderOptions<'a> {
    pub kind: Option<Kind>,
    pub lang: Lang,
    pub name: &'a str,
    pub viewport: [u32; 2],
    pub form_path: Option<&'a str>,
    pub millis: u64,
    pub format: RenderFormat,
}
pub(crate) fn render(
    source: &[u8],
    assets: &[u8],
    options: RenderOptions<'_>,
) -> Result<Vec<u8>, ()> {
    use form_model::{preview_answers, preview_layout, preview_render as wire};
    let RenderOptions {
        kind,
        lang,
        name,
        viewport,
        form_path,
        millis,
        format,
    } = options;
    if !(320..=3840).contains(&viewport[0]) || !(240..=2160).contains(&viewport[1]) {
        return Err(());
    }
    let assets = wire::decode_assets_checked(
        assets,
        ui_style::package::MAX_FILE,
        ui_style::package::MAX_PACKAGE,
        || Ok::<_, ()>(()),
    )
    .map_err(|_| ())?;
    let generated = kind.is_some();
    if generated && (!assets.is_empty() || form_path.is_some()) {
        return Err(());
    }
    let input = preview_layout::decode_input_checked(source, || Ok::<_, ()>(())).map_err(|_| ())?;
    let form = load_form(input.source, kind, lang)?;
    let projected = match form {
        Err((kind, message)) => Err((
            match kind {
                ErrorKind::Model => preview_layout::ErrorKind::Model,
                ErrorKind::Autoform => preview_layout::ErrorKind::Autoform,
            },
            message,
        )),
        Ok(form) => {
            let preview_wire::Prepared::Requests(requests) =
                preview_wire::decode_result(input.requests).map_err(|_| ())?
            else {
                return Err(());
            };
            let answers = preview_answers::decode(input.answers, requests).map_err(|_| ())?;
            let floating =
                generated && form_model::stdcmd::floating(form_model::stdcmd::form_role(&form));
            let preview = super::build(
                form,
                name,
                &answers,
                form_layout::Size {
                    w: viewport[0] as f32,
                    h: viewport[1] as f32,
                },
                floating,
                std::time::Instant::now(),
            );
            if answers.has_unknown_request() {
                return Err(());
            }
            match preview {
                Err(error) => Err((preview_layout::ErrorKind::Layout, error.to_string())),
                Ok(mut preview) => {
                    preview.millis = u128::from(millis);
                    preview.form_path = form_path.map(str::to_owned);
                    for asset in assets.iter() {
                        let asset = asset.map_err(|_| ())?;
                        if preview
                            .pictures
                            .insert(
                                asset.key.to_owned(),
                                super::PictureResource {
                                    mime: asset.image.mime.as_str(),
                                    bytes: asset.image.bytes.to_vec(),
                                },
                            )
                            .is_some()
                        {
                            return Err(());
                        }
                    }
                    Ok(match format {
                        RenderFormat::HtmlInline => {
                            super::html::document(&preview, &super::html::Resources::Inline)
                        }
                        RenderFormat::HtmlUrls => super::html::document(
                            &preview,
                            &super::html::Resources::Urls {
                                prefix: "/res".to_owned(),
                            },
                        ),
                        RenderFormat::Json => {
                            serde_json::to_string(&super::preview_json(&preview)).map_err(|_| ())?
                        }
                        RenderFormat::Text => {
                            let mut output = Vec::new();
                            super::render_text(&preview, &mut output).map_err(|_| ())?;
                            String::from_utf8(output).map_err(|_| ())?
                        }
                    })
                }
            }
        }
    };
    let (kind, text) = match projected {
        Ok(text) => (None, text),
        Err((kind, message)) => (Some(kind), message),
    };
    let mut output = Vec::new();
    wire::encode_result(kind, &text, |bytes| {
        output.extend_from_slice(bytes);
        Ok::<_, ()>(())
    })
    .map_err(|_| ())?;
    Ok(output)
}
