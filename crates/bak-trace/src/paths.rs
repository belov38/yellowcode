#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    Relative,
    Absolute,
    Drive,
    Unc,
}

impl PathKind {
    pub fn tag(self) -> &'static str {
        match self {
            PathKind::Relative => "relative",
            PathKind::Absolute => "absolute",
            PathKind::Drive => "drive",
            PathKind::Unc => "unc",
        }
    }
}

fn is_separator(c: char) -> bool {
    c == '\\' || c == '/'
}

fn starts_with_drive(chars: &[char]) -> bool {
    matches!(chars, [letter, ':', ..] if letter.is_ascii_alphabetic())
}

pub fn classify(path: &str) -> PathKind {
    let chars: Vec<char> = path.chars().collect();
    match chars.as_slice() {
        [a, b, marker @ ('?' | '.'), c, rest @ ..]
            if is_separator(*a) && is_separator(*b) && is_separator(*c) =>
        {
            if starts_with_drive(rest) {
                PathKind::Drive
            } else if *marker == '?' && is_unc_prefix(rest) {
                PathKind::Unc
            } else if *marker == '?' {
                PathKind::Absolute
            } else {
                PathKind::Unc
            }
        }
        [a, b, ..] if is_separator(*a) && is_separator(*b) => PathKind::Unc,
        [a, ..] if is_separator(*a) => PathKind::Absolute,
        _ if starts_with_drive(&chars) => PathKind::Drive,
        _ => PathKind::Relative,
    }
}

fn is_unc_prefix(chars: &[char]) -> bool {
    matches!(chars, [u, n, c, sep, ..]
        if u.eq_ignore_ascii_case(&'u')
            && n.eq_ignore_ascii_case(&'n')
            && c.eq_ignore_ascii_case(&'c')
            && is_separator(*sep))
}
