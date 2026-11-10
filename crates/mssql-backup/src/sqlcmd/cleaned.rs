use super::unfinished::{digits, plain};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failed {
    Alter,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NotRemoved {
    pub(super) step: Failed,
    pub(super) error: u32,
    pub(super) megabytes: u64,
    pub(super) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Cleaned {
    Gone,
    NotRemoved(NotRemoved),
}

pub(super) fn parse_answer(text: &str) -> Option<Cleaned> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    if line == "YC_BAK_GONE" {
        return Some(Cleaned::Gone);
    }
    let mut fields = line.strip_prefix("YC_BAK_NOT_REMOVED:")?.splitn(4, ':');
    let step = match fields.next()? {
        "alter" => Failed::Alter,
        "drop" => Failed::Drop,
        _ => return None,
    };
    let error = digits(fields.next()?, 9).and_then(|n| u32::try_from(n).ok())?;
    let megabytes = digits(fields.next()?, 12)?;
    let text = fields.next()?;
    (!text.is_empty()).then(|| {
        Cleaned::NotRemoved(NotRemoved {
            step,
            error,
            megabytes,
            text: text.to_owned(),
        })
    })
}

pub(super) fn message(name: &str, failed: &NotRemoved) -> String {
    let what = match (failed.step, failed.error) {
        (_, 0) => plain(&failed.text),
        (Failed::Alter, error) => format!(
            "its sessions could not be removed: ALTER DATABASE ... SET SINGLE_USER WITH ROLLBACK IMMEDIATE failed (error {error}: {})",
            plain(&failed.text)
        ),
        (Failed::Drop, error) => format!(
            "DROP DATABASE failed (error {error}: {})",
            plain(&failed.text)
        ),
    };
    format!(
        "{what}; its size in the catalog is {} MB; to remove it, {}",
        failed.megabytes,
        command(name)
    )
}

const NOT_PROVED: &str = "temporary database ownership mismatch";

fn size_query(name: &str) -> String {
    format!(
        "SELECT SUM(CONVERT(bigint, size)) * 8 / 1024 AS size_mb FROM sys.master_files WHERE database_id = DB_ID(N'{name}')"
    )
}

fn command(name: &str) -> String {
    format!(
        "a login that may alter and drop it (the one that ran this command, or a member of the sysadmin role) runs: ALTER DATABASE [{name}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{name}]; (if the ALTER fails because the database is RESTORING, run the DROP alone; the DROP removes its files if the server can; otherwise delete the remaining files by hand; list them BEFORE the DROP with SELECT physical_name FROM sys.master_files WHERE database_id = DB_ID(N'{name}'))"
    )
}

pub(super) fn carries_size(failure: &str) -> bool {
    failure.contains("its size in the catalog is")
}

pub(super) fn size_unknown(name: &str) -> String {
    format!("size unknown (not read; {} shows it)", size_query(name))
}

pub(super) fn complete(name: &str, failure: &str) -> String {
    if carries_size(failure) {
        return failure.to_owned();
    }
    if failure.contains(NOT_PROVED) {
        return format!(
            "{failure}; the database carries no marker of this run, so this command did not remove it; {}; check that this is your database ({name} is the name that this run generated and printed first); if it is, {}",
            size_unknown(name),
            command(name)
        );
    }
    format!(
        "{failure}; {}; if it is still there, {}",
        size_unknown(name),
        command(name)
    )
}
