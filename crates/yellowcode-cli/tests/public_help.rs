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
        "err-metadata-format",
        ["metadata", "tree", "source.cf", "--format", "xml"]
    ),
    ("err-unknown", ["bogus"]),
];

fn run(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_yellowcode"))
        .args(args)
        .env("COLUMNS", "100")
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
