use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TempDbName(String);

impl TempDbName {
    pub fn parse(name: &str) -> Option<Self> {
        let token = name.strip_prefix("yc_bak_")?;
        let generated = token.len() == 32
            && token
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        generated.then(|| TempDbName(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TempDbName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
