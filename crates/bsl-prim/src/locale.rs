const SUNDAY: &[&str] = &[
    "AG", "AS", "AU", "BD", "BR", "BS", "BT", "BW", "BZ", "CA", "CN", "CO", "DM", "DO", "ET", "GT",
    "GU", "HK", "HN", "ID", "IL", "IN", "JM", "JP", "KE", "KH", "KR", "LA", "MH", "MM", "MO", "MT",
    "MX", "MZ", "NI", "NP", "PA", "PE", "PH", "PK", "PR", "PT", "PY", "SA", "SG", "SV", "TH", "TT",
    "TW", "UM", "US", "VE", "VI", "WS", "YE", "ZA", "ZW",
];

const SATURDAY: &[&str] = &[
    "AE", "AF", "BH", "DJ", "DZ", "EG", "IQ", "IR", "JO", "KW", "LY", "OM", "QA", "SD", "SY",
];

const FRIDAY: &[&str] = &["MV"];

fn likely_region(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "ru" => "RU",
        "en" => "US",
        "uk" => "UA",
        "be" => "BY",
        "kk" => "KZ",
        "uz" => "UZ",
        "az" => "AZ",
        "hy" => "AM",
        "ka" => "GE",
        "ro" => "RO",
        "de" => "DE",
        "fr" => "FR",
        "it" => "IT",
        "es" => "ES",
        "pl" => "PL",
        "lv" => "LV",
        "lt" => "LT",
        "et" => "EE",
        "bg" => "BG",
        "tr" => "TR",
        "vi" => "VN",
        "zh" => "CN",
        "ja" => "JP",
        "ko" => "KR",
        "ar" => "EG",
        _ => return None,
    })
}

pub fn first_weekday(code: &str) -> Option<u32> {
    let mut parts = code.split(['_', '-']);
    let lang = parts
        .next()
        .filter(|l| (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_alphabetic()))?;
    let lang = lang.to_ascii_lowercase();
    let region = match parts.next() {
        Some(r) if r.len() == 2 && r.chars().all(|c| c.is_ascii_alphabetic()) => {
            r.to_ascii_uppercase()
        }
        Some(_) => return None,
        None => likely_region(&lang)?.to_string(),
    };
    let r = region.as_str();
    Some(if SUNDAY.contains(&r) {
        7
    } else if SATURDAY.contains(&r) {
        6
    } else if FRIDAY.contains(&r) {
        5
    } else {
        1
    })
}
