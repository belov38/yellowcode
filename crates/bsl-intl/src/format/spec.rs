pub const MAX_DIGITS: u32 = 1000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormatSpec {
    pub nd: Option<u32>,
    pub nfd: Option<u32>,
    pub ns: Option<i32>,
    pub nds: Option<String>,
    pub ngs: Option<String>,
    pub nz: Option<String>,
    pub nlz: bool,
    pub ng: Option<(u32, Option<u32>)>,
    pub nn: Option<u8>,
    pub nf: Option<String>,
    pub bf: Option<String>,
    pub bt: Option<String>,
    pub df: Option<String>,
    pub dlf: Option<String>,
    pub de: Option<String>,
    pub locale: Option<String>,
}

fn int_value(v: &str) -> i64 {
    let t = v.trim_start();
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut n: i64 = 0;
    for c in rest.chars() {
        let Some(d) = c.to_digit(10) else { break };
        n = n.saturating_mul(10).saturating_add(i64::from(d));
    }
    if neg { -n } else { n }
}

fn digits(v: &str) -> u32 {
    int_value(v).clamp(0, i64::from(MAX_DIGITS)) as u32
}

impl FormatSpec {
    fn set(&mut self, key: &str, v: String) {
        match key {
            "ЧЦ" | "ND" => self.nd = Some(digits(&v)),
            "ЧДЦ" | "NFD" => self.nfd = Some(digits(&v)),
            "ЧС" | "NS" => {
                let m = i64::from(MAX_DIGITS);
                self.ns = Some(int_value(&v).clamp(-m, m) as i32);
            }
            "ЧРД" | "NDS" => self.nds = Some(v),
            "ЧРГ" | "NGS" => self.ngs = Some(v),
            "ЧН" | "NZ" => self.nz = Some(v),
            "ЧВН" | "NLZ" => self.nlz = true,
            "ЧГ" | "NG" => {
                let mut sizes = v.split(',');
                let first = digits(sizes.next().unwrap_or(""));
                self.ng = Some((first, sizes.next().map(digits)));
            }
            "ЧО" | "NN" => {
                if let Ok(s @ 0..=4) = u8::try_from(int_value(&v)) {
                    self.nn = Some(s);
                }
            }
            "ЧФ" | "NF" => self.nf = Some(v),
            "БЛ" | "BF" => self.bf = Some(v),
            "БИ" | "BT" => self.bt = Some(v),
            "ДФ" | "DF" => self.df = Some(v),
            "ДЛФ" | "DLF" => self.dlf = Some(v),
            "ДП" | "DE" => self.de = Some(v),
            "Л" | "L" => self.locale = Some(v),
            _ => {}
        }
    }
}

pub fn parse_spec(fmt: &[u16]) -> FormatSpec {
    let mut spec = FormatSpec::default();
    for (key, value) in pairs_with(fmt, true) {
        spec.set(&key, value);
    }
    spec
}

pub(crate) fn pairs(fmt: &[u16]) -> Vec<(String, String)> {
    pairs_with(fmt, false)
}

fn pairs_with(fmt: &[u16], ends_at_space: bool) -> Vec<(String, String)> {
    let text: Vec<char> = char::decode_utf16(fmt.iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let start = i;
        while i < text.len() && text[i] != '=' && text[i] != ';' {
            i += 1;
        }
        if text.get(i) != Some(&'=') {
            i += 1;
            continue;
        }
        let key: String = text[start..i].iter().collect();
        i += 1;
        while i < text.len() && text[i] != ';' && text[i].is_whitespace() {
            i += 1;
        }
        let value = match text.get(i) {
            Some(&q @ ('\'' | '"')) => {
                i += 1;
                let mut v = String::new();
                while i < text.len() {
                    if text[i] == q {
                        if text.get(i + 1) == Some(&q) {
                            v.push(q);
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    v.push(text[i]);
                    i += 1;
                }
                while i < text.len() && text[i] != ';' {
                    i += 1;
                }
                v
            }
            _ if ends_at_space => {
                let s = i;
                while i < text.len() && text[i] != ';' && !text[i].is_whitespace() {
                    i += 1;
                }
                let v: String = text[s..i].iter().collect();
                while i < text.len() && text[i] != ';' && text[i].is_whitespace() {
                    i += 1;
                }
                if i < text.len() && text[i] != ';' {
                    out.push((key.trim().to_string(), v));
                    return out;
                }
                v
            }
            _ => {
                let s = i;
                while i < text.len() && text[i] != ';' {
                    i += 1;
                }
                text[s..i].iter().collect::<String>().trim().to_string()
            }
        };
        i += 1;
        out.push((key.trim().to_string(), value));
    }
    out
}
