use std::io::Read;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use ui_style::skin::{MAX_PACKAGE, Skin};

const RELEASE: &str = "https://github.com/belov38/yellowcode-skins/releases/latest/download/";

fn fetch(client: &reqwest::blocking::Client, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("skin download: {e}"))?
        .error_for_status()
        .map_err(|e| format!("skin download: {e}"))?;
    if response.content_length().is_some_and(|len| len > limit) {
        return Err("skin download exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("skin download exceeds size limit".into());
    }
    Ok(bytes)
}

fn download(client: &reqwest::blocking::Client, base: &str) -> Result<Skin, String> {
    let sums = fetch(client, &format!("{base}SHA256SUMS"), 64 * 1024)?;
    let sums = std::str::from_utf8(&sums).map_err(|_| "SHA256SUMS is not UTF-8")?;
    let mut expected = None;
    for line in sums.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() == 2 && fields[1].trim_start_matches('*') == "1c-classic.zip" {
            if expected.is_some()
                || fields[0].len() != 64
                || !fields[0].bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("invalid or duplicate SHA256SUMS entry".into());
            }
            expected = Some(fields[0].to_ascii_lowercase());
        }
    }
    let expected = expected.ok_or("SHA256SUMS has no 1c-classic.zip entry")?;
    let bytes = fetch(client, &format!("{base}1c-classic.zip"), MAX_PACKAGE)?;
    if format!("{:x}", Sha256::digest(&bytes)) != expected {
        return Err("1c-classic.zip SHA-256 mismatch".into());
    }
    let skin = Skin::from_zip(&bytes)?;
    if skin.manifest.id != "1c-classic" {
        return Err("downloaded skin id must be 1c-classic".into());
    }
    Ok(skin)
}

fn root(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    super::skin_root(app).map_err(|e| e.to_string())
}

fn install(app: &AppHandle, skin: Skin) -> Result<serde_json::Value, String> {
    let st = app.state::<super::AppState>();
    let mut skins = super::lock(&st.skins);
    skin.install(&root(app)?)?;
    let id = skin.manifest.id.clone();
    skins.add(skin);
    skins.select(&id)?;
    Ok(skins.list())
}

#[tauri::command]
pub async fn install_skin(app: AppHandle, path: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || install(&app, Skin::load(Path::new(&path))?))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn download_classic_skin(app: AppHandle) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;
        install(&app, download(&client, RELEASE)?)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn pick_skin(app: AppHandle, folder: Option<bool>) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let dialog = app.dialog().file();
    let picked = if folder.unwrap_or(false) {
        dialog.blocking_pick_folder()
    } else {
        dialog
            .add_filter("Skin package", &["zip"])
            .blocking_pick_file()
    };
    picked?.into_path().ok()?.to_str().map(str::to_string)
}

#[tauri::command]
pub fn select_skin(app: AppHandle, id: String) -> Result<serde_json::Value, String> {
    let st = app.state::<super::AppState>();
    let mut skins = super::lock(&st.skins);
    skins.select(&id)?;
    Ok(skins.list())
}

#[tauri::command]
pub fn list_skins(app: AppHandle) -> serde_json::Value {
    let st = app.state::<super::AppState>();
    super::lock(&st.skins).list()
}
