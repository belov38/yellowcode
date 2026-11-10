use std::path::Path;

pub fn failure_written(path: &Path) -> String {
    format!(
        "yellowcode: отчёт о сбое записан: {}; приложите его к сообщению об ошибке (имена в нём заменены хешами; что скрыто и что нет — в docs/bak-failure-report.md)",
        path.display()
    )
}

pub fn trace_written(path: &Path) -> String {
    format!("yellowcode: трасса записана: {}", path.display())
}

pub fn not_written(os_error: Option<i32>, kind: &str) -> String {
    let number = os_error.map_or_else(|| "без номера".to_owned(), |n| n.to_string());
    format!("yellowcode: не удалось записать отчёт о сбое (ошибка ОС: {number}, {kind})")
}
