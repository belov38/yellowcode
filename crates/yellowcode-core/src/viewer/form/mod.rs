pub mod html;
pub(crate) mod worker;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::time::Instant;

use form_layout::{ItemState, LayoutInput, Size, WindowMode, scene::Scene};
use form_model::{FormDesc, MdResolver, NoConfig, TypeResolver};
use mdconf::{Configuration, MdKind, SubKind};

use super::path::{self, Target};
use crate::Result;

const VIEWPORT_W: std::ops::RangeInclusive<u32> = 320..=3840;
const VIEWPORT_H: std::ops::RangeInclusive<u32> = 240..=2160;

pub struct Preview {
    pub title: String,
    pub pictures: BTreeMap<String, PictureResource>,
    pub form_path: Option<String>,
    pub form: FormDesc,
    pub layout: form_layout::Layout,
    pub scene: Scene,
    pub viewport: Size,
    pub millis: u128,
}

pub fn parse_viewport(s: &str) -> Result<Size> {
    let bad = || format!("bad --viewport `{s}`: expected WxH, 320-3840 by 240-2160");
    let (w, h) = s.split_once(['x', 'X']).ok_or_else(bad)?;
    let w: u32 = w.trim().parse().map_err(|_| bad())?;
    let h: u32 = h.trim().parse().map_err(|_| bad())?;
    if !VIEWPORT_W.contains(&w) || !VIEWPORT_H.contains(&h) {
        return Err(bad().into());
    }
    Ok(Size {
        w: w as f32,
        h: h as f32,
    })
}

fn build(
    form: FormDesc,
    name: &str,
    types: &dyn TypeResolver,
    viewport: Size,
    floating: bool,
    t0: Instant,
) -> Result<Preview> {
    let panel = form_layout::content_box(viewport);
    let size = Size {
        w: panel.w,
        h: panel.h,
    };
    let state = ItemState::default();
    let window = if floating {
        WindowMode::Floating
    } else {
        WindowMode::Workspace
    };
    let layout = form_layout::layout(&LayoutInput {
        form: &form,
        types,
        interface: form_layout::Interface::Taxi,
        content: size,
        opened: size,
        state: &state,
        window,
        viewport,
    })
    .map_err(|e| format!("{e:?}"))?;
    let scene = form_layout::scene::scene(&form, &layout);
    let title = form
        .title
        .as_ref()
        .and_then(|t| t.any())
        .filter(|t| !t.is_empty())
        .unwrap_or(name)
        .to_string();
    Ok(Preview {
        pictures: BTreeMap::new(),
        form_path: None,
        title,
        form,
        layout,
        scene,
        viewport,
        millis: t0.elapsed().as_millis(),
    })
}

pub fn preview_cfg(cfg: &Configuration, path: &str, viewport: Size) -> Result<Preview> {
    let t0 = Instant::now();
    let (id, name, loaded) = match path::resolve(cfg, path)? {
        Target::Sub { sub, .. } if sub.kind == SubKind::Form => {
            (sub.id, sub.name.clone(), sub.form.as_ref())
        }
        Target::Object(o) if o.kind == MdKind::CommonForm => {
            (o.id, o.name.clone(), o.form.as_ref())
        }
        _ => return Err(format!("{path}: not a form").into()),
    };
    let Some((form, pictures)) = form_model::from_configuration(cfg, id)? else {
        if let Some(s) = loaded
            .and_then(mdconf::forms::serialization)
            .filter(|s| !s.is_4_59())
        {
            let code = mdconf::forms::form_refusal(cfg, id).unwrap_or_default();
            return Err(format!(
                "{path}: form preview unavailable: managed form serialization {s} is not translated: {code}"
            )
            .into());
        }
        return Err(
            format!("{path}: form preview unavailable: this build could not load or convert the form for rendering").into(),
        );
    };
    let mut preview = build(form, &name, &MdResolver { cfg }, viewport, false, t0)?;
    preview.form_path = Some(path.to_owned());
    preview.load_pictures(cfg, &pictures);
    Ok(preview)
}

pub fn parse_role(s: &str) -> Result<form_model::autoform::GeneratedKind> {
    use form_model::autoform::GeneratedKind as K;
    let s = s.trim();
    Ok(match s.to_ascii_lowercase().as_str() {
        "object" => K::Object,
        "group" => K::Group,
        "list" => K::List,
        "choice" => K::Choice,
        "record" => K::Record,
        _ => K::from_ru(s).ok_or_else(|| {
            format!("bad role `{s}`: expected object, group, list, choice or record")
        })?,
    })
}

pub fn generated_roles(cfg: &Configuration, object_path: &str) -> Result<Vec<&'static str>> {
    use form_model::autoform::GeneratedKind as K;
    let obj = match path::resolve(cfg, object_path)? {
        Target::Object(o) => o,
        _ => return Ok(Vec::new()),
    };
    Ok([
        ("object", K::Object),
        ("group", K::Group),
        ("list", K::List),
        ("choice", K::Choice),
        ("record", K::Record),
    ]
    .into_iter()
    .filter(|(_, k)| form_model::autoform::generate(cfg, obj, *k).is_ok())
    .map(|(n, _)| n)
    .collect())
}

pub fn preview_generated(
    cfg: &Configuration,
    object_path: &str,
    kind: form_model::autoform::GeneratedKind,
    viewport: Size,
) -> Result<Preview> {
    let t0 = Instant::now();
    let obj = match path::resolve(cfg, object_path)? {
        Target::Object(o) => o,
        _ => return Err(format!("{object_path}: not a metadata object").into()),
    };
    let form = form_model::autoform::generate(cfg, obj, kind)
        .map_err(|e| format!("{object_path}: {e}"))?;
    let floating = form_model::stdcmd::floating(form_model::stdcmd::form_role(&form));
    build(form, &obj.name, &MdResolver { cfg }, viewport, floating, t0)
}

pub fn preview_cfg_id(
    cfg: &Configuration,
    id: brace::Guid,
    name: &str,
    viewport: Size,
) -> Result<Option<Preview>> {
    let t0 = Instant::now();
    let Some((form, pictures)) = form_model::from_configuration(cfg, id)? else {
        return Ok(None);
    };
    let mut preview = build(form, name, &MdResolver { cfg }, viewport, false, t0)?;
    preview.load_pictures(cfg, &pictures);
    Ok(Some(preview))
}

pub fn preview_xml(xml: &str, viewport: Size) -> Result<Preview> {
    let t0 = Instant::now();
    let form = form_model::parse_form_xml(xml.trim_start_matches('\u{feff}'))?;
    build(form, "Form", &NoConfig, viewport, false, t0)
}

pub fn preview_json(p: &Preview) -> serde_json::Value {
    let boxes = serde_json::to_value(&p.layout.boxes).unwrap_or_default();
    serde_json::json!({
        "title": p.title,
        "viewport": [p.viewport.w, p.viewport.h],
        "content": [p.layout.content.w, p.layout.content.h],
        "boxes": boxes,
        "stubs": p.form.stats.stubs,
        "skipped": p.form.stats.skipped,
        "warnings": p.layout.warnings,
        "ms": p.millis as u64,
    })
}

pub fn render_text(p: &Preview, out: &mut dyn Write) -> Result<()> {
    let visible = p.layout.boxes.iter().filter(|b| !b.hidden).count();
    let stubs: u32 = p.form.stats.stubs.values().sum();
    let skipped: u32 = p.form.stats.skipped.values().sum();
    writeln!(out, "form: {}", p.title)?;
    writeln!(
        out,
        "content: {} x {}",
        p.layout.content.w, p.layout.content.h
    )?;
    writeln!(out, "boxes: {} ({visible} visible)", p.layout.boxes.len())?;
    writeln!(out, "display list: {} nodes", p.scene.nodes.len())?;
    writeln!(out, "stubs: {stubs}")?;
    writeln!(out, "skipped properties: {skipped}")?;
    writeln!(out, "warnings: {}", p.layout.warnings.len())?;
    for w in &p.layout.warnings {
        writeln!(out, "  {w}")?;
    }
    writeln!(out, "time: {} ms", p.millis)?;
    Ok(())
}

pub struct PictureResource {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

pub fn picture_key(reference: &form_model::PictureRef) -> String {
    match reference {
        form_model::PictureRef::Std(name) => format!("std:{name}"),
        form_model::PictureRef::Config(name) => format!("config:{name}"),
        form_model::PictureRef::Embedded(name) => format!("form:{name}"),
    }
}

pub fn configuration_picture(cfg: &Configuration, name: &str) -> Option<PictureResource> {
    let picture = cfg
        .common_picture(name)
        .or_else(|| brace::Guid::parse(name).and_then(|id| cfg.common_picture_by_id(id)))
        .or_else(|| cfg.config_picture(name))?;
    picture_resource(&picture.ext, &picture.data)
}

pub fn picture_resource(ext: &str, bytes: &[u8]) -> Option<PictureResource> {
    use ui_style::package::read_package;
    if ext.eq_ignore_ascii_case("zip") {
        let package = read_package(bytes).ok()?;
        let (name, bytes) = package
            .taxi_variant()
            .or_else(|| package.default_variant())?;
        return image_resource(name.rsplit('.').next()?, bytes);
    }
    image_resource(ext, bytes)
}

fn image_resource(ext: &str, bytes: &[u8]) -> Option<PictureResource> {
    if bytes.len() > ui_style::package::MAX_FILE {
        return None;
    }
    let mime = match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => return None,
    };
    Some(PictureResource {
        mime,
        bytes: bytes.to_vec(),
    })
}

impl Preview {
    fn load_pictures(&mut self, cfg: &Configuration, embedded: &[(String, Vec<u8>)]) {
        use form_layout::scene::Prim;
        use form_model::PictureRef;
        let mut total = 0usize;
        let mut seen = BTreeSet::new();
        for node in &self.scene.nodes {
            let Prim::Picture { reference } = &node.prim else {
                continue;
            };
            let key = picture_key(reference);
            if !seen.insert(key.clone()) {
                continue;
            }
            let resource = match reference {
                PictureRef::Std(_) => continue,
                PictureRef::Config(name) => configuration_picture(cfg, name),
                PictureRef::Embedded(name) => embedded_picture(embedded, name),
            };
            if let Some(resource) = resource {
                let Some(next) = total
                    .checked_add(resource.bytes.len())
                    .filter(|n| *n <= ui_style::package::MAX_PACKAGE)
                else {
                    continue;
                };
                total = next;
                self.pictures.insert(key, resource);
            }
        }
    }
}

pub fn embedded_picture(files: &[(String, Vec<u8>)], name: &str) -> Option<PictureResource> {
    if !ui_style::package::valid_path(name) {
        return None;
    }
    let (_, bytes) = files.iter().find(|(path, _)| path == name)?;
    picture_resource(name.rsplit('.').next()?, bytes)
}
