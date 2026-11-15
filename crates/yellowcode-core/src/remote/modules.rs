use std::io::{BufRead, Write};
use std::path::Path;

use crate::output::Out;
use onec_client::session::Session;
use onec_client::{ModuleDef, ModuleDefs, ModuleId, ModuleSource, SessionError, Step};
use serde_json::{Value as Json, json};

use super::{ModuleUrn, ModulesRequest, open_session, steps_line, users_hint};
use crate::Result;

mod out;

use out::{check_output_dir, file_names, write_all, write_new_file};

struct Planned {
    id: ModuleId,
    listed: Option<usize>,
    file: String,
}

struct Fetched {
    defs: ModuleDefs,
    by_id: bool,
    sources: Vec<(Planned, ModuleSource)>,
}

enum Fail {
    Session(SessionError),
    Names(String),
}

pub(super) fn run(a: ModulesRequest, out: &mut Out, input: &mut dyn BufRead) -> Result<i32> {
    run_with(a, out, input, &mut write_new_file)
}

fn run_with(
    a: ModulesRequest,
    out: &mut Out,
    input: &mut dyn BufRead,
    write: &mut dyn FnMut(&Path, &[u8]) -> std::io::Result<()>,
) -> Result<i32> {
    let ids: Vec<ModuleId> = a.ids.into_iter().map(|ModuleUrn(id)| id).collect();
    for (i, id) in ids.iter().enumerate() {
        if ids.iter().take(i).any(|p| p == id) {
            return Err(format!("--id {id} is given twice").into());
        }
    }
    if let Some(dir) = &a.out {
        check_output_dir(dir)?;
    }
    let (mut session, login) = open_session(&a.login, input)?;
    let outcome = login
        .map_err(Fail::Session)
        .and_then(|()| fetch(&mut session, a.out.is_some(), &ids));
    let _ = session.logout();
    let fetched = match outcome {
        Ok(f) => f,
        Err(fail) => {
            if let Fail::Session(e) = &fail {
                users_hint(&a.login.user, e, out)?;
            }
            writeln!(out, "RESULT FAIL {}", steps_line(&session))?;
            return Err(match fail {
                Fail::Session(e) => e.into(),
                Fail::Names(m) => m.into(),
            });
        }
    };
    if let Some(dir) = &a.out {
        let files: Vec<(&str, &[u8])> = fetched
            .sources
            .iter()
            .map(|(p, s)| (p.file.as_str(), s.text.as_slice()))
            .collect();
        if let Err(e) = write_all(dir, &files, write) {
            writeln!(out, "RESULT FAIL {}", steps_line(&session))?;
            return Err(e);
        }
    }
    if a.json {
        let j = modules_json(&fetched, a.out.as_deref(), session.steps());
        writeln!(out, "{}", serde_json::to_string_pretty(&j)?)?;
    } else {
        for line in modules_lines(&fetched) {
            writeln!(out, "{line}")?;
        }
        writeln!(out, "RESULT OK {}", steps_line(&session))?;
    }
    Ok(0)
}

fn fetch(
    session: &mut Session,
    sources: bool,
    ids: &[ModuleId],
) -> std::result::Result<Fetched, Fail> {
    let defs = session.run_modules().map_err(Fail::Session)?;
    let by_id = !ids.is_empty();
    let wanted: Vec<(ModuleId, Option<usize>)> = if !sources {
        Vec::new()
    } else if by_id {
        ids.iter()
            .map(|id| (id.clone(), defs.modules.iter().position(|m| m.id == *id)))
            .collect()
    } else {
        defs.modules
            .iter()
            .enumerate()
            .filter(|(_, m)| m.kind.is_client())
            .map(|(i, m)| (m.id.clone(), Some(i)))
            .collect()
    };
    let stems: Vec<String> = wanted
        .iter()
        .map(|(id, listed)| {
            listed
                .and_then(|i| defs.modules.get(i))
                .map_or_else(|| id_stem(id), |m| m.title().to_string())
        })
        .collect();
    let files =
        file_names(&stems.iter().map(String::as_str).collect::<Vec<_>>()).map_err(Fail::Names)?;
    let mut fetched = Fetched {
        defs,
        by_id,
        sources: Vec::new(),
    };
    for ((id, listed), file) in wanted.into_iter().zip(files) {
        let src = session.module_source(&id).map_err(Fail::Session)?;
        fetched.sources.push((Planned { id, listed, file }, src));
    }
    Ok(fetched)
}

fn id_stem(id: &ModuleId) -> String {
    match id.ext() {
        Some(ext) => format!("{}.{}.ext{ext}", id.object(), id.property()),
        None => format!("{}.{}", id.object(), id.property()),
    }
}

fn shown(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    for c in t.chars() {
        if c.is_control()
            || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            s.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            s.push(c);
        }
    }
    s
}

fn module_line(m: &ModuleDef) -> String {
    let code = match (&m.name, &m.code) {
        (Some(_), Some(code)) => format!(" (code {})", shown(code)),
        _ => String::new(),
    };
    format!(
        "{} {}{code} version {} id {}",
        m.kind.tag(),
        shown(m.title()),
        m.id.version(),
        m.id
    )
}

fn file_lines(plan: &Planned, src: &ModuleSource, l: &mut Vec<String>) {
    l.push(format!(
        "  file: {} ({} bytes)",
        shown(&plan.file),
        src.text.len()
    ));
    if let Some(w) = src.charset_warning() {
        l.push(format!("  warning: {}", shown(&w)));
    }
}

fn modules_lines(f: &Fetched) -> Vec<String> {
    let mut l = vec![format!("[modules] ({})", f.defs.modules.len())];
    if f.defs.modules.is_empty() {
        l.push("-".into());
    }
    for (i, m) in f.defs.modules.iter().enumerate() {
        l.push(module_line(m));
        if !f.by_id
            && let Some((plan, src)) = f.sources.iter().find(|(p, _)| p.listed == Some(i))
        {
            file_lines(plan, src, &mut l);
        }
    }
    if f.by_id {
        l.push(format!("[by id] ({})", f.sources.len()));
        for (plan, src) in &f.sources {
            l.push(format!("id {}", plan.id));
            file_lines(plan, src, &mut l);
        }
    }
    l
}

fn source_json(plan: &Planned, src: &ModuleSource) -> Json {
    json!({
        "file": plan.file,
        "bytes": src.text.len(),
        "content_type": src.content_type,
        "charset_warning": src.charset_warning(),
        "etag": src.cache.etag,
        "cache_control": src.cache.cache_control,
    })
}

fn id_json(id: &ModuleId) -> Json {
    json!({
        "id": id.as_str(),
        "object": id.object(),
        "property": id.property(),
        "ext": id.ext(),
        "version": id.version(),
    })
}

fn modules_json(f: &Fetched, dir: Option<&Path>, steps: &[Step]) -> Json {
    let modules: Vec<Json> = f
        .defs
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let source = (!f.by_id)
                .then(|| f.sources.iter().find(|(p, _)| p.listed == Some(i)))
                .flatten()
                .map(|(p, s)| source_json(p, s));
            let mut j = json!({
                "kind": m.kind.tag(),
                "name": m.name,
                "code": m.code,
                "client": m.kind.is_client(),
            });
            if let (Json::Object(o), Json::Object(id)) = (&mut j, id_json(&m.id)) {
                o.extend(id);
                o.insert("cached".into(), json!(m.cached));
                o.insert("security_info".into(), json!(m.security_info));
                o.insert("source".into(), source.unwrap_or(Json::Null));
            }
            j
        })
        .collect();
    let by_id: Option<Vec<Json>> = f.by_id.then(|| {
        f.sources
            .iter()
            .map(|(p, s)| {
                let mut j = id_json(&p.id);
                if let Json::Object(o) = &mut j {
                    o.insert("listed".into(), json!(p.listed.is_some()));
                    o.insert("source".into(), source_json(p, s));
                }
                j
            })
            .collect()
    });
    json!({
        "modules": modules,
        "defs_cache": {"etag": f.defs.cache.etag, "cache_control": f.defs.cache.cache_control},
        "by_id": by_id,
        "out": dir.map(|d| d.display().to_string()),
        "steps": steps.iter().map(|s| json!({"name": s.name, "status": s.status})).collect::<Vec<_>>(),
    })
}
