use std::process::Command;

macro_rules! cases {
    ($(($name:literal, [$($arg:literal),*])),* $(,)?) => {
        &[$(($name, &[$($arg),*], include_str!(concat!("help/", $name, ".txt")))),*]
    };
}

const CASES: &[(&str, &[&str], &str)] = cases![
    ("none", []),
    ("help", ["help"]),
    ("root--help", ["--help"]),
    ("root-h", ["-h"]),
    ("bak--help", ["bak", "--help"]),
    ("bak-inspect--help", ["bak", "inspect", "--help"]),
    ("bak-convert--help", ["bak", "convert", "--help"]),
    ("convert--help", ["convert", "--help"]),
    ("convert-h", ["convert", "-h"]),
    ("open--help", ["open", "--help"]),
    ("open-h", ["open", "-h"]),
    ("query--help", ["query", "--help"]),
    ("query-h", ["query", "-h"]),
    ("serve--help", ["serve", "--help"]),
    ("serve-h", ["serve", "-h"]),
    ("table--help", ["table", "--help"]),
    ("table-h", ["table", "-h"]),
    ("users--help", ["users", "--help"]),
    ("users-list--help", ["users", "list", "--help"]),
    ("users-show--help", ["users", "show", "--help"]),
    ("users-create--help", ["users", "create", "--help"]),
    ("users-edit--help", ["users", "edit", "--help"]),
    ("users-delete--help", ["users", "delete", "--help"]),
    (
        "users-clear-password--help",
        ["users", "clear-password", "--help"]
    ),
    ("metadata--help", ["metadata", "--help"]),
    ("metadata-h", ["metadata", "-h"]),
    ("metadata-tree--help", ["metadata", "tree", "--help"]),
    ("metadata-show--help", ["metadata", "show", "--help"]),
    (
        "metadata-extension-info--help",
        ["metadata", "extension-info", "--help"]
    ),
    (
        "metadata-extension-objects--help",
        ["metadata", "extension-objects", "--help"]
    ),
    (
        "metadata-extension-modules--help",
        ["metadata", "extension-modules", "--help"]
    ),
    (
        "metadata-extension-intercepts--help",
        ["metadata", "extension-intercepts", "--help"]
    ),
    ("module--help", ["module", "--help"]),
    ("module-list--help", ["module", "list", "--help"]),
    ("module-show--help", ["module", "show", "--help"]),
    ("module-outline--help", ["module", "outline", "--help"]),
    ("template--help", ["template", "--help"]),
    ("template-list--help", ["template", "list", "--help"]),
    ("template-show--help", ["template", "show", "--help"]),
    ("grep--help", ["grep", "--help"]),
    ("export--help", ["export", "--help"]),
    ("help-query", ["help", "query"]),
    ("err-query-no-source", ["query", "base"]),
    (
        "err-query-two-sources",
        ["query", "base", "--text", "SELECT 1", "--stdin"]
    ),
    (
        "err-query-timeout",
        ["query", "base", "--text", "SELECT 1", "--timeout", "0"]
    ),
    (
        "err-query-timeout-text",
        ["query", "base", "--text", "SELECT 1", "--timeout", "soon"]
    ),
    (
        "err-query-max-rows",
        ["query", "base", "--text", "SELECT 1", "--max-rows", "0"]
    ),
    (
        "err-table-limit",
        ["table", "base", "Document", "Doc", "--limit", "0"]
    ),
    (
        "err-table-list-indexes-cursor",
        [
            "table",
            "base",
            "Document",
            "Doc",
            "--list-indexes",
            "--cursor",
            "c"
        ]
    ),
    (
        "err-table-eq-without-index",
        ["table", "base", "Document", "Doc", "--eq", "1"]
    ),
    (
        "err-table-order-without-index",
        ["table", "base", "Document", "Doc", "--order", "desc"]
    ),
    (
        "err-table-gt-gte",
        [
            "table", "base", "Document", "Doc", "--index", "I", "--gt", "1", "--gte", "2"
        ]
    ),
    (
        "err-table-lt-lte",
        [
            "table", "base", "Document", "Doc", "--index", "I", "--lt", "1", "--lte", "2"
        ]
    ),
    (
        "err-table-posted",
        ["table", "base", "Document", "Doc", "--posted", "maybe"]
    ),
    (
        "err-table-deletion-mark",
        [
            "table",
            "base",
            "Document",
            "Doc",
            "--deletion-mark",
            "TRUE"
        ]
    ),
    (
        "err-convert-no-output",
        ["convert", "source.cf", "--to", "dt"]
    ),
    (
        "err-convert-format",
        ["convert", "source.cf", "--to", "zip", "--output", "out"]
    ),
    (
        "err-bak-convert-raw-dt",
        [
            "bak",
            "convert",
            "base.bak",
            "--server",
            "localhost",
            "--to",
            "dt",
            "--output",
            "base.dt",
            "--raw"
        ]
    ),
    (
        "err-bak-convert-raw-1cd",
        [
            "bak",
            "convert",
            "base.bak",
            "--server",
            "localhost",
            "--to",
            "1cd",
            "--output",
            "base.1CD",
            "--raw"
        ]
    ),
    (
        "err-bak-convert-direct-trace",
        [
            "bak",
            "convert",
            "base.bak",
            "--to",
            "cf",
            "--output",
            "base.cf",
            "--trace",
            "trace.log"
        ]
    ),
    (
        "err-bak-convert-strict-server",
        [
            "bak",
            "convert",
            "base.bak",
            "--server",
            "localhost",
            "--to",
            "cf",
            "--output",
            "base.cf",
            "--strict"
        ]
    ),
    (
        "err-bak-convert-ignore-extra-tables-cf",
        [
            "bak",
            "convert",
            "base.bak",
            "--server",
            "localhost",
            "--to",
            "cf",
            "--output",
            "base.cf",
            "--ignore-extra-tables"
        ]
    ),
    (
        "err-bak-convert-ignore-extra-tables-direct",
        [
            "bak",
            "convert",
            "base.bak",
            "--to",
            "cf",
            "--output",
            "base.cf",
            "--ignore-extra-tables"
        ]
    ),
    (
        "err-bak-convert-direct-sql-options",
        [
            "bak",
            "convert",
            "base.bak",
            "--to",
            "dt",
            "--output",
            "base.dt",
            "--backup-set",
            "2",
            "--user",
            "sa",
            "--sqlcmd",
            "sqlcmd",
            "--sqlcmd-lifecycle-timeout",
            "60",
            "--trust-server-certificate",
            "--memory-limit",
            "auto"
        ]
    ),
    (
        "err-metadata-format",
        ["metadata", "tree", "source.cf", "--format", "xml"]
    ),
    (
        "err-module-lines",
        [
            "module",
            "show",
            "source.cf",
            "Module.Path",
            "--lines",
            "5:1"
        ]
    ),
    (
        "err-grep-conflict",
        ["grep", "x", "source.cf", "--files-with-matches", "--count"]
    ),
    ("err-export-no-output", ["export", "source.cf"]),
    (
        "err-template-query-without-set",
        ["template", "show", "source.cf", "Макет", "--query"]
    ),
    ("remote--help", ["remote", "--help"]),
    ("remote-info--help", ["remote", "info", "--help"]),
    ("remote-metadata--help", ["remote", "metadata", "--help"]),
    ("remote-call--help", ["remote", "call", "--help"]),
    ("remote-modules--help", ["remote", "modules", "--help"]),
    ("remote-object--help", ["remote", "object", "--help"]),
    (
        "err-remote-object-no-ref",
        ["remote", "object", "http://127.0.0.1:1"]
    ),
    (
        "err-remote-object-bad-ref",
        [
            "remote",
            "object",
            "http://127.0.0.1:1",
            "--ref",
            "Справочник.Товары"
        ]
    ),
    ("remote-list--help", ["remote", "list", "--help"]),
    (
        "err-remote-list-page-and-all",
        [
            "remote",
            "list",
            "http://127.0.0.1:1",
            "--object",
            "A.B",
            "--page",
            "2",
            "--all"
        ]
    ),
    ("remote-report--help", ["remote", "report", "--help"]),
    ("remote-create--help", ["remote", "create", "--help"]),
    ("remote-write--help", ["remote", "write", "--help"]),
    (
        "err-remote-create-no-object",
        [
            "remote",
            "create",
            "http://127.0.0.1:1",
            "--set",
            "Наименование=x"
        ]
    ),
    (
        "err-remote-write-bad-set",
        [
            "remote",
            "write",
            "http://127.0.0.1:1",
            "--ref",
            "Справочник.Товары(a3da0ff9-e955-11f1-a6fc-00090faa0001)",
            "--set",
            "Товары[0].Цена=1"
        ]
    ),
    (
        "err-remote-write-post-and-unpost",
        [
            "remote",
            "write",
            "http://127.0.0.1:1",
            "--ref",
            "Документ.Счет(a3da0ff9-e955-11f1-a6fc-00090faa0001)",
            "--post",
            "--unpost"
        ]
    ),
    (
        "err-remote-call-no-call",
        ["remote", "call", "http://127.0.0.1:1"]
    ),
    (
        "err-remote-report-csv-no-call",
        [
            "remote",
            "report",
            "http://127.0.0.1:1",
            "--form",
            "Отчет.Х.Форма.Ф",
            "--format",
            "csv"
        ]
    ),
    (
        "err-remote-report-attribute-no-call",
        [
            "remote",
            "report",
            "http://127.0.0.1:1",
            "--form",
            "Отчет.Х.Форма.Ф",
            "--attribute",
            "Р"
        ]
    ),
    (
        "err-remote-report-no-form",
        ["remote", "report", "http://127.0.0.1:1"]
    ),
    (
        "err-remote-report-form-and-report",
        [
            "remote",
            "report",
            "http://127.0.0.1:1",
            "--form",
            "Отчет.Х.Форма.Ф",
            "--report",
            "Отчет.Х"
        ]
    ),
    (
        "err-remote-report-report-and-call",
        [
            "remote",
            "report",
            "http://127.0.0.1:1",
            "--report",
            "Отчет.Х",
            "--call",
            "П"
        ]
    ),
    (
        "err-remote-report-generate-and-call",
        [
            "remote",
            "report",
            "http://127.0.0.1:1",
            "--form",
            "Отчет.Х.Форма.Ф",
            "--generate",
            "--call",
            "П"
        ]
    ),
    (
        "err-remote-report-bad-set",
        [
            "remote",
            "report",
            "http://127.0.0.1:1",
            "--report",
            "Отчет.Х",
            "--set",
            "Период"
        ]
    ),
    (
        "err-remote-call-param-conflict",
        [
            "remote",
            "call",
            "http://127.0.0.1:1",
            "--call",
            "М.Ф",
            "--param",
            "x",
            "--param-json",
            "null"
        ]
    ),
    (
        "err-remote-call-bad-param",
        [
            "remote",
            "call",
            "http://127.0.0.1:1",
            "--call",
            "М.Ф",
            "--param",
            "b:yes"
        ]
    ),
    (
        "err-remote-call-bare-number",
        [
            "remote",
            "call",
            "http://127.0.0.1:1",
            "--call",
            "М.Ф",
            "--param-json",
            "1.5"
        ]
    ),
    (
        "err-remote-info-auth-settings-only-user",
        [
            "remote",
            "info",
            "http://127.0.0.1:1",
            "--auth-settings-only",
            "--user",
            "u"
        ]
    ),
    (
        "err-remote-password",
        [
            "remote",
            "info",
            "http://127.0.0.1:1",
            "--user",
            "u",
            "--password",
            "p"
        ]
    ),
    (
        "err-remote-password-env-without-user",
        [
            "remote",
            "metadata",
            "http://127.0.0.1:1",
            "--password-env",
            "P"
        ]
    ),
    (
        "err-remote-strong-password-without-user",
        ["remote", "info", "http://127.0.0.1:1", "--strong-password"]
    ),
    (
        "err-remote-server-version-bad",
        [
            "remote",
            "info",
            "http://127.0.0.1:1",
            "--server-version",
            "8.3.x"
        ]
    ),
    (
        "err-remote-server-version-auth-settings-only",
        [
            "remote",
            "info",
            "http://127.0.0.1:1",
            "--auth-settings-only",
            "--server-version",
            "8.3.27.2214"
        ]
    ),
    (
        "err-remote-modules-id-without-out",
        [
            "remote",
            "modules",
            "http://127.0.0.1:1",
            "--id",
            "urn:module:md:6a1c0de0-0000-4a00-8000-00000000c0f1@property='d22e852a-cf8a-4f77-8ccb-3548e7792bea';version='cfc541901463f141846df230f689362900000000'"
        ]
    ),
    (
        "err-remote-modules-bad-id",
        [
            "remote",
            "modules",
            "http://127.0.0.1:1",
            "--out",
            "d",
            "--id",
            "urn:form:md:0:x"
        ]
    ),
    ("err-unknown", ["bogus"]),
];

fn run(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_yellowcode"))
        .args(args)
        .env("COLUMNS", "100")
        .env_remove("YELLOWCODE_TRACE")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    let text = format!(
        "exit {}\n--- stdout\n{}--- stderr\n{}",
        out.status.code().map_or("signal".into(), |c| c.to_string()),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if cfg!(windows) {
        text.replace("yellowcode.exe", "yellowcode")
    } else {
        text
    }
}

fn subcommands(help: &str) -> Vec<String> {
    let Some((_, block)) = help.split_once("\nCommands:\n") else {
        return Vec::new();
    };
    block
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.strip_prefix("  "))
        .filter(|l| !l.starts_with(' '))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_command_has_a_help_snapshot() {
    let mut missing = Vec::new();
    let mut queue: Vec<Vec<String>> = vec![Vec::new()];
    while let Some(path) = queue.pop() {
        let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
        args.push("--help");
        let help = run(&args);
        assert!(
            help.starts_with("exit 0\n"),
            "yellowcode {}:\n{help}",
            args.join(" ")
        );
        for sub in subcommands(&help) {
            let mut next = path.clone();
            next.push(sub);
            let name = format!("{}--help", next.join("-"));
            if !CASES.iter().any(|(case, _, _)| *case == name) {
                let args: Vec<String> = next
                    .iter()
                    .chain([&"--help".to_owned()])
                    .map(|a| format!("{a:?}"))
                    .collect();
                missing.push(format!("({name:?}, [{}]),", args.join(", ")));
            }
            queue.push(next);
        }
    }
    assert!(
        missing.is_empty(),
        "commands without a help snapshot: create an empty tests/help/<name>.txt, add the line to CASES and \
         fill the file with HELP_SNAPSHOTS_WRITE=<dir>:\n{}",
        missing.join("\n")
    );
}

#[test]
fn help_and_usage_errors_match_the_snapshots() {
    if let Some(dir) = std::env::var_os("HELP_SNAPSHOTS_WRITE") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, args, _) in CASES {
            std::fs::write(dir.join(format!("{name}.txt")), run(args)).unwrap();
        }
        return;
    }
    let mut differ = Vec::new();
    for (name, args, want) in CASES {
        if args.is_empty() && env!("CARGO_PKG_NAME") == "onec-desktop" {
            continue;
        }
        let got = run(args);
        if got != *want {
            differ.push(format!("{name}:\n--- want\n{want}\n--- got\n{got}"));
        }
    }
    assert!(
        differ.is_empty(),
        "{} of {} differ:\n{}",
        differ.len(),
        CASES.len(),
        differ.join("\n")
    );
}
