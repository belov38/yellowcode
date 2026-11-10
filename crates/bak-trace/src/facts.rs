#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerFacts {
    pub product_year: Option<i64>,
    pub major: Option<i64>,
    pub minor: Option<i64>,
    pub build: Option<i64>,
    pub revision: Option<i64>,
    pub cumulative_update: Option<i64>,
    pub kb: Option<i64>,
    pub arch: &'static str,
    pub os: &'static str,
    pub edition: &'static str,
    pub collation: &'static str,
}

const COLLATIONS: &[&str] = &[
    "sql_latin1_general_cp1_ci_as",
    "sql_latin1_general_cp1_cs_as",
    "sql_latin1_general_cp1251_ci_as",
    "sql_latin1_general_cp1251_cs_as",
    "latin1_general_ci_as",
    "latin1_general_cs_as",
    "latin1_general_bin",
    "latin1_general_bin2",
    "latin1_general_100_ci_as",
    "latin1_general_100_cs_as",
    "latin1_general_100_ci_as_sc",
    "latin1_general_100_ci_as_sc_utf8",
    "latin1_general_100_bin2",
    "cyrillic_general_ci_as",
    "cyrillic_general_cs_as",
    "cyrillic_general_100_ci_as",
    "cyrillic_general_bin",
    "cyrillic_general_bin2",
];

fn number_after(text: &str, marker: &str) -> Option<i64> {
    text.match_indices(marker).find_map(|(at, _)| {
        let rest = text.get(at + marker.len()..)?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

fn version_quad(text: &str) -> Option<[i64; 4]> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find_map(|run| {
            let groups: Vec<&str> = run.split('.').collect();
            if groups.len() != 4 || groups.iter().any(|g| g.is_empty() || g.len() > 9) {
                return None;
            }
            let mut quad = [0_i64; 4];
            for (slot, group) in quad.iter_mut().zip(&groups) {
                *slot = group.parse().ok()?;
            }
            Some(quad)
        })
}

impl ServerFacts {
    pub fn parse(version: &str, edition: &str, collation: &str) -> Self {
        let first = version.lines().next().unwrap_or_default();
        let year = first
            .find("SQL Server ")
            .and_then(|at| first.get(at + "SQL Server ".len()..))
            .and_then(|rest| {
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                (digits.len() == 4).then(|| digits.parse().ok()).flatten()
            });
        let quad = version_quad(first);
        let at = |k: usize| quad.and_then(|q| q.get(k).copied());
        let arch = if first.contains("ARM64") {
            "arm64"
        } else if first.contains("X64") {
            "x64"
        } else if first.contains("X86") {
            "x86"
        } else {
            "other"
        };
        let os = if version.contains(" on Linux") {
            "linux"
        } else if version.contains(" on Windows") {
            "windows"
        } else {
            "other"
        };
        let lowered = edition.to_lowercase();
        let edition = [
            "edge",
            "azure",
            "developer",
            "express",
            "enterprise",
            "standard",
            "web",
        ]
        .into_iter()
        .find(|word| lowered.contains(word))
        .unwrap_or("other");
        let wanted = collation.trim().to_lowercase();
        let collation = COLLATIONS
            .iter()
            .copied()
            .find(|known| *known == wanted)
            .unwrap_or("other");
        ServerFacts {
            product_year: year,
            major: at(0),
            minor: at(1),
            build: at(2),
            revision: at(3),
            cumulative_update: number_after(first, "CU"),
            kb: number_after(first, "KB"),
            arch,
            os,
            edition,
            collation,
        }
    }
}
