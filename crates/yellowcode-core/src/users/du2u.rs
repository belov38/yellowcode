use super::*;
use serde_json::json;
use std::path::PathBuf;

#[doc(hidden)]
pub fn qualify_sealed_unix_clear(
    root: &Path,
    request_sha256: &str,
    base_sha256: &str,
) -> Result<UserWriteOutcome, UserWriteError> {
    let head = option_env!("DU2U_QUALIFICATION_HEAD").ok_or(UserWriteError::UnsupportedSource)?;
    let source =
        option_env!("DU2U_QUALIFICATION_SOURCE_SHA256").ok_or(UserWriteError::UnsupportedSource)?;
    let base = candidate(root, head, source, request_sha256, base_sha256)?;
    let snapshot = write_snapshot(&base)?;
    let mut names = snapshot
        .users
        .iter()
        .map(|card| card.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    if names != ["Du2aAdmin", "Du2aEdited", "Du2aUnrelated"] {
        return Err(UserWriteError::UnsupportedSource);
    }
    let target = snapshot
        .users
        .iter()
        .find(|card| card.name == "Du2aAdmin")
        .filter(|card| card.password_set && card.data_separation.is_empty())
        .ok_or(UserWriteError::UnsupportedSource)?;
    let request = UserWriteRequest {
        revision: snapshot.revision,
        confirmed: true,
        allow_last_user: false,
        action: UserAction::ClearPassword {
            identity: target.id.clone(),
        },
    };
    mutation::apply_qualified_clear(&base, &request, source)
}

fn candidate(
    root: &Path,
    head: &str,
    source: &str,
    request_sha256: &str,
    base_sha256: &str,
) -> Result<PathBuf, UserWriteError> {
    let refused = || UserWriteError::UnsupportedSource;
    let hex =
        |text: &str, length| text.len() == length && text.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex(head, 40)
        || !hex(source, 64)
        || !hex(request_sha256, 64)
        || source != base_sha256
        || !root.is_absolute()
        || std::fs::canonicalize(root).map_err(|_| refused())? != root
        || !root
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("du2u-copy-"))
    {
        return Err(refused());
    }
    let (raw, seal) = qualification_pin(&root.join("request.json"), 65536)?;
    if seal != request_sha256 {
        return Err(refused());
    }
    let request: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| refused())?;
    let base = root.join("candidate").join("1Cv8.1CD");
    let bytes = std::fs::metadata(&base).map_err(|_| refused())?.len();
    if request
        != json!({"schema":1,"stage":"DU2u","head":head,
        "source_binding":qualification::source_binding(),"platform":std::env::consts::OS,
        "root":root,"action":"clear_r13_admin_history_1_to_2",
        "source_sha256":source,"source_bytes":bytes})
        || qualification_pin(&base, 256 * 1_048_576)?.1 != source
    {
        return Err(refused());
    }
    Ok(base)
}
