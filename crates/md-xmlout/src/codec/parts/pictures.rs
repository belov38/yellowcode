use super::each;
use crate::paths::object_dir;
use crate::{DUMP_VERSION, DumpSink, Part, PartStatus, Result};
use md_model::parts::*;
use md_model::*;

fn picture_xml(p: &Picture) -> String {
    let mut s = format!(
        "{}\r\n<ExtPicture xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         version=\"{DUMP_VERSION}\">\r\n\t<Picture>\r\n\t\t<xr:Abs>Picture.{}</xr:Abs>\r\n\
         \t\t<xr:LoadTransparent>{}</xr:LoadTransparent>",
        super::XML_HEAD,
        p.ext,
        p.load_transparent
    );
    if let Some((x, y)) = p.transparent_pixel {
        s.push_str(&format!(
            "\r\n\t\t<xr:TransparentPixel x=\"{x}\" y=\"{y}\"/>"
        ));
    }
    s.push_str("\r\n\t</Picture>\r\n</ExtPicture>");
    s
}

pub fn picture_files(dir: &str, file_stem: &str, p: &Picture) -> Vec<(String, Vec<u8>)> {
    vec![
        (
            format!("{dir}/{file_stem}/Picture.{}", p.ext),
            p.data.clone(),
        ),
        (
            format!("{dir}/{file_stem}.xml"),
            picture_xml(p).into_bytes(),
        ),
    ]
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    each(&cfg.objects, &mut |o| {
        if let Some(p) = &o.parts.picture {
            let dir = object_dir(cfg, o);
            if !dir.is_empty() {
                for (path, data) in picture_files(&format!("{dir}/Ext"), "Picture", p) {
                    sink.put(Part::Pictures, &path, &data)?;
                }
            }
        }
        Ok(())
    })?;
    for (name, p) in &cfg.parts.pictures {
        for (path, data) in picture_files("Ext", name, p) {
            sink.put(Part::Pictures, &path, &data)?;
        }
    }
    Ok(PartStatus::Written)
}
