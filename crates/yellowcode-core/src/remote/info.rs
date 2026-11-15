use onec_client::Step;
use onec_client::auth::{AuthSettings, printable};
use onec_client::info::{MemberKind, ServerInfo};
use onec_client::metadata::MdKind;
use serde_json::{Value as Json, json};

pub(super) fn info_lines(i: &ServerInfo) -> Vec<String> {
    let mut l = Lines(Vec::new());
    l.head("server");
    l.kv("version", &i.server.version);
    l.kv("type", &i.server.server_type);
    l.kv("dbms", &i.server.dbms);
    l.kv("deployment", &i.server.deployment_type);
    l.kv("compress", &i.server.compress);

    let ib = &i.infobase;
    l.head("infobase");
    l.kv("alias", &ib.alias);
    l.kv("instance", &ib.instance_id);
    l.kv("file mode", &ib.file_mode);
    l.kv("locale", &ib.locale);
    let list = if ib.langs.list.is_empty() {
        "-".to_string()
    } else {
        ib.langs
            .list
            .iter()
            .map(|(id, name)| format!("{id} {name}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    l.raw(format!(
        "languages: {list} (current {}, default {})",
        shown(&ib.langs.current),
        shown(&ib.langs.default)
    ));

    let c = &i.config;
    l.head("configuration");
    l.kv("name", &c.name);
    l.kv("full name", &c.full_name);
    l.kv("code", &c.code);
    l.kv("version", &c.version);
    l.raw(format!("hash: {}", c.hash));
    l.kv("assembly", &c.assembly_version);
    l.kv("compatibility", &c.compatibility_mode);
    l.kv("extensions", &c.has_extensions);
    l.kv("license check", &c.license_check);
    l.kv("presentation", &c.presentation);

    let u = &i.user;
    l.head("user");
    l.kv("name", &u.name);
    l.kv("full name", &u.full_name);
    l.kv("id", &u.id);
    l.kv("roles id", &u.roles_id);
    l.kv("password min length", &u.password.min_length);
    l.kv(
        "password expiration ticks left",
        &u.password.expiration_ticks_left,
    );
    l.kv("password compliance status", &u.password.compliance_status);
    l.kv("password cannot change", &u.password.cannot_change);

    let s = &i.session;
    l.head("session");
    l.kv("seance", &s.seance);
    l.kv("number", &s.seance_no);
    if s.licenses.is_empty() {
        l.raw("license: -".into());
    }
    for lic in &s.licenses {
        l.raw(format!("license: {lic}"));
    }

    auth_lines(i.auth_settings.as_ref(), &mut l);

    l.head("modules");
    l.kv("main", &i.modules.main);
    if i.modules.server.is_empty() {
        l.raw("server: -".into());
    }
    for m in &i.modules.server {
        match &m.members {
            None => l.raw(format!("server: {}", m.code)),
            Some(members) => {
                l.raw(format!("server: {} ({})", m.code, members.len()));
                for x in members {
                    let defaults = if x.defaults.is_empty() {
                        String::new()
                    } else {
                        format!("; default {}", joined(&x.defaults))
                    };
                    let params = if x.params == 0 {
                        String::new()
                    } else {
                        x.params.to_string()
                    };
                    l.raw(format!("  {} {}({params}{defaults})", kind(x.kind), x.name));
                }
            }
        }
    }

    l.head("types");
    match &i.types {
        None => l.raw("-".into()),
        Some(t) => {
            l.kv("hash", &t.hash);
            let names: Vec<&str> = t.type_sets.iter().map(|(n, _)| n.as_str()).collect();
            l.raw(format!(
                "type sets ({}): {}",
                names.len(),
                if names.is_empty() {
                    "-".to_string()
                } else {
                    names.join(", ")
                }
            ));
            l.raw(format!("objects: {}", objects(&t.objects)));
        }
    }

    l.head("sections");
    match &i.sections {
        None => l.raw("-".into()),
        Some(v) if v.is_empty() => l.raw("section: -".into()),
        Some(v) => {
            for x in v {
                l.raw(format!("section: {} ({})", shown(&x.text), shown(&x.name)));
            }
        }
    }

    l.head("unrecognized");
    if i.unrecognized.is_empty() {
        l.raw("-".into());
    }
    for u in &i.unrecognized {
        l.raw(format!("{}: {}", u.answer, u.name));
    }
    l.0
}

fn auth_lines(a: Option<&AuthSettings>, l: &mut Lines) {
    l.head("auth settings");
    let Some(a) = a else {
        l.raw("-".into());
        return;
    };
    let lc = a.lcinfo.as_ref();
    let lc_auth = lc.and_then(|x| x.auth.as_ref());
    let types = match lc.and_then(|x| x.auth_types.as_ref()) {
        None => "-".to_string(),
        Some(v) if v.is_empty() => "(empty)".to_string(),
        Some(v) => quoted_list(v),
    };
    l.raw(format!("auth types: {types}"));
    l.kv("password recovery", &a.enable_password_recovery);
    l.kv("password recovery method", &a.password_recovery_method);
    l.raw(format!(
        "password recovery url: {}",
        a.password_recovery_url
            .as_deref()
            .map_or("-".to_string(), quoted)
    ));
    l.kv(
        "save for reauthentication",
        &a.allow_save_for_reauthentication,
    );
    l.kv(
        "save for reauthentication by default",
        &a.save_for_reauthentication_by_default,
    );
    l.kv(
        "saved authentication lifetime",
        &a.saved_authentication_lifetime,
    );
    l.kv(
        "lcinfo save for reauthentication",
        &lc_auth.and_then(|x| x.allow_save_for_reauthentication),
    );
    l.kv(
        "lcinfo save for reauthentication by default",
        &lc_auth.and_then(|x| x.save_for_reauthentication_by_default),
    );
    l.kv(
        "lcinfo saved authentication lifetime",
        &lc_auth.and_then(|x| x.saved_authentication_lifetime),
    );
    l.kv("inactivity timeout", &a.inactivity_timeout);
    l.kv("inactivity notification", &a.inactivity_notification);
    let unknown = if a.unrecognized.is_empty() {
        "-".to_string()
    } else {
        quoted_list(&a.unrecognized)
    };
    l.raw(format!("unrecognized: {unknown}"));
}

fn quoted(t: &str) -> String {
    let t = t.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", printable(&t))
}

fn quoted_list(v: &[String]) -> String {
    v.iter().map(|t| quoted(t)).collect::<Vec<_>>().join(", ")
}

pub(super) fn auth_only_lines(a: &AuthSettings) -> Vec<String> {
    let mut l = Lines(Vec::new());
    auth_lines(Some(a), &mut l);
    l.0
}

pub(super) fn auth_json(a: &AuthSettings) -> Json {
    json!({
        "password_recovery_method": a.password_recovery_method,
        "password_recovery_url": a.password_recovery_url,
        "allow_save_for_reauthentication": a.allow_save_for_reauthentication,
        "save_for_reauthentication_by_default": a.save_for_reauthentication_by_default,
        "saved_authentication_lifetime": a.saved_authentication_lifetime,
        "lcinfo": a.lcinfo.as_ref().map(|lc| json!({
            "auth": lc.auth.as_ref().map(|x| json!({
                "allow_save_for_reauthentication": x.allow_save_for_reauthentication,
                "saved_authentication_lifetime": x.saved_authentication_lifetime,
                "save_for_reauthentication_by_default": x.save_for_reauthentication_by_default,
            })),
            "auth_types": lc.auth_types,
        })),
        "enable_password_recovery": a.enable_password_recovery,
        "inactivity_timeout": a.inactivity_timeout,
        "inactivity_notification": a.inactivity_notification,
        "unrecognized": a.unrecognized,
    })
}

pub(super) fn info_json(i: &ServerInfo, steps: &[Step], auth_settings_error: Option<&str>) -> Json {
    let ib = &i.infobase;
    let c = &i.config;
    let u = &i.user;
    let p = &u.password;
    let mut j = json!({
        "server": {
            "version": i.server.version,
            "type": i.server.server_type,
            "dbms": i.server.dbms,
            "deployment_type": i.server.deployment_type,
            "compress": i.server.compress,
        },
        "infobase": {
            "alias": ib.alias,
            "instance_id": ib.instance_id,
            "file_mode": ib.file_mode,
            "locale": ib.locale,
            "langs": {
                "current": ib.langs.current,
                "default": ib.langs.default,
                "list": ib.langs.list.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
            },
        },
        "configuration": {
            "name": c.name,
            "full_name": c.full_name,
            "code": c.code,
            "version": c.version,
            "hash": c.hash,
            "assembly_version": c.assembly_version,
            "compatibility_mode": c.compatibility_mode,
            "has_extensions": c.has_extensions,
            "license_check": c.license_check,
            "presentation": c.presentation,
        },
        "user": {
            "name": u.name,
            "full_name": u.full_name,
            "id": u.id,
            "roles_id": u.roles_id,
            "password": {
                "min_length": p.min_length,
                "expiration_ticks_left": p.expiration_ticks_left,
                "compliance_status": p.compliance_status,
                "cannot_change": p.cannot_change,
            },
        },
        "session": {
            "seance": i.session.seance,
            "number": i.session.seance_no,
            "licenses": i.session.licenses,
        },
        "auth_settings": i.auth_settings.as_ref().map(auth_json),
        "modules": {
            "main": i.modules.main,
            "server": i.modules.server.iter().map(|m| json!({
                "code": m.code,
                "id": m.id,
                "members": m.members.as_ref().map(|v| v.iter().map(|x| json!({
                    "kind": kind(x.kind),
                    "name": x.name,
                    "params": x.params,
                    "defaults": x.defaults,
                })).collect::<Vec<_>>()),
            })).collect::<Vec<_>>(),
        },
        "types": i.types.as_ref().map(|t| json!({
            "hash": t.hash,
            "type_sets": t.type_sets.iter().map(|(name, clsid)| json!({"name": name, "clsid": clsid})).collect::<Vec<_>>(),
            "objects": t.objects.iter().map(|(k, n)| json!({"kind": k.en(), "count": n})).collect::<Vec<_>>(),
        })),
        "sections": i.sections.as_ref().map(|v| v.iter().map(|x| json!({
            "id": x.id,
            "name": x.name,
            "text": x.text,
        })).collect::<Vec<_>>()),
        "unrecognized": i.unrecognized.iter().map(|x| json!({"answer": x.answer, "name": x.name})).collect::<Vec<_>>(),
        "steps": steps.iter().map(|s| json!({"name": s.name, "status": s.status})).collect::<Vec<_>>(),
    });
    if let (Some(o), Some(e)) = (j.as_object_mut(), auth_settings_error) {
        let at = o
            .keys()
            .position(|k| k == "auth_settings")
            .map_or(o.len(), |i| i + 1);
        o.shift_insert(at, "auth_settings_error".into(), json!(e));
    }
    j
}

struct Lines(Vec<String>);

impl Lines {
    fn head(&mut self, name: &str) {
        self.0.push(format!("[{name}]"));
    }

    fn kv<T: std::fmt::Display>(&mut self, key: &str, v: &Option<T>) {
        self.0.push(format!("{key}: {}", shown(v)));
    }

    fn raw(&mut self, line: String) {
        self.0.push(line);
    }
}

fn shown<T: std::fmt::Display>(v: &Option<T>) -> String {
    v.as_ref()
        .map_or_else(|| "-".to_string(), ToString::to_string)
}

fn joined(v: &[u32]) -> String {
    v.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
}

fn kind(k: MemberKind) -> &'static str {
    match k {
        MemberKind::Func => "func",
        MemberKind::Proc => "proc",
    }
}

fn objects(v: &[(MdKind, usize)]) -> String {
    if v.is_empty() {
        return "-".to_string();
    }
    v.iter()
        .map(|(k, n)| format!("{} {n}", k.en()))
        .collect::<Vec<_>>()
        .join(", ")
}
