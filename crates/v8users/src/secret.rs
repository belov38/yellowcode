use sha2::{Digest, Sha256};

pub const FILE_NAME_COLUMN: &str = "FILENAME";

const SECRET_COLUMNS: &[(&str, &[&str])] = &[
    ("V8USERS", &["DATA"]),
    ("V8USERSMATKEYS", &["MATKEYSH", "DATA"]),
    ("V8CMSDPWDS", &["PWDHASH"]),
];

const SECRET_FILES: &[(&str, &str)] = &[("PARAMS", "users.usr")];

const FILE_DATA_COLUMN: &str = "BINARYDATA";

const COPY_SUFFIXES: &[&str] = &["", "OG", "NG", "OLD"];

fn is_table(table: &str, base: &str) -> bool {
    let Some(rest) = table
        .get(..base.len())
        .filter(|head| head.eq_ignore_ascii_case(base))
        .and_then(|_| table.get(base.len()..))
    else {
        return false;
    };
    COPY_SUFFIXES.iter().any(|s| rest.eq_ignore_ascii_case(s))
}

pub fn may_hold_secrets(table: &str, column: &str) -> bool {
    is_secret(table, column, None)
        || SECRET_FILES
            .iter()
            .any(|(t, _)| is_table(table, t) && column.eq_ignore_ascii_case(FILE_DATA_COLUMN))
}

pub fn is_secret(table: &str, column: &str, file_name: Option<&str>) -> bool {
    let secret_column = SECRET_COLUMNS
        .iter()
        .any(|(t, cols)| is_table(table, t) && cols.iter().any(|c| column.eq_ignore_ascii_case(c)));
    secret_column
        || file_name.is_some_and(|file| {
            column.eq_ignore_ascii_case(FILE_DATA_COLUMN)
                && SECRET_FILES
                    .iter()
                    .any(|(t, f)| is_table(table, t) && file.eq_ignore_ascii_case(f))
        })
}

pub fn marker(value: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = format!("<secret: {} bytes, sha256 ", value.len());
    for b in Sha256::digest(value) {
        let _ = write!(s, "{b:02x}");
    }
    s.push('>');
    s
}
