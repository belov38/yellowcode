#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CompatMode {
    Version(u8, u8, u8),
    DontUse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompatFlags {
    pub map_ignores_undefined_key: bool,
    pub error_detail_without_categories: bool,
    pub no_bitwise_functions: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedCompat(pub String);

impl CompatMode {
    pub fn from_xml_name(name: &str) -> Result<CompatMode, UnsupportedCompat> {
        if name == "DontUse" {
            return Ok(CompatMode::DontUse);
        }

        if !name.starts_with("Version") {
            return Err(UnsupportedCompat(name.to_string()));
        }

        let rest = &name[7..];

        let parts: Vec<&str> = rest.split('_').collect();
        let [a, b, c] = parts[..] else {
            return Err(UnsupportedCompat(name.to_string()));
        };
        let num = |p: &str| -> Result<u8, UnsupportedCompat> {
            let plain = !p.is_empty()
                && p.bytes().all(|c| c.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'));
            plain
                .then(|| p.parse::<u8>().ok())
                .flatten()
                .ok_or_else(|| UnsupportedCompat(name.to_string()))
        };
        let (a, b, c) = (num(a)?, num(b)?, num(c)?);

        if a < 8 {
            return Err(UnsupportedCompat(name.to_string()));
        }

        if a == 8 {
            if b < 3 {
                return Err(UnsupportedCompat(name.to_string()));
            }
            if b == 3 && c < 10 {
                return Err(UnsupportedCompat(name.to_string()));
            }
        }

        Ok(CompatMode::Version(a, b, c))
    }

    pub fn xml_name(self) -> String {
        match self {
            CompatMode::DontUse => "DontUse".to_string(),
            CompatMode::Version(a, b, c) => format!("Version{}_{}_{}", a, b, c),
        }
    }

    pub fn flags(self) -> CompatFlags {
        match self {
            CompatMode::DontUse => CompatFlags::default(),
            CompatMode::Version(8, 3, 10) => CompatFlags {
                map_ignores_undefined_key: true,
                error_detail_without_categories: true,
                no_bitwise_functions: true,
            },
            CompatMode::Version(8, 3, c) if (11..=20).contains(&c) => CompatFlags {
                map_ignores_undefined_key: true,
                error_detail_without_categories: true,
                no_bitwise_functions: false,
            },
            _ => CompatFlags::default(),
        }
    }
}
