use crate::model::Value;
use brace::Node;
use mdschema::Ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutEncodeError {
    WrongValue,
    InvalidPrefix,
    InvalidText,
    UnknownKey,
    AmbiguousKey,
}
type Result<T> = std::result::Result<T, ShortcutEncodeError>;

pub(crate) fn encode_shortcut(ui: &Ui, value: &Value, prefix: &Node) -> Result<Node> {
    let Value::Text(text) = value else {
        return Err(ShortcutEncodeError::WrongValue);
    };
    if matches!(prefix, Node::List(_)) {
        return Err(ShortcutEncodeError::InvalidPrefix);
    }
    if text.chars().any(char::is_whitespace) {
        return Err(ShortcutEncodeError::InvalidText);
    }
    let (key, mods) = if text.is_empty() {
        (0, 0)
    } else {
        let mut tokens = text.split('+').peekable();
        let mut order = 0;
        let mut mods = 0;
        let mut token_count = 0;
        let key = loop {
            let token = tokens.next().ok_or(ShortcutEncodeError::InvalidText)?;
            token_count += 1;
            if token.is_empty() || token_count > 4 {
                return Err(ShortcutEncodeError::InvalidText);
            }
            if tokens.peek().is_none() {
                break key_code(ui, token)?;
            }
            let (rank, bit) = match token {
                "Ctrl" => (1, 8),
                "Alt" => (2, 16),
                "Shift" => (3, 4),
                _ => return Err(ShortcutEncodeError::InvalidText),
            };
            if rank <= order {
                return Err(ShortcutEncodeError::InvalidText);
            }
            order = rank;
            mods |= bit;
        };
        (key, mods)
    };
    Ok(Node::list(vec![
        prefix.clone(),
        Node::int(key),
        Node::int(mods),
    ]))
}

fn builtin(code: i64) -> bool {
    code == 0
        || (112..=135).contains(&code)
        || (48..=57).contains(&code)
        || (65..=90).contains(&code)
}

fn key_code(ui: &Ui, token: &str) -> Result<i64> {
    if token.len() == 1
        && let Some(byte) = token
            .bytes()
            .next()
            .filter(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
    {
        return Ok(i64::from(byte));
    }
    if let Some(digits) = token
        .strip_prefix('F')
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        let number = digits
            .parse::<u8>()
            .map_err(|_| ShortcutEncodeError::InvalidText)?;
        if !(1..=24).contains(&number) || digits.starts_with('0') {
            return Err(ShortcutEncodeError::InvalidText);
        }
        return Ok(111 + i64::from(number));
    }
    let mut found = None;
    for (code, name) in &ui.keys {
        if name != token {
            continue;
        }
        let Ok(number) = code.parse::<i64>() else {
            continue;
        };
        if builtin(number) || number.to_string() != *code {
            continue;
        }
        if found.replace(number).is_some() {
            return Err(ShortcutEncodeError::AmbiguousKey);
        }
    }
    found.ok_or(ShortcutEncodeError::UnknownKey)
}
