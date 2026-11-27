use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const DESKTOP_ENV: &str = "YELLOWCODE_DESKTOP";

#[derive(clap::Args)]
#[command(after_help = "\
Примеры:
  # Открыть конфигурацию
  yellowcode open configuration.cf

  # Открыть файловую базу с английским интерфейсом
  yellowcode open base --lang en

  # Открыть DT, выделив 3 ГиБ на чтение конфигурации
  yellowcode open backup.dt --memory-limit 3GiB")]
pub(crate) struct OpenArgs {
    #[arg(help = "Что открыть: CF, DT, файл 1Cv8.1CD или каталог базы")]
    source: Option<PathBuf>,
    #[arg(long, value_parser = ["ru", "en"], help = "Язык интерфейса")]
    lang: Option<String>,
    #[arg(
        long,
        help = "Готовая выгрузка конфигурации в XML для проверки модулей; без параметра приложение делает её само"
    )]
    names: Option<PathBuf>,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Бюджет памяти на чтение конфигурации из DT; память самой модели расходуется сверх него"
    )]
    memory_limit: Option<String>,
    #[arg(long, help = "Проверочный запуск: открыть источник и выйти")]
    smoke: bool,
}

impl OpenArgs {
    fn window_args(&self, platform_build: Option<&str>) -> Vec<OsString> {
        let mut args = Vec::new();
        if let Some(source) = &self.source {
            args.push(source.into());
        }
        if let Some(lang) = &self.lang {
            args.extend(["--lang".into(), lang.into()]);
        }
        if let Some(names) = &self.names {
            args.extend(["--names".into(), names.into()]);
        }
        if let Some(limit) = &self.memory_limit {
            args.extend(["--memory-limit".into(), limit.into()]);
        }
        if let Some(build) = platform_build {
            args.extend(["--platform-build".into(), build.into()]);
        }
        if self.smoke {
            args.push("--smoke".into());
        }
        args
    }
}

fn window_path() -> std::io::Result<PathBuf> {
    if let Some(path) = std::env::var_os(DESKTOP_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let exe = std::fs::canonicalize(std::env::current_exe()?)?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    Ok(dir.join(format!(
        "yellowcode-desktop{}",
        std::env::consts::EXE_SUFFIX
    )))
}

fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(unc) => format!(r"\\{unc}"),
            None => rest.to_string(),
        },
        None => text,
    }
}

fn without_verbatim_prefix(path: &Path) -> Option<PathBuf> {
    const MAX_PATH_CHARS: usize = 259;
    let rest = path.to_str()?.strip_prefix(r"\\?\")?;
    let bytes = rest.as_bytes();
    let drive = bytes.len() >= 3
        && bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(1..3) == Some(b":\\".as_slice());
    (drive && rest.encode_utf16().count() <= MAX_PATH_CHARS).then(|| PathBuf::from(rest))
}

fn check_memory_limit(limit: Option<&str>, err: &mut dyn Write) -> std::io::Result<Option<i32>> {
    let Some(limit) = limit else {
        return Ok(None);
    };
    match yellowcode_core::viewer::source::OpenMemory::resolve(Some(limit.to_owned())) {
        Ok(_) => Ok(None),
        Err(error) => {
            writeln!(err, "yellowcode: {error}")?;
            Ok(Some(
                if yellowcode_core::service::memory::is_config_error(&*error) {
                    2
                } else {
                    1
                },
            ))
        }
    }
}

pub(crate) fn run(
    args: OpenArgs,
    platform_build: Option<&str>,
    err: &mut dyn Write,
) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(code) = check_memory_limit(args.memory_limit.as_deref(), err)? {
        return Ok(code);
    }
    let window = window_path()?;
    if !window.is_file() {
        writeln!(
            err,
            "yellowcode: окно приложения не найдено: {}; установите Yellowcode с окном или запустите yellowcode-desktop",
            shown(&window)
        )?;
        return Ok(1);
    }
    let program = if cfg!(windows) {
        without_verbatim_prefix(&window).unwrap_or_else(|| window.clone())
    } else {
        window.clone()
    };
    let mut command = Command::new(program);
    command.args(args.window_args(platform_build));
    let start_error = |error: std::io::Error| format!("{}: {error}", shown(&window));
    if args.smoke {
        let status = command.status().map_err(start_error)?;
        return Ok(status.code().unwrap_or(1));
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut command);
    drop(command.spawn().map_err(start_error)?);
    Ok(0)
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn detach(_command: &mut Command) {}
