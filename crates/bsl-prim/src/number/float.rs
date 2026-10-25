use super::Number;

const EXACT_DIGITS: usize = 800;

pub(super) fn from_f64_15(v: f64) -> Option<Number> {
    if !v.is_finite() {
        return None;
    }
    if v == 0.0 {
        return Some(Number::zero());
    }
    let text = format!("{:.*e}", EXACT_DIGITS, v.abs());
    let (mant, exp) = text.split_once('e')?;
    let mut exp: i64 = exp.parse().ok()?;
    let all: Vec<u8> = mant.bytes().filter(u8::is_ascii_digit).collect();
    let mut digits = all.get(..15)?.to_vec();
    if *all.get(15)? >= b'5' {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                digits.pop();
                exp += 1;
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let digits = String::from_utf8(digits).ok()?;
    let point = exp + 1;
    let len = digits.len() as i64;
    let body = if point >= len {
        format!("{digits}{}", "0".repeat((point - len) as usize))
    } else if point > 0 {
        format!(
            "{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    } else {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    };
    Number::parse(&format!("{}{body}", if v < 0.0 { "-" } else { "" }))
}

pub(super) fn to_f64(n: &Number) -> f64 {
    n.to_xml_string().parse::<f64>().unwrap_or(f64::NAN)
}
