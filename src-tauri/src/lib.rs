use anyhow::{anyhow, Context};
use chrono::{DateTime, Local};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::Arc,
};
use tar::Archive;
use tauri::{AppHandle, Manager, State};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
};
use zip::ZipArchive;

const DEFAULT_VERSION: &str = "v0.67.0";
const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/fatedier/frp/releases/latest";

#[derive(Debug, Error)]
enum AppError {
    #[error("{0}")]
    Message(String),
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(error: anyhow::Error) -> Self {
        Self::Message(error.to_string())
    }
}

type AppResult<T> = Result<T, AppError>;

#[derive(Default)]
struct ProcessState {
    child: Mutex<Option<Child>>,
    logs: Mutex<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
struct PathInfo {
    app_data_dir: String,
    workspace_dir: String,
    config_path: String,
    runtimes_dir: String,
    current_binary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeInfo {
    id: String,
    version: String,
    platform: String,
    imported_at: String,
    archive_name: String,
    binary_path: String,
    is_current: bool,
    can_delete: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigSummary {
    server_addr: String,
    server_port: Option<u16>,
    auth_method: String,
    auth_token: String,
    proxies: Vec<ProxySummary>,
}

#[derive(Debug, Clone, Serialize)]
struct ProxySummary {
    name: String,
    proxy_type: String,
    local_ip: String,
    local_port: Option<u16>,
    remote_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
struct ServiceStatus {
    running: bool,
    pid: Option<u32>,
    logs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DashboardState {
    paths: PathInfo,
    current_platform: String,
    config_text: String,
    config_summary: ConfigSummary,
    runtimes: Vec<RuntimeInfo>,
    current_runtime: Option<RuntimeInfo>,
    service: ServiceStatus,
    latest_release: Option<ReleaseInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AppSettings {
    current_runtime_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ReleaseInfo {
    version: String,
    html_url: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, Serialize)]
struct ReleaseAsset {
    name: String,
    download_url: String,
    platform: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct ImportRuntimeRequest {
    archive_path: String,
}

#[derive(Debug, Deserialize)]
struct SaveConfigRequest {
    config_text: String,
}

#[derive(Debug, Deserialize)]
struct SaveQuickConfigRequest {
    server_addr: String,
    server_port: u16,
    auth_method: String,
    auth_token: String,
    proxies: Vec<QuickProxyRequest>,
}

#[derive(Debug, Deserialize)]
struct QuickProxyRequest {
    name: String,
    proxy_type: String,
    local_ip: String,
    local_port: u16,
    remote_port: u16,
}

#[derive(Debug, Deserialize)]
struct SetCurrentRuntimeRequest {
    id: String,
}

#[derive(Debug, Deserialize)]
struct DownloadRuntimeRequest {
    download_url: String,
    archive_name: String,
}

#[derive(Debug, Clone)]
struct RuntimeCandidate {
    version: String,
    platform: String,
    archive_name: String,
    binary_bytes: Vec<u8>,
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .manage(Arc::new(ProcessState::default()))
        .invoke_handler(tauri::generate_handler![
            get_dashboard_state,
            reload_config,
            save_config,
            save_quick_config,
            reset_config,
            start_frpc,
            stop_frpc,
            get_service_status,
            import_runtime,
            download_and_import_runtime,
            set_current_runtime,
            delete_runtime,
            check_latest_release,
            open_path
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            apply_macos_app_icon().map_err(|error| error.to_string())?;
            ensure_workspace(app.handle()).map_err(|error| error.to_string())?;
            ensure_seed_runtime(app.handle()).map_err(|error| error.to_string())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(target_os = "macos")]
fn apply_macos_app_icon() -> anyhow::Result<()> {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else {
        return Ok(());
    };

    let app = NSApplication::sharedApplication(mtm);
    let data = NSData::with_bytes(include_bytes!("../icons/icon.icns"));
    let app_icon = NSImage::initWithData(NSImage::alloc(), &data)
        .context("failed to create macOS app icon")?;

    unsafe {
        app.setApplicationIconImage(Some(&app_icon));
    }

    Ok(())
}

#[tauri::command]
async fn get_dashboard_state(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
) -> AppResult<DashboardState> {
    ensure_workspace(&app)?;
    ensure_seed_runtime(&app)?;

    let config_text = read_config_text(&app)?;
    let config_summary = parse_config_summary(&config_text);
    let runtimes = list_runtimes(&app)?;
    let settings = read_settings(&app)?;
    let current_runtime = select_current_runtime(&runtimes, &settings);
    let service = service_status(&state).await;

    Ok(DashboardState {
        paths: path_info(&app, current_runtime.as_ref())?,
        current_platform: current_platform(),
        config_text,
        config_summary,
        runtimes,
        current_runtime,
        service,
        latest_release: None,
    })
}

#[tauri::command]
async fn reload_config(app: AppHandle) -> AppResult<String> {
    read_config_text(&app).map_err(AppError::from)
}

#[tauri::command]
async fn save_config(app: AppHandle, request: SaveConfigRequest) -> AppResult<ConfigSummary> {
    validate_config_text(&request.config_text)?;
    fs::write(config_path(&app)?, request.config_text.as_bytes())
        .context("写入 frpc.toml 失败")?;
    Ok(parse_config_summary(&request.config_text))
}

#[tauri::command]
async fn save_quick_config(
    app: AppHandle,
    request: SaveQuickConfigRequest,
) -> AppResult<(String, ConfigSummary)> {
    let config_text = render_quick_config(&request);
    validate_config_text(&config_text)?;
    fs::write(config_path(&app)?, config_text.as_bytes()).context("写入快捷配置失败")?;
    let summary = parse_config_summary(&config_text);
    Ok((config_text, summary))
}

#[tauri::command]
async fn reset_config(app: AppHandle) -> AppResult<String> {
    let defaults = default_config_text(&app)?;
    fs::write(config_path(&app)?, defaults.as_bytes()).context("恢复默认配置失败")?;
    Ok(defaults)
}

#[tauri::command]
async fn start_frpc(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
) -> AppResult<ServiceStatus> {
    if service_status(&state).await.running {
        return Ok(service_status(&state).await);
    }

    let runtime = current_runtime(&app)?;
    let binary = PathBuf::from(&runtime.binary_path);
    if !binary.exists() {
        return Err(anyhow!("当前 frpc 二进制不存在：{}", binary.display()).into());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&binary)
            .context("读取 frpc 二进制权限失败")?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).context("设置 frpc 可执行权限失败")?;
    }

    let config = config_path(&app)?;
    let mut command = Command::new(&binary);
    command
        .arg("-c")
        .arg(&config)
        .current_dir(workspace_dir(&app)?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().context("启动 frpc 失败")?;

    state.logs.lock().await.clear();

    if let Some(stdout) = child.stdout.take() {
        let log_state = Arc::clone(&state);
        tokio::spawn(async move {
            collect_logs(stdout, log_state).await;
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let log_state = Arc::clone(&state);
        tokio::spawn(async move {
            collect_logs(stderr, log_state).await;
        });
    }

    *state.child.lock().await = Some(child);
    Ok(service_status(&state).await)
}

#[tauri::command]
async fn stop_frpc(state: State<'_, Arc<ProcessState>>) -> AppResult<ServiceStatus> {
    terminate_child(&state).await?;
    Ok(service_status(&state).await)
}

#[tauri::command]
async fn get_service_status(state: State<'_, Arc<ProcessState>>) -> AppResult<ServiceStatus> {
    Ok(service_status(&state).await)
}

#[tauri::command]
async fn import_runtime(app: AppHandle, request: ImportRuntimeRequest) -> AppResult<Vec<RuntimeInfo>> {
    let archive_path = PathBuf::from(&request.archive_path);
    let candidate = extract_runtime_candidate(&archive_path)?;
    install_runtime(&app, candidate)?;
    list_runtimes(&app).map_err(AppError::from)
}

#[tauri::command]
async fn download_and_import_runtime(
    app: AppHandle,
    request: DownloadRuntimeRequest,
) -> AppResult<Vec<RuntimeInfo>> {
    let bytes = reqwest::Client::new()
        .get(&request.download_url)
        .header("User-Agent", "frpc-tunnel-manager")
        .send()
        .await
        .context("下载版本包失败")?
        .error_for_status()
        .context("下载版本包返回非成功状态")?
        .bytes()
        .await
        .context("读取下载内容失败")?;

    let candidate = extract_runtime_candidate_from_bytes(&request.archive_name, &bytes)?;
    install_runtime(&app, candidate)?;
    list_runtimes(&app).map_err(AppError::from)
}

#[tauri::command]
async fn set_current_runtime(
    app: AppHandle,
    request: SetCurrentRuntimeRequest,
) -> AppResult<Vec<RuntimeInfo>> {
    let runtimes = list_runtimes(&app)?;
    if !runtimes.iter().any(|runtime| runtime.id == request.id) {
        return Err(anyhow!("找不到指定版本：{}", request.id).into());
    }
    write_settings(
        &app,
        &AppSettings {
            current_runtime_id: Some(request.id),
        },
    )?;
    list_runtimes(&app).map_err(AppError::from)
}

#[tauri::command]
async fn delete_runtime(app: AppHandle, id: String) -> AppResult<Vec<RuntimeInfo>> {
    let runtimes = list_runtimes(&app)?;
    let target = runtimes
        .iter()
        .find(|runtime| runtime.id == id)
        .ok_or_else(|| anyhow!("找不到版本：{}", id))?;

    if target.is_current {
        return Err(anyhow!("当前版本不可删除，请先切换到其他版本").into());
    }

    let target_dir = runtimes_dir(&app)?.join(&target.id);
    if target_dir.exists() {
        fs::remove_dir_all(&target_dir).context("删除版本目录失败")?;
    }

    list_runtimes(&app).map_err(AppError::from)
}

#[tauri::command]
async fn check_latest_release() -> AppResult<ReleaseInfo> {
    let release = reqwest::Client::new()
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", "frpc-tunnel-manager")
        .send()
        .await
        .context("检查 GitHub Release 失败")?
        .error_for_status()
        .context("GitHub Release 返回非成功状态")?
        .json::<GithubRelease>()
        .await
        .context("解析 GitHub Release 失败")?;

    Ok(ReleaseInfo {
        version: release.tag_name,
        html_url: release.html_url,
        assets: release
            .assets
            .into_iter()
            .filter_map(|asset| {
                let platform = platform_from_asset_name(&asset.name)?;
                Some(ReleaseAsset {
                    name: asset.name,
                    download_url: asset.browser_download_url,
                    platform: Some(platform),
                })
            })
            .collect(),
    })
}

#[tauri::command]
async fn open_path(path: String) -> AppResult<()> {
    #[cfg(target_os = "macos")]
    StdCommand::new("open")
        .arg(path)
        .spawn()
        .context("打开路径失败")?;

    #[cfg(target_os = "windows")]
    StdCommand::new("explorer")
        .arg(path)
        .spawn()
        .context("打开路径失败")?;

    Ok(())
}

async fn collect_logs<R>(reader: R, state: Arc<ProcessState>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        append_log(&state, line).await;
    }
}

async fn append_log(state: &Arc<ProcessState>, line: impl Into<String>) {
    let mut logs = state.logs.lock().await;
    logs.push(line.into());
    let overflow = logs.len().saturating_sub(240);
    if overflow > 0 {
        logs.drain(0..overflow);
    }
}

async fn terminate_child(state: &Arc<ProcessState>) -> anyhow::Result<()> {
    let child = {
        let mut child_guard = state.child.lock().await;
        child_guard.take()
    };

    if let Some(mut child) = child {
        let pid = child.id();
        if let Err(error) = child.kill().await {
            append_log(state, format!("停止 frpc 失败：{error}")).await;
        }
        let _ = child.wait().await;
        if let Some(pid) = pid {
            append_log(state, format!("frpc 已停止，PID {pid}")).await;
        } else {
            append_log(state, "frpc 已停止").await;
        }
    }

    Ok(())
}

async fn service_status(state: &Arc<ProcessState>) -> ServiceStatus {
    let mut exit_log = None;
    let (running, pid) = {
        let mut child_guard = state.child.lock().await;
        if let Some(child) = child_guard.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit_log = Some(format!("frpc 已退出：{status}"));
                    *child_guard = None;
                    (false, None)
                }
                Ok(None) => (true, child.id()),
                Err(error) => {
                    exit_log = Some(format!("读取 frpc 状态失败：{error}"));
                    *child_guard = None;
                    (false, None)
                }
            }
        } else {
            (false, None)
        }
    };

    if let Some(line) = exit_log {
        append_log(state, line).await;
    }

    let logs = state.logs.lock().await.clone();
    ServiceStatus {
        running,
        pid,
        logs,
    }
}

fn ensure_workspace(app: &AppHandle) -> anyhow::Result<()> {
    fs::create_dir_all(workspace_dir(app)?).context("创建工作区失败")?;
    fs::create_dir_all(runtimes_dir(app)?).context("创建运行时目录失败")?;

    let config = config_path(app)?;
    if !config.exists() {
        fs::write(&config, default_config_text(app)?.as_bytes()).context("初始化 frpc.toml 失败")?;
    }

    let settings = settings_path(app)?;
    if !settings.exists() {
        write_settings(app, &AppSettings::default())?;
    }

    Ok(())
}

fn ensure_seed_runtime(app: &AppHandle) -> anyhow::Result<()> {
    let platform = current_platform();
    let seed_binary = app
        .path()
        .resolve(
            format!("resources/frpc/{platform}/{}", binary_name()).as_str(),
            tauri::path::BaseDirectory::Resource,
        )
        .context("定位内置 frpc 失败")?;

    if !seed_binary.exists() {
        return Ok(());
    }

    let target_dir = runtimes_dir(app)?.join(runtime_id(DEFAULT_VERSION, &platform));
    let target_binary = target_dir.join(binary_name());
    if !target_binary.exists() {
        fs::create_dir_all(&target_dir).context("创建内置运行时目录失败")?;
        fs::copy(&seed_binary, &target_binary).context("复制内置 frpc 失败")?;
        write_runtime_manifest(
            &target_dir,
            DEFAULT_VERSION,
            &platform,
            "bundled-frpc",
            &target_binary,
        )?;
    }

    let settings = read_settings(app)?;
    if settings.current_runtime_id.is_none() {
        write_settings(
            app,
            &AppSettings {
                current_runtime_id: Some(runtime_id(DEFAULT_VERSION, &platform)),
            },
        )?;
    }

    Ok(())
}

fn path_info(app: &AppHandle, runtime: Option<&RuntimeInfo>) -> anyhow::Result<PathInfo> {
    Ok(PathInfo {
        app_data_dir: app_data_dir(app)?.display().to_string(),
        workspace_dir: workspace_dir(app)?.display().to_string(),
        config_path: config_path(app)?.display().to_string(),
        runtimes_dir: runtimes_dir(app)?.display().to_string(),
        current_binary: runtime.map(|item| item.binary_path.clone()),
    })
}

fn app_data_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    app.path()
        .app_data_dir()
        .context("无法定位应用数据目录")
}

fn workspace_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(app_data_dir(app)?.join("workspace"))
}

fn config_path(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(workspace_dir(app)?.join("frpc.toml"))
}

fn runtimes_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(app_data_dir(app)?.join("runtimes"))
}

fn settings_path(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(app_data_dir(app)?.join("settings.json"))
}

fn default_config_text(app: &AppHandle) -> anyhow::Result<String> {
    let bundled = app
        .path()
        .resolve(
            "resources/defaults/frpc.toml",
            tauri::path::BaseDirectory::Resource,
        )
        .context("定位默认配置失败")?;

    if bundled.exists() {
        fs::read_to_string(&bundled).context("读取默认配置失败")
    } else {
        Ok(render_quick_config(&SaveQuickConfigRequest {
            server_addr: "127.0.0.1".to_string(),
            server_port: 7000,
            auth_method: "token".to_string(),
            auth_token: String::new(),
            proxies: vec![QuickProxyRequest {
                name: "web".to_string(),
                proxy_type: "tcp".to_string(),
                local_ip: "127.0.0.1".to_string(),
                local_port: 8080,
                remote_port: 8080,
            }],
        }))
    }
}

fn read_config_text(app: &AppHandle) -> anyhow::Result<String> {
    fs::read_to_string(config_path(app)?).context("读取 frpc.toml 失败")
}

fn validate_config_text(config_text: &str) -> anyhow::Result<()> {
    toml::from_str::<toml::Value>(config_text).context("TOML 格式校验失败")?;
    Ok(())
}

fn parse_config_summary(config_text: &str) -> ConfigSummary {
    let value = toml::from_str::<toml::Value>(config_text)
        .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()));

    let server_addr = get_string(&value, &["serverAddr"]).unwrap_or_default();
    let server_port = get_integer(&value, &["serverPort"]).map(|port| port as u16);
    let auth_method = get_string(&value, &["auth", "method"]).unwrap_or_else(|| "token".to_string());
    let auth_token = get_string(&value, &["auth", "token"]).unwrap_or_default();

    let proxies = value
        .get("proxies")
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| ProxySummary {
                    name: get_string(item, &["name"]).unwrap_or_default(),
                    proxy_type: get_string(item, &["type"]).unwrap_or_else(|| "tcp".to_string()),
                    local_ip: get_string(item, &["localIP"]).unwrap_or_else(|| "127.0.0.1".to_string()),
                    local_port: get_integer(item, &["localPort"]).map(|port| port as u16),
                    remote_port: get_integer(item, &["remotePort"]).map(|port| port as u16),
                })
                .collect()
        })
        .unwrap_or_default();

    ConfigSummary {
        server_addr,
        server_port,
        auth_method,
        auth_token,
        proxies,
    }
}

fn get_string(value: &toml::Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(value, |current, segment| current.get(*segment))
        .and_then(toml::Value::as_str)
        .map(ToString::to_string)
}

fn get_integer(value: &toml::Value, path: &[&str]) -> Option<i64> {
    path.iter()
        .try_fold(value, |current, segment| current.get(*segment))
        .and_then(toml::Value::as_integer)
}

fn render_quick_config(request: &SaveQuickConfigRequest) -> String {
    let mut config = format!(
        r#"serverAddr = "{server_addr}"
serverPort = {server_port}

auth.method = "{auth_method}"
auth.token = "{auth_token}"

"#,
        server_addr = escape_toml_string(&request.server_addr),
        server_port = request.server_port,
        auth_method = escape_toml_string(&request.auth_method),
        auth_token = escape_toml_string(&request.auth_token),
    );

    for proxy in &request.proxies {
        config.push_str(&format!(
            r#"[[proxies]]
name = "{proxy_name}"
type = "{proxy_type}"
localIP = "{local_ip}"
localPort = {local_port}
remotePort = {remote_port}

"#,
            proxy_name = escape_toml_string(&proxy.name),
            proxy_type = escape_toml_string(&proxy.proxy_type),
            local_ip = escape_toml_string(&proxy.local_ip),
            local_port = proxy.local_port,
            remote_port = proxy.remote_port
        ));
    }

    config
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn read_settings(app: &AppHandle) -> anyhow::Result<AppSettings> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(AppSettings::default());
    }

    let text = fs::read_to_string(path).context("读取设置失败")?;
    serde_json::from_str(&text).context("解析设置失败")
}

fn write_settings(app: &AppHandle, settings: &AppSettings) -> anyhow::Result<()> {
    fs::create_dir_all(app_data_dir(app)?).context("创建应用数据目录失败")?;
    fs::write(
        settings_path(app)?,
        serde_json::to_vec_pretty(settings).context("序列化设置失败")?,
    )
    .context("写入设置失败")
}

fn current_runtime(app: &AppHandle) -> anyhow::Result<RuntimeInfo> {
    let runtimes = list_runtimes(app)?;
    let settings = read_settings(app)?;
    select_current_runtime(&runtimes, &settings).ok_or_else(|| anyhow!("没有可用的 frpc 运行时"))
}

fn select_current_runtime(
    runtimes: &[RuntimeInfo],
    settings: &AppSettings,
) -> Option<RuntimeInfo> {
    settings
        .current_runtime_id
        .as_ref()
        .and_then(|id| runtimes.iter().find(|runtime| &runtime.id == id))
        .cloned()
        .or_else(|| runtimes.first().cloned())
}

fn list_runtimes(app: &AppHandle) -> anyhow::Result<Vec<RuntimeInfo>> {
    let settings = read_settings(app)?;
    let mut runtimes = Vec::new();
    let runtimes_path = runtimes_dir(app)?;

    if !runtimes_path.exists() {
        return Ok(runtimes);
    }

    for entry in fs::read_dir(runtimes_path).context("读取运行时目录失败")? {
        let entry = entry.context("读取运行时条目失败")?;
        if !entry.file_type().context("读取运行时文件类型失败")?.is_dir() {
            continue;
        }

        let dir = entry.path();
        let manifest = dir.join("manifest.json");
        if !manifest.exists() {
            continue;
        }

        let text = fs::read_to_string(&manifest).context("读取运行时清单失败")?;
        let mut info: RuntimeInfo = serde_json::from_str(&text).context("解析运行时清单失败")?;
        info.is_current = settings
            .current_runtime_id
            .as_ref()
            .map(|id| id == &info.id)
            .unwrap_or(false);
        info.can_delete = !info.is_current;
        runtimes.push(info);
    }

    runtimes.sort_by(|left, right| right.imported_at.cmp(&left.imported_at));
    Ok(runtimes)
}

fn write_runtime_manifest(
    target_dir: &Path,
    version: &str,
    platform: &str,
    archive_name: &str,
    binary_path: &Path,
) -> anyhow::Result<()> {
    let id = runtime_id(version, platform);
    let imported_at = Local::now().to_rfc3339();
    let info = RuntimeInfo {
        id,
        version: version.to_string(),
        platform: platform.to_string(),
        imported_at,
        archive_name: archive_name.to_string(),
        binary_path: binary_path.display().to_string(),
        is_current: false,
        can_delete: true,
    };
    fs::write(
        target_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&info).context("序列化运行时清单失败")?,
    )
    .context("写入运行时清单失败")
}

fn install_runtime(app: &AppHandle, candidate: RuntimeCandidate) -> anyhow::Result<()> {
    let id = runtime_id(&candidate.version, &candidate.platform);
    let target_dir = runtimes_dir(app)?.join(&id);
    let binary_path = target_dir.join(binary_name_for_platform(&candidate.platform));

    fs::create_dir_all(&target_dir).context("创建运行时目录失败")?;
    fs::write(&binary_path, candidate.binary_bytes).context("写入 frpc 二进制失败")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&binary_path)
            .context("读取 frpc 二进制权限失败")?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary_path, permissions).context("设置 frpc 可执行权限失败")?;
    }

    write_runtime_manifest(
        &target_dir,
        &candidate.version,
        &candidate.platform,
        &candidate.archive_name,
        &binary_path,
    )
}

fn extract_runtime_candidate(archive_path: &Path) -> anyhow::Result<RuntimeCandidate> {
    let archive_name = archive_path
        .file_name()
        .and_then(|item| item.to_str())
        .ok_or_else(|| anyhow!("版本包文件名无效"))?
        .to_string();
    let bytes = fs::read(archive_path).context("读取版本包失败")?;
    extract_runtime_candidate_from_bytes(&archive_name, &bytes)
}

fn extract_runtime_candidate_from_bytes(
    archive_name: &str,
    bytes: &[u8],
) -> anyhow::Result<RuntimeCandidate> {
    let (version, platform) = parse_archive_name(archive_name)?;
    let binary_bytes = if archive_name.ends_with(".zip") {
        extract_from_zip(bytes, &platform)?
    } else if archive_name.ends_with(".tar.gz") || archive_name.ends_with(".tgz") {
        extract_from_tar_gz(bytes, &platform)?
    } else {
        return Err(anyhow!("仅支持 .tar.gz、.tgz、.zip 版本包"));
    };

    Ok(RuntimeCandidate {
        version,
        platform,
        archive_name: archive_name.to_string(),
        binary_bytes,
    })
}

fn extract_from_tar_gz(bytes: &[u8], platform: &str) -> anyhow::Result<Vec<u8>> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder);
    let expected = binary_name_for_platform(platform);

    for entry in archive.entries().context("读取 tar.gz 版本包失败")? {
        let mut entry = entry.context("读取 tar.gz 条目失败")?;
        let path = entry.path().context("读取 tar.gz 条目路径失败")?;
        if path
            .file_name()
            .and_then(|item| item.to_str())
            .map(|name| name.eq_ignore_ascii_case(&expected))
            .unwrap_or(false)
        {
            let mut output = Vec::new();
            entry.read_to_end(&mut output).context("提取 frpc 失败")?;
            return Ok(output);
        }
    }

    Err(anyhow!("版本包中没有找到 {}", expected))
}

fn extract_from_zip(bytes: &[u8], platform: &str) -> anyhow::Result<Vec<u8>> {
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor).context("读取 zip 版本包失败")?;
    let expected = binary_name_for_platform(platform);

    for index in 0..archive.len() {
        let mut file = archive.by_index(index).context("读取 zip 条目失败")?;
        let path = PathBuf::from(file.name());
        if path
            .file_name()
            .and_then(|item| item.to_str())
            .map(|name| name.eq_ignore_ascii_case(&expected))
            .unwrap_or(false)
        {
            let mut output = Vec::new();
            file.read_to_end(&mut output).context("提取 frpc 失败")?;
            return Ok(output);
        }
    }

    Err(anyhow!("版本包中没有找到 {}", expected))
}

fn parse_archive_name(archive_name: &str) -> anyhow::Result<(String, String)> {
    let clean = archive_name
        .trim_end_matches(".tar.gz")
        .trim_end_matches(".tgz")
        .trim_end_matches(".zip");
    let parts: Vec<&str> = clean.split('_').collect();
    if parts.len() < 4 || parts.first() != Some(&"frp") {
        return Err(anyhow!("版本包命名不符合 frp 发布格式：{}", archive_name));
    }

    let version = parts
        .get(1)
        .map(|item| format!("v{}", item.trim_start_matches('v')))
        .ok_or_else(|| anyhow!("无法识别版本号"))?;
    let os = parts
        .get(2)
        .ok_or_else(|| anyhow!("无法识别系统平台"))?
        .to_string();
    let arch = parts
        .get(3)
        .ok_or_else(|| anyhow!("无法识别系统架构"))?
        .to_string();

    let platform = normalize_platform(&os, &arch)
        .ok_or_else(|| anyhow!("仅支持 Windows 和 macOS 的 amd64/arm64 版本包"))?;

    Ok((version, platform))
}

fn platform_from_asset_name(name: &str) -> Option<String> {
    let clean = name
        .trim_end_matches(".tar.gz")
        .trim_end_matches(".tgz")
        .trim_end_matches(".zip");
    let parts: Vec<&str> = clean.split('_').collect();
    if parts.len() < 4 || parts.first() != Some(&"frp") {
        return None;
    }
    normalize_platform(parts.get(2)?, parts.get(3)?)
}

fn normalize_platform(os: &str, arch: &str) -> Option<String> {
    let os = match os {
        "darwin" => "darwin",
        "windows" => "windows",
        _ => return None,
    };
    let arch = match arch {
        "amd64" => "x64",
        "arm64" => "arm64",
        _ => return None,
    };
    Some(format!("{os}-{arch}"))
}

fn current_platform() -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unsupported"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    format!("{os}-{arch}")
}

fn runtime_id(version: &str, platform: &str) -> String {
    format!("{}_{}", version.trim_start_matches('v'), platform)
}

fn binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "frpc.exe"
    } else {
        "frpc"
    }
}

fn binary_name_for_platform(platform: &str) -> &'static str {
    if platform.starts_with("windows-") {
        "frpc.exe"
    } else {
        "frpc"
    }
}

#[allow(dead_code)]
fn format_import_time(timestamp: &str) -> String {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|time| time.with_timezone(&Local).format("%Y/%m/%d %H:%M").to_string())
        .unwrap_or_else(|_| timestamp.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn quick_config_output_is_valid_toml_document() {
        let request = SaveQuickConfigRequest {
            server_addr: "203.0.113.10".to_string(),
            server_port: 7000,
            auth_method: "token".to_string(),
            auth_token: String::new(),
            proxies: vec![QuickProxyRequest {
                name: "pet-h5".to_string(),
                proxy_type: "tcp".to_string(),
                local_ip: "127.0.0.1".to_string(),
                local_port: 2999,
                remote_port: 2999,
            }],
        };

        let config_text = render_quick_config(&request);

        validate_config_text(&config_text).expect("quick config should render valid TOML");
        let summary = parse_config_summary(&config_text);
        assert_eq!(summary.server_addr, "203.0.113.10");
        assert_eq!(summary.proxies[0].name, "pet-h5");
    }

    #[test]
    fn release_asset_names_are_mapped_to_supported_platforms() {
        assert_eq!(
            platform_from_asset_name("frp_0.67.0_windows_amd64.zip"),
            Some("windows-x64".to_string())
        );
        assert_eq!(
            platform_from_asset_name("frp_0.67.0_darwin_arm64.tar.gz"),
            Some("darwin-arm64".to_string())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_child_returns_stopped_status() {
        let state = Arc::new(ProcessState::default());
        let child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("test child should start");

        *state.child.lock().await = Some(child);

        tokio::time::timeout(Duration::from_secs(2), terminate_child(&state))
            .await
            .expect("stop should not deadlock")
            .expect("stop should succeed");

        let status = tokio::time::timeout(Duration::from_secs(2), service_status(&state))
            .await
            .expect("status should not deadlock");

        assert!(!status.running);
        assert_eq!(status.pid, None);
    }
}
