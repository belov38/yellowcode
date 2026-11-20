#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Unc,
    Drive(char),
    Other,
}

pub fn path_kind(path: &str) -> PathKind {
    let p = path.replace('/', "\\");
    let starts = |prefix: &str| {
        p.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    };
    if starts(r"\\?\UNC\") || starts(r"\\.\UNC\") {
        return PathKind::Unc;
    }
    let local = if starts(r"\\?\") || starts(r"\\.\") {
        p.get(4..).unwrap_or_default()
    } else if starts(r"\\") {
        return PathKind::Unc;
    } else {
        p.as_str()
    };
    let mut chars = local.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => {
            PathKind::Drive(letter.to_ascii_uppercase())
        }
        _ => PathKind::Other,
    }
}

pub fn server_is_local(server: &str, computer: Option<&str>) -> bool {
    let mut name = server.trim();
    for prefix in ["tcp:", "lpc:", "np:", "admin:"] {
        if name
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        {
            name = name.get(prefix.len()..).unwrap_or_default();
            break;
        }
    }
    let name = name.split(',').next().unwrap_or_default();
    let name = name.split('\\').next().unwrap_or_default().trim();
    let name = name
        .strip_suffix('.')
        .filter(|n| !n.is_empty())
        .unwrap_or(name);
    ["localhost", ".", "(local)", "127.0.0.1", "::1", "[::1]"]
        .iter()
        .any(|local| name.eq_ignore_ascii_case(local))
        || computer.is_some_and(|c| !c.is_empty() && name.eq_ignore_ascii_case(c))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warning {
    Unc,
    MappedDrive,
}

impl Warning {
    pub fn text(self) -> &'static str {
        match self {
            Warning::Unc => {
                "yellowcode: warning: the backup is on a network share (UNC path); SQL Server reads it under the account \
                 of its service, not yours, and that account needs read access to the share; otherwise copy the file \
                 to the SQL Server machine"
            }
            Warning::MappedDrive => {
                "yellowcode: warning: the backup is on a network drive letter; a drive mapped in your session is not \
                 visible to the SQL Server service; give the UNC path (\\\\host\\share\\...) with read access for the \
                 account of the service, or copy the file to the SQL Server machine"
            }
        }
    }
}

pub fn warning_with(
    path: &str,
    server: &str,
    computer: Option<&str>,
    drive_is_remote: impl FnOnce(char) -> Option<bool>,
) -> Option<Warning> {
    match path_kind(path) {
        PathKind::Unc => Some(Warning::Unc),
        PathKind::Drive(letter)
            if server_is_local(server, computer) && drive_is_remote(letter) == Some(true) =>
        {
            Some(Warning::MappedDrive)
        }
        PathKind::Drive(_) | PathKind::Other => None,
    }
}

pub fn warning(path: &str, server: &str) -> Option<Warning> {
    let computer = std::env::var("COMPUTERNAME").ok();
    warning_with(path, server, computer.as_deref(), drive_is_remote)
}

#[cfg(windows)]
fn drive_is_remote(letter: char) -> Option<bool> {
    let root = std::fs::canonicalize(format!("{letter}:\\")).ok()?;
    Some(path_kind(&root.to_string_lossy()) == PathKind::Unc)
}

#[cfg(not(windows))]
fn drive_is_remote(_letter: char) -> Option<bool> {
    None
}
