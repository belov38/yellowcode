use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Instant,
};
use tauri::AppHandle;
use yellowcode_core::{
    plain::convert::{self, Format, Layout, Request},
    viewer::source::{self, SourceKind},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    source: PathBuf,
    output: PathBuf,
    to: String,
    #[serde(default)]
    without_users: bool,
    memory_limit: Option<String>,
    users_layout: Option<String>,
}

impl Input {
    fn request(self) -> Result<Request, String> {
        Ok(Request {
            source: self.source,
            output: self.output,
            to: match self.to.as_str() {
                "cf" => Format::Cf,
                "dt" => Format::Dt,
                "1cd" => Format::Infobase,
                _ => return Err("unsupported output format".into()),
            },
            users_layout: match self.users_layout.as_deref() {
                None => None,
                Some("users8") => Some(Layout::Users8),
                Some("users-spr11") => Some(Layout::UsersSpr11),
                Some(_) => return Err("unsupported users layout".into()),
            },
            memory_limit: self.memory_limit,
            without_users: self.without_users,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub path: PathBuf,
    pub kind: &'static str,
    pub targets: &'static [&'static str],
    pub platform_build: &'static str,
}

pub fn source_info(path: &Path) -> Result<SourceInfo, String> {
    let path = source::resolve(path);
    let (kind, targets): (_, &'static [&'static str]) =
        match source::detect(&path).map_err(|e| e.to_string())? {
            SourceKind::Dt => ("dt", &["1cd", "cf"]),
            SourceKind::Infobase => ("1cd", &["dt", "cf"]),
            SourceKind::Cf => ("cf", &[]),
            SourceKind::XmlDir => ("xml", &[]),
        };
    Ok(SourceInfo {
        path,
        kind,
        targets,
        platform_build: yellowcode_core::platform::active_build(),
    })
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub state: String,
    pub source: PathBuf,
    pub output: PathBuf,
    pub to: String,
    pub targets: &'static [&'static str],
    pub without_users: bool,
    pub memory_limit: Option<String>,
    pub users_layout: Option<String>,
    pub platform_build: &'static str,
    pub elapsed_ms: u64,
    pub progress: Option<String>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Job {
    snapshot: Snapshot,
    since: Option<Instant>,
}

#[derive(Clone, Default)]
pub struct Jobs(Arc<Mutex<Job>>);

impl Jobs {
    fn claim(&self, input: &Input) -> Result<(), String> {
        let mut job = super::lock(&self.0);
        if job.snapshot.state == "running" {
            return Err("another conversion is running".into());
        }
        *job = Job {
            snapshot: Snapshot {
                state: "running".into(),
                source: input.source.clone(),
                output: input.output.clone(),
                to: input.to.clone(),
                without_users: input.without_users,
                memory_limit: input.memory_limit.clone(),
                users_layout: input.users_layout.clone(),
                ..Default::default()
            },
            since: Some(Instant::now()),
        };
        Ok(())
    }

    pub fn status(&self) -> Snapshot {
        let job = super::lock(&self.0);
        let mut snapshot = job.snapshot.clone();
        snapshot.platform_build = yellowcode_core::platform::active_build();
        if snapshot.state.is_empty() {
            snapshot.state = "idle".into();
        }
        if let Some(since) = job.since {
            snapshot.elapsed_ms = u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX);
        }
        snapshot
    }

    pub fn running(&self) -> bool {
        super::lock(&self.0).snapshot.state == "running"
    }

    fn finish(&self, result: Result<serde_json::Value, String>) {
        let mut job = super::lock(&self.0);
        job.snapshot.elapsed_ms = job.since.take().map_or(0, |since| {
            u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        match result {
            Ok(value) => {
                job.snapshot.state = "done".into();
                job.snapshot.result = Some(value);
            }
            Err(error) => {
                job.snapshot.state = "error".into();
                job.snapshot.error = Some(error);
            }
        }
    }

    pub fn start(&self, input: Input) -> Result<JoinHandle<()>, String> {
        self.claim(&input)?;
        let jobs = self.clone();
        match std::thread::Builder::new()
            .name("conversion".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let request = input.request()?;
                    let info = source_info(&request.source)?;
                    super::lock(&jobs.0).snapshot.targets = info.targets;
                    let mut output = Vec::new();
                    let mut progress = Progress {
                        jobs: jobs.clone(),
                        line: Vec::new(),
                    };
                    convert::run(request, &mut output, &mut progress).map_err(|e| e.to_string())?;
                    serde_json::from_slice(&output).map_err(|e| e.to_string())
                }))
                .unwrap_or_else(|_| Err("conversion failed unexpectedly".into()));
                jobs.finish(result);
            }) {
            Ok(handle) => Ok(handle),
            Err(error) => {
                let message = error.to_string();
                self.finish(Err(message.clone()));
                Err(message)
            }
        }
    }
}

struct Progress {
    jobs: Jobs,
    line: Vec<u8>,
}
impl Write for Progress {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for byte in bytes {
            if *byte == b'\n' {
                if !self.line.is_empty() {
                    super::lock(&self.jobs.0).snapshot.progress =
                        Some(String::from_utf8_lossy(&self.line).into_owned());
                    self.line.clear();
                }
            } else if self.line.len() < 4096 {
                self.line.push(*byte);
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tauri::command(async)]
pub fn convert_source_info(
    state: tauri::State<'_, super::AppState>,
    path: Option<String>,
) -> Result<SourceInfo, String> {
    let path = path
        .map(PathBuf::from)
        .or_else(|| {
            state
                .current
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .and_then(|viewer| {
                    viewer
                        .with(
                            yellowcode_core::viewer::Need::Stages(
                                yellowcode_core::viewer::Viewer::OPEN,
                            ),
                            std::time::Duration::ZERO,
                        )
                        .ok()
                        .map(|snapshot| snapshot.source.clone())
                })
        })
        .ok_or("choose a source file")?;
    source_info(&path)
}

#[tauri::command]
pub fn conversion_status(jobs: tauri::State<'_, Jobs>) -> Snapshot {
    jobs.status()
}

#[tauri::command]
pub fn start_conversion(jobs: tauri::State<'_, Jobs>, input: Input) -> Result<(), String> {
    jobs.start(input).map(|_| ())
}

#[tauri::command(async)]
pub fn pick_conversion_output(app: AppHandle, to: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (extension, name) = match to.as_str() {
        "1cd" => ("1CD", "1Cv8.1CD"),
        "dt" => ("dt", "export.dt"),
        "cf" => ("cf", "configuration.cf"),
        _ => return Err("unsupported output format".into()),
    };
    app.dialog()
        .file()
        .set_file_name(name)
        .add_filter(extension, &[extension])
        .blocking_save_file()
        .map(|file| {
            file.into_path().map_err(|e| e.to_string()).and_then(|p| {
                p.into_os_string()
                    .into_string()
                    .map_err(|_| "path is not Unicode".into())
            })
        })
        .transpose()
}
