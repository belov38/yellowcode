use std::fmt::Write as _;

pub struct EnvVar {
    pub name: &'static str,
    pub about: &'static str,
}

pub const PUBLIC: &[EnvVar] = &[
    EnvVar {
        name: "YELLOWCODE_READONLY",
        about: "1 или true: отказ с кодом 4 командам, которые меняют данные (ib\u{a0}users\u{a0}create, edit, \
                delete, clear-password, bak\u{a0}convert\u{a0}--server, remote\u{a0}call, \
                remote\u{a0}create и remote\u{a0}write без\u{a0}--dry-run, remote\u{a0}form\u{a0}--command/--call, \
                remote\u{a0}print\u{a0}--init-call, remote\u{a0}report\u{a0}--call/--init-call/--before/--variant); \
                защита от ошибок, а не граница безопасности: переменную можно снять",
    },
    EnvVar {
        name: "YELLOWCODE_PROGRESS",
        about: "ход работы в stderr без --progress (config, ext, convert): auto, always или never",
    },
    EnvVar {
        name: "YELLOWCODE_MEMORY_LIMIT",
        about: "бюджет памяти без --memory-limit: ib\u{a0}query, ib\u{a0}table, ib\u{a0}serve, convert\u{a0}в\u{a0}1CD, \
                bak\u{a0}convert\u{a0}--to\u{a0}1cd",
    },
    EnvVar {
        name: "YELLOWCODE_CACHE_LIMIT",
        about: "предел кэша страниц без --cache-limit (ib\u{a0}query, ib\u{a0}table, ib\u{a0}serve)",
    },
    EnvVar {
        name: "YELLOWCODE_QUERY_MEMORY_LIMIT",
        about: "предел памяти запроса без --query-memory-limit (ib\u{a0}query, ib\u{a0}table, ib\u{a0}serve)",
    },
    EnvVar {
        name: "YELLOWCODE_RESULT_MEMORY_LIMIT",
        about: "предел памяти результатов без --result-memory-limit (ib\u{a0}query, ib\u{a0}table, ib\u{a0}serve)",
    },
    EnvVar {
        name: "YELLOWCODE_RESPONSE_LIMIT",
        about: "предел размера ответа без --response-limit (ib\u{a0}query, ib\u{a0}table, ib\u{a0}serve)",
    },
    EnvVar {
        name: "YELLOWCODE_CONTAINER_MEMORY_BYTES",
        about: "память контейнера в байтах; ограничивает бюджет памяти auto (ib\u{a0}query, ib\u{a0}table, ib\u{a0}serve, \
                convert\u{a0}в\u{a0}1CD, bak\u{a0}convert\u{a0}--to\u{a0}1cd)",
    },
    EnvVar {
        name: "YELLOWCODE_TRACE",
        about: "новый файл трассы bak\u{a0}convert\u{a0}--server без --trace",
    },
    EnvVar {
        name: "YELLOWCODE_PLATFORM_RESOURCES",
        about: "каталог ресурсов платформы для --platform-build, отличной от встроенной (все команды); по \
                умолчанию каталог рядом с yellowcode",
    },
    EnvVar {
        name: "YELLOWCODE_DESKTOP",
        about: "исполняемый файл окна для open; по умолчанию yellowcode-desktop рядом с yellowcode",
    },
];

pub const INTERNAL: &[&str] = &[
    "YELLOWCODE_VERSION",
    "YELLOWCODE_EXTERNAL_INTERRUPT_CHILD",
    "YELLOWCODE_MEMORY_PROFILE",
];

const COLUMNS: usize = 115;

pub(crate) fn help_section() -> String {
    let width = PUBLIC.iter().map(|v| v.name.len()).max().unwrap_or(0);
    let values: Vec<String> = PUBLIC
        .iter()
        .map(|v| shown_value(std::env::var_os(v.name).as_deref()))
        .collect();
    let value_width = values
        .iter()
        .map(|v| v.chars().count())
        .filter(|&n| n <= 16)
        .max()
        .unwrap_or(0);
    let mut text = String::from("Переменные окружения (имя, текущее значение, назначение):");
    let indent = 2 + width + 2 + value_width + 2 + "— ".chars().count();
    for (var, value) in PUBLIC.iter().zip(&values) {
        let pad = value_width.saturating_sub(value.chars().count());
        let mut line = format!(
            "  {name:<width$}  {value}{blank:pad$}  —",
            name = var.name,
            blank = "",
        );
        let mut used = line.chars().count();
        let mut first = true;
        for word in var.about.split(' ') {
            let len = word.chars().count();
            if !first && used + 1 + len > COLUMNS {
                let _ = write!(text, "\n{line}");
                line = " ".repeat(indent - 1);
                used = indent - 1;
            }
            line.push(' ');
            line.push_str(&word.replace('\u{a0}', " "));
            used += 1 + len;
            first = false;
        }
        let _ = write!(text, "\n{line}");
    }
    text
}

fn shown_value(value: Option<&std::ffi::OsStr>) -> String {
    match value {
        None => "не задана".to_owned(),
        Some(value) if value.is_empty() => "пустая".to_owned(),
        Some(value) => {
            let mut shown = String::new();
            for c in value.to_string_lossy().chars() {
                if c.is_control() {
                    shown.extend(c.escape_default());
                } else {
                    shown.push(c);
                }
            }
            shown
        }
    }
}
