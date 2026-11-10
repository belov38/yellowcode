use super::{Request, cleaned, has_server_error, query_timeout_seconds, run};
use crate::sql_server::SqlPlan;
use std::io::Read;

const LONGEST_WAIT: u64 = 120;
const MARGIN: u64 = 5;
const FAILURE_TEXT: usize = 300;

pub(super) fn wait_seconds(query_timeout: u64) -> u32 {
    u32::try_from(query_timeout.saturating_sub(MARGIN).min(LONGEST_WAIT)).unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LeftFile {
    pub(super) exists: Option<bool>,
    pub(super) megabytes: u64,
    pub(super) path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Step {
    NotTried,
    Gone,
    Kept(String),
    Busy(u32),
    NeedsSysadmin(Vec<LeftFile>),
    NotDropped { error: u32, text: String },
    Dropped(Vec<LeftFile>),
}

pub(super) fn digits(text: &str, longest: usize) -> Option<u64> {
    if text.is_empty() || text.len() > longest || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_file(rest: &str) -> Option<LeftFile> {
    let mut fields = rest.splitn(3, '|');
    let exists = match fields.next()? {
        "1" => Some(true),
        "0" => Some(false),
        "?" => None,
        _ => return None,
    };
    let megabytes = digits(fields.next()?, 12)?;
    let path = fields.next()?;
    (!path.is_empty()).then(|| LeftFile {
        exists,
        megabytes,
        path: path.to_owned(),
    })
}

pub(super) fn parse_answer(text: &str) -> Option<Step> {
    let mut files = Vec::new();
    let mut verdict = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if verdict.is_some() {
            return None;
        }
        match line.strip_prefix("YC_BAK_FILE|") {
            Some(rest) => files.push(parse_file(rest)?),
            None => verdict = Some(line),
        }
    }
    let verdict = verdict?;
    if verdict == "YC_BAK_DROPPED" {
        return Some(Step::Dropped(files));
    }
    if verdict == "YC_BAK_NEEDS_SYSADMIN" {
        return Some(Step::NeedsSysadmin(files));
    }
    if !files.is_empty() {
        return None;
    }
    if verdict == "YC_BAK_GONE" {
        return Some(Step::Gone);
    }
    if let Some(state) = verdict.strip_prefix("YC_BAK_KEPT:") {
        let known = !state.is_empty()
            && state.len() <= 32
            && state.bytes().all(|b| b.is_ascii_uppercase() || b == b'_');
        return known.then(|| Step::Kept(state.to_owned()));
    }
    if let Some(session) = verdict.strip_prefix("YC_BAK_BUSY:") {
        return digits(session, 9)
            .and_then(|n| u32::try_from(n).ok())
            .map(Step::Busy);
    }
    if let Some(rest) = verdict.strip_prefix("YC_BAK_NOT_DROPPED:") {
        let (error, text) = rest.split_once(':')?;
        let error = digits(error, 9).and_then(|n| u32::try_from(n).ok())?;
        return (!text.is_empty()).then(|| Step::NotDropped {
            error,
            text: text.to_owned(),
        });
    }
    None
}

pub(super) fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn list(files: &[&LeftFile]) -> String {
    files
        .iter()
        .map(|f| format!("{} ({} MB in the catalog)", plain(&f.path), f.megabytes))
        .collect::<Vec<_>>()
        .join(", ")
}

fn removed(base: &str, name: &str, files: &[LeftFile]) -> String {
    let head = format!("{base}; the unfinished temporary database {name} was removed");
    if files.is_empty() {
        return head;
    }
    let remain: Vec<&LeftFile> = files.iter().filter(|f| f.exists == Some(true)).collect();
    let unknown: Vec<&LeftFile> = files.iter().filter(|f| f.exists.is_none()).collect();
    if remain.is_empty() && unknown.is_empty() {
        return format!("{head}; none of its files remains on the SQL Server machine");
    }
    if remain.is_empty() {
        return format!(
            "{head}; could not check whether its files remain (SQL Server does not always delete the files of an interrupted RESTORE); look for them on the SQL Server machine: {}",
            list(&unknown)
        );
    }
    let mut text = format!(
        "{head}, but SQL Server left its files on disk (it does not always delete the files of an interrupted RESTORE); delete them by hand on the SQL Server machine: {}",
        list(&remain)
    );
    if !unknown.is_empty() {
        text.push_str(&format!(
            "; could not check whether these remain: {}",
            list(&unknown)
        ));
    }
    text
}

fn sysadmin_advice(name: &str, files: &[LeftFile]) -> String {
    let mut text = format!(
        "this login is not a member of the sysadmin role, which removing the database that an interrupted RESTORE leaves needs (the DROP fails for other logins, the owner included): ask a member of the sysadmin role to run DROP DATABASE [{name}]; and then to delete those of its files that remain"
    );
    if files.is_empty() {
        text.push_str(&format!(
            " (their paths: SELECT physical_name FROM sys.master_files WHERE database_id = DB_ID(N'{name}'), read before the DROP)"
        ));
    } else {
        let all: Vec<&LeftFile> = files.iter().collect();
        text.push_str(&format!(": {}", list(&all)));
    }
    text
}

pub(super) fn verdict(
    base: &str,
    name: &str,
    step: &Result<Step, String>,
    cleanup: &Result<(), String>,
) -> String {
    let failure = match (cleanup, step) {
        (Ok(()), Ok(Step::Dropped(files))) => return removed(base, name, files),
        (Ok(()), _) => {
            return format!(
                "{base}; temporary database {name} (cleanup confirmed that it is gone)"
            );
        }
        (Err(failure), _) => failure,
    };
    let failure = match step {
        Ok(Step::NotTried | Step::NeedsSysadmin(_)) => failure.clone(),
        Ok(Step::Busy(_) | Step::NotDropped { .. }) if cleaned::carries_size(failure) => {
            failure.clone()
        }
        Ok(Step::Busy(_) | Step::NotDropped { .. }) => {
            format!("{failure}; {}", cleaned::size_unknown(name))
        }
        _ => cleaned::complete(name, failure),
    };
    let mut text = format!(
        "{base}; temporary database {name} (server-side completion/cancellation unproven); could not safely clean temporary database {name}: {failure}"
    );
    match step {
        Ok(Step::Kept(state)) => text.push_str(&format!(
            "; its state is {state}, not RESTORING, so it was not dropped without the ownership check"
        )),
        Ok(Step::Busy(session)) => text.push_str(&format!(
            "; session {session} holds a lock on the database, probably a RESTORE that is still running: wait for it to end, or have a member of the sysadmin role run KILL {session}; and then DROP DATABASE [{name}];"
        )),
        Ok(Step::NeedsSysadmin(files)) => {
            text.push_str("; ");
            text.push_str(&sysadmin_advice(name, files));
        }
        Ok(Step::NotDropped { error, text: why }) => text.push_str(&format!(
            "; SQL Server refused to drop it (error {error}: {}); to remove it by hand a member of the sysadmin role runs: DROP DATABASE [{name}];",
            plain(why)
        )),
        Ok(Step::Dropped(_)) => {
            text.push_str("; the removal step had reported that it dropped the database");
        }
        Err(failed) if has_server_error(failed) => {
            let shown: String = failed.chars().take(FAILURE_TEXT).collect();
            let cut = if shown.len() < failed.len() { " [cut]" } else { "" };
            text.push_str(&format!(
                "; removing an unfinished restore also failed: {shown}{cut}"
            ));
        }
        Ok(Step::Gone | Step::NotTried) | Err(_) => {}
    }
    text
}

pub(super) fn attempt(request: &Request, plan: &SqlPlan) -> Result<Step, String> {
    let request = &super::cleanup_request(request);
    let _cleaning = super::lifecycle::OrdinaryCleanup::enter(
        plan.database_name(),
        super::cleanup_seconds(request),
    );
    let wait = wait_seconds(query_timeout_seconds(request.lifecycle_timeout));
    run(request, &plan.abandoned_restore_script(wait), |stdout| {
        let mut bytes = Vec::new();
        stdout
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("reading SQL response: {e}"))?;
        if bytes.len() > 8192 {
            return Err("unexpected SQL response".to_owned());
        }
        std::str::from_utf8(&bytes)
            .ok()
            .and_then(parse_answer)
            .ok_or_else(|| "unexpected SQL response".to_owned())
    })
    .map_err(|e| e.message)
}
