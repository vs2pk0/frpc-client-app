use anyhow::{anyhow, Context};
use chrono::{DateTime, Local};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    future::Future,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tar::Archive;
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
};
use zip::ZipArchive;

const DEFAULT_VERSION: &str = "v0.67.0";
const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/fatedier/frp/releases/latest";
const DEFAULT_WORKSPACE_ID: &str = "default";

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
    services: Mutex<BTreeMap<String, ManagedService>>,
    service_operation: Mutex<()>,
    logs: Mutex<BTreeMap<String, Vec<String>>>,
    log_cleanup_policies: Mutex<BTreeMap<String, LogCleanupPolicy>>,
    close_prompt_open: AtomicBool,
    shutdown_requested: AtomicBool,
}

struct LogCleanupPolicy {
    interval_minutes: u64,
    next_cleanup_at: Instant,
}

struct ManagedService {
    workspace_id: String,
    runtime_id: String,
    proxy_index: usize,
    proxy_name: String,
    config_text: String,
    child: Child,
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
    #[serde(default)]
    is_in_use: bool,
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
    running_count: usize,
    services: Vec<ProxyServiceStatus>,
    logs: Vec<String>,
    workspace_running_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
struct ProxyServiceStatus {
    proxy_index: usize,
    proxy_name: String,
    running: bool,
    pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct DashboardState {
    workspaces: Vec<WorkspaceInfo>,
    current_workspace: WorkspaceRecord,
    paths: PathInfo,
    current_platform: String,
    config_text: String,
    config_summary: ConfigSummary,
    runtimes: Vec<RuntimeInfo>,
    current_runtime: Option<RuntimeInfo>,
    service: ServiceStatus,
    log_cleanup_interval_minutes: u64,
    latest_release: Option<ReleaseInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AppSettings {
    current_runtime_id: Option<String>,
    current_workspace_id: Option<String>,
    #[serde(default)]
    log_cleanup_intervals: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceRecord {
    id: String,
    name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WorkspaceRegistry {
    workspaces: Vec<WorkspaceRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkspaceInfo {
    id: String,
    name: String,
    running_count: usize,
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
    workspace_id: String,
    config_text: String,
}

#[derive(Debug, Deserialize)]
struct SaveWorkspaceQuickConfigRequest {
    workspace_id: String,
    config: SaveQuickConfigRequest,
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

#[derive(Debug, Deserialize)]
struct ProxyServiceRequest {
    workspace_id: String,
    proxy_index: usize,
}

#[derive(Debug, Deserialize)]
struct WorkspaceRequest {
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RequiredWorkspaceRequest {
    workspace_id: String,
}

#[derive(Debug, Deserialize)]
struct CreateWorkspaceRequest {
    source_workspace_id: String,
}

#[derive(Debug, Deserialize)]
struct RenameWorkspaceRequest {
    workspace_id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct LogCleanupRequest {
    workspace_id: String,
    interval_minutes: u64,
}

#[derive(Debug, Clone)]
struct RuntimeCandidate {
    version: String,
    platform: String,
    archive_name: String,
    binary_bytes: Vec<u8>,
}

pub fn run() {
    let process_state = Arc::new(ProcessState::default());

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .manage(Arc::clone(&process_state))
        .invoke_handler(tauri::generate_handler![
            get_dashboard_state,
            create_workspace,
            rename_workspace,
            delete_workspace,
            reload_config,
            save_config,
            save_quick_config,
            reset_config,
            start_frpc,
            stop_frpc,
            start_proxy_service,
            stop_proxy_service,
            restart_proxy_service,
            get_service_status,
            clear_workspace_logs,
            set_log_cleanup_interval,
            import_runtime,
            download_and_import_runtime,
            set_current_runtime,
            delete_runtime,
            check_latest_release,
            open_path,
            open_containing_folder
        ])
        .on_window_event({
            let process_state = Arc::clone(&process_state);
            move |window, event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();

                    if process_state.shutdown_requested.load(Ordering::SeqCst)
                        || process_state.close_prompt_open.swap(true, Ordering::SeqCst)
                    {
                        return;
                    }

                    let app = window.app_handle().clone();
                    let process_state = Arc::clone(&process_state);
                    window
                        .dialog()
                        .message("确定要退出 FRPC Tunnel Manager 吗？\n退出前会停止所有正在运行的 frpc 服务，未保存的配置修改将丢失。")
                        .title("退出确认")
                        .kind(MessageDialogKind::Warning)
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            "确定".to_string(),
                            "取消".to_string(),
                        ))
                        .show(move |confirmed| {
                            let app = app.clone();
                            let process_state = Arc::clone(&process_state);
                            tauri::async_runtime::spawn(async move {
                                process_state.close_prompt_open.store(false, Ordering::SeqCst);
                                let app_for_cleanup = app.clone();
                                let process_state_for_cleanup = Arc::clone(&process_state);
                                if let Err(error) =
                                    confirm_shutdown_and_exit(
                                        Arc::clone(&process_state),
                                        confirmed,
                                        move || async move {
                                            let _operation = process_state_for_cleanup
                                                .service_operation
                                                .lock()
                                                .await;
                                            terminate_all_services(
                                                &app_for_cleanup,
                                                &process_state_for_cleanup,
                                            )
                                            .await
                                        },
                                        move || app.exit(0),
                                    )
                                    .await
                                {
                                    process_state
                                        .shutdown_requested
                                        .store(false, Ordering::SeqCst);
                                    append_log(
                                        &process_state,
                                        DEFAULT_WORKSPACE_ID,
                                        format!("退出前停止服务失败：{error}"),
                                    )
                                    .await;
                                }
                            });
                        });
                }
            }
        })
        .setup(|app| {
            #[cfg(target_os = "macos")]
            apply_macos_app_icon().map_err(|error| error.to_string())?;
            ensure_workspaces(app.handle()).map_err(|error| error.to_string())?;
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
    request: WorkspaceRequest,
) -> AppResult<DashboardState> {
    let _operation = state.service_operation.lock().await;
    ensure_workspaces(&app)?;
    ensure_seed_runtime(&app)?;

    let (registry, current_workspace) = select_workspace(&app, request.workspace_id.as_deref())?;
    let config_text = read_config_text(&app, &current_workspace.id)?;
    reconcile_services_for_config(state.inner(), &current_workspace.id, &config_text).await;
    let config_summary = parse_config_summary(&config_text);
    let settings = read_settings(&app)?;
    sync_log_cleanup_policies(state.inner(), &settings.log_cleanup_intervals).await;
    let service = service_status(&state, &current_workspace.id).await;
    let runtimes = list_runtimes_with_usage(&app, state.inner()).await?;
    let current_runtime = select_current_runtime(&runtimes, &settings);
    let workspaces = workspace_infos(&registry, &service.workspace_running_counts);
    let log_cleanup_interval_minutes = settings
        .log_cleanup_intervals
        .get(&current_workspace.id)
        .copied()
        .filter(|interval| is_valid_log_cleanup_interval(*interval))
        .unwrap_or(0);

    Ok(DashboardState {
        workspaces,
        current_workspace: current_workspace.clone(),
        paths: path_info(&app, &current_workspace.id, current_runtime.as_ref())?,
        current_platform: current_platform(),
        config_text,
        config_summary,
        runtimes,
        current_runtime,
        service,
        log_cleanup_interval_minutes,
        latest_release: None,
    })
}

#[tauri::command]
async fn create_workspace(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: CreateWorkspaceRequest,
) -> AppResult<WorkspaceRecord> {
    let _operation = state.service_operation.lock().await;
    ensure_workspaces(&app)?;
    let workspace = create_workspace_copy(&app, &request.source_workspace_id)?;
    let settings = read_settings(&app)?;
    sync_log_cleanup_policies(state.inner(), &settings.log_cleanup_intervals).await;
    Ok(workspace)
}

#[tauri::command]
async fn rename_workspace(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: RenameWorkspaceRequest,
) -> AppResult<WorkspaceRecord> {
    let _operation = state.service_operation.lock().await;
    ensure_workspaces(&app)?;
    let mut registry = read_workspace_registry(&app)?;
    let workspace = rename_workspace_record(&mut registry, &request.workspace_id, &request.name)?;
    write_workspace_registry(&app, &registry)?;
    Ok(workspace)
}

#[tauri::command]
async fn delete_workspace(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: RequiredWorkspaceRequest,
) -> AppResult<WorkspaceRecord> {
    let _operation = state.service_operation.lock().await;
    ensure_workspaces(&app)?;
    let mut registry = read_workspace_registry(&app)?;
    let next_workspace = remove_workspace_record(&mut registry, &request.workspace_id)?;
    let original_settings = read_settings(&app)?;
    terminate_workspace_services(&app, state.inner(), &request.workspace_id).await?;

    let dir = workspace_dir(&app, &request.workspace_id)?;
    let staged_dir = if dir.exists() {
        let staged = workspaces_dir(&app)?.join(format!(
            ".deleting-{}-{}",
            request.workspace_id,
            Local::now().timestamp_millis()
        ));
        fs::rename(&dir, &staged).context("暂存待删除工作区失败")?;
        Some(staged)
    } else {
        None
    };

    let mut next_settings = original_settings.clone();
    let current_workspace_changed =
        next_settings.current_workspace_id.as_deref() == Some(&request.workspace_id);
    if current_workspace_changed {
        next_settings.current_workspace_id = Some(next_workspace.id.clone());
    }
    let cleanup_settings_changed = next_settings
        .log_cleanup_intervals
        .remove(&request.workspace_id)
        .is_some();
    let settings_changed = current_workspace_changed || cleanup_settings_changed;

    let commit_result = (|| -> anyhow::Result<()> {
        if settings_changed {
            write_settings(&app, &next_settings)?;
        }
        write_workspace_registry(&app, &registry)
    })();
    if let Err(error) = commit_result {
        if settings_changed {
            let _ = write_settings(&app, &original_settings);
        }
        if let Some(staged) = staged_dir.as_ref() {
            fs::rename(staged, &dir).context("删除失败且恢复工作区目录失败")?;
        }
        return Err(error.into());
    }

    state.logs.lock().await.remove(&request.workspace_id);
    state
        .log_cleanup_policies
        .lock()
        .await
        .remove(&request.workspace_id);
    if let Some(staged) = staged_dir {
        if let Err(error) = fs::remove_dir_all(&staged) {
            append_log(
                state.inner(),
                &next_workspace.id,
                format!("工作区已删除，但暂存目录清理失败：{error}"),
            )
            .await;
        }
    }

    Ok(next_workspace)
}

#[tauri::command]
async fn reload_config(app: AppHandle, request: RequiredWorkspaceRequest) -> AppResult<String> {
    ensure_workspace_exists(&app, &request.workspace_id)?;
    read_config_text(&app, &request.workspace_id).map_err(AppError::from)
}

#[tauri::command]
async fn save_config(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: SaveConfigRequest,
) -> AppResult<ConfigSummary> {
    validate_config_text(&request.config_text)?;
    let _operation = state.service_operation.lock().await;
    ensure_workspace_exists(&app, &request.workspace_id)?;
    fs::write(
        config_path(&app, &request.workspace_id)?,
        request.config_text.as_bytes(),
    )
    .context("写入 frpc.toml 失败")?;
    reconcile_services_for_config(state.inner(), &request.workspace_id, &request.config_text).await;
    Ok(parse_config_summary(&request.config_text))
}

#[tauri::command]
async fn save_quick_config(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: SaveWorkspaceQuickConfigRequest,
) -> AppResult<(String, ConfigSummary)> {
    let config_text = render_quick_config(&request.config);
    validate_config_text(&config_text)?;
    let _operation = state.service_operation.lock().await;
    ensure_workspace_exists(&app, &request.workspace_id)?;
    fs::write(
        config_path(&app, &request.workspace_id)?,
        config_text.as_bytes(),
    )
    .context("写入快捷配置失败")?;
    reconcile_services_for_config(state.inner(), &request.workspace_id, &config_text).await;
    let summary = parse_config_summary(&config_text);
    Ok((config_text, summary))
}

#[tauri::command]
async fn reset_config(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: RequiredWorkspaceRequest,
) -> AppResult<String> {
    let defaults = default_config_text(&app)?;
    validate_config_text(&defaults)?;
    let _operation = state.service_operation.lock().await;
    ensure_workspace_exists(&app, &request.workspace_id)?;
    fs::write(
        config_path(&app, &request.workspace_id)?,
        defaults.as_bytes(),
    )
    .context("恢复默认配置失败")?;
    reconcile_services_for_config(state.inner(), &request.workspace_id, &defaults).await;
    Ok(defaults)
}

#[tauri::command]
async fn start_frpc(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: RequiredWorkspaceRequest,
) -> AppResult<ServiceStatus> {
    let _operation = state.service_operation.lock().await;
    ensure_workspace_exists(&app, &request.workspace_id)?;
    let config_text = read_config_text(&app, &request.workspace_id)?;
    let proxy_count = parse_config_summary(&config_text).proxies.len();
    if proxy_count == 0 {
        return Err(anyhow!("没有可启动的代理配置").into());
    }

    let mut started_proxy_indexes = Vec::new();
    for proxy_index in 0..proxy_count {
        match start_proxy_child(&app, state.inner(), &request.workspace_id, proxy_index).await {
            Ok(true) => started_proxy_indexes.push(proxy_index),
            Ok(false) => {}
            Err(error) => {
                rollback_started_proxy_children(
                    state.inner(),
                    &request.workspace_id,
                    started_proxy_indexes,
                )
                .await?;
                return Err(error.into());
            }
        }
    }

    Ok(service_status(&state, &request.workspace_id).await)
}

#[tauri::command]
async fn stop_frpc(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: RequiredWorkspaceRequest,
) -> AppResult<ServiceStatus> {
    let _operation = state.service_operation.lock().await;
    ensure_workspace_exists(&app, &request.workspace_id)?;
    terminate_workspace_services(&app, state.inner(), &request.workspace_id).await?;
    Ok(service_status(&state, &request.workspace_id).await)
}

#[tauri::command]
async fn start_proxy_service(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: ProxyServiceRequest,
) -> AppResult<ServiceStatus> {
    let _operation = state.service_operation.lock().await;
    ensure_workspace_exists(&app, &request.workspace_id)?;
    start_proxy_child(
        &app,
        state.inner(),
        &request.workspace_id,
        request.proxy_index,
    )
    .await?;
    Ok(service_status(&state, &request.workspace_id).await)
}

#[tauri::command]
async fn stop_proxy_service(
    state: State<'_, Arc<ProcessState>>,
    request: ProxyServiceRequest,
) -> AppResult<ServiceStatus> {
    let _operation = state.service_operation.lock().await;
    terminate_proxy_child(state.inner(), &request.workspace_id, request.proxy_index).await?;
    Ok(service_status(&state, &request.workspace_id).await)
}

#[tauri::command]
async fn restart_proxy_service(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: ProxyServiceRequest,
) -> AppResult<ServiceStatus> {
    let _operation = state.service_operation.lock().await;
    ensure_workspace_exists(&app, &request.workspace_id)?;
    terminate_proxy_child(state.inner(), &request.workspace_id, request.proxy_index).await?;
    start_proxy_child(
        &app,
        state.inner(),
        &request.workspace_id,
        request.proxy_index,
    )
    .await?;
    Ok(service_status(&state, &request.workspace_id).await)
}

#[tauri::command]
async fn get_service_status(
    state: State<'_, Arc<ProcessState>>,
    request: RequiredWorkspaceRequest,
) -> AppResult<ServiceStatus> {
    Ok(service_status(&state, &request.workspace_id).await)
}

#[tauri::command]
async fn clear_workspace_logs(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: RequiredWorkspaceRequest,
) -> AppResult<ServiceStatus> {
    let _operation = state.service_operation.lock().await;
    ensure_workspace_exists(&app, &request.workspace_id)?;
    clear_workspace_log_buffer(state.inner(), &request.workspace_id).await;
    Ok(service_status(&state, &request.workspace_id).await)
}

#[tauri::command]
async fn set_log_cleanup_interval(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: LogCleanupRequest,
) -> AppResult<u64> {
    if !is_valid_log_cleanup_interval(request.interval_minutes) {
        return Err(anyhow!("不支持的日志定时清理周期").into());
    }

    let _operation = state.service_operation.lock().await;
    ensure_workspace_exists(&app, &request.workspace_id)?;
    let mut settings = read_settings(&app)?;
    if request.interval_minutes == 0 {
        settings.log_cleanup_intervals.remove(&request.workspace_id);
    } else {
        settings
            .log_cleanup_intervals
            .insert(request.workspace_id.clone(), request.interval_minutes);
    }
    write_settings(&app, &settings)?;
    configure_log_cleanup_policy(
        state.inner(),
        &request.workspace_id,
        request.interval_minutes,
    )
    .await;
    Ok(request.interval_minutes)
}

#[tauri::command]
async fn import_runtime(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: ImportRuntimeRequest,
) -> AppResult<Vec<RuntimeInfo>> {
    let _operation = state.service_operation.lock().await;
    let archive_path = PathBuf::from(&request.archive_path);
    let candidate = extract_runtime_candidate(&archive_path)?;
    let _ = service_status(state.inner(), DEFAULT_WORKSPACE_ID).await;
    ensure_runtime_not_in_use(
        state.inner(),
        &runtime_id(&candidate.version, &candidate.platform),
    )
    .await?;
    install_runtime(&app, candidate)?;
    list_runtimes_with_usage(&app, state.inner())
        .await
        .map_err(AppError::from)
}

#[tauri::command]
async fn download_and_import_runtime(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
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
    let _operation = state.service_operation.lock().await;
    let _ = service_status(state.inner(), DEFAULT_WORKSPACE_ID).await;
    ensure_runtime_not_in_use(
        state.inner(),
        &runtime_id(&candidate.version, &candidate.platform),
    )
    .await?;
    install_runtime(&app, candidate)?;
    list_runtimes_with_usage(&app, state.inner())
        .await
        .map_err(AppError::from)
}

#[tauri::command]
async fn set_current_runtime(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    request: SetCurrentRuntimeRequest,
) -> AppResult<Vec<RuntimeInfo>> {
    let _operation = state.service_operation.lock().await;
    let runtimes = list_runtimes(&app)?;
    if !runtimes.iter().any(|runtime| runtime.id == request.id) {
        return Err(anyhow!("找不到指定版本：{}", request.id).into());
    }
    let mut settings = read_settings(&app)?;
    settings.current_runtime_id = Some(request.id);
    write_settings(&app, &settings)?;
    list_runtimes_with_usage(&app, state.inner())
        .await
        .map_err(AppError::from)
}

#[tauri::command]
async fn delete_runtime(
    app: AppHandle,
    state: State<'_, Arc<ProcessState>>,
    id: String,
) -> AppResult<Vec<RuntimeInfo>> {
    let _operation = state.service_operation.lock().await;
    let _ = service_status(state.inner(), DEFAULT_WORKSPACE_ID).await;
    let runtimes = list_runtimes_with_usage(&app, state.inner()).await?;
    let target = runtimes
        .iter()
        .find(|runtime| runtime.id == id)
        .ok_or_else(|| anyhow!("找不到版本：{}", id))?;

    if target.is_current || target.is_in_use {
        return Err(anyhow!("当前版本或服务正在使用的版本不可删除").into());
    }

    let target_dir = runtimes_dir(&app)?.join(&target.id);
    if target_dir.exists() {
        fs::remove_dir_all(&target_dir).context("删除版本目录失败")?;
    }

    list_runtimes_with_usage(&app, state.inner())
        .await
        .map_err(AppError::from)
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

#[tauri::command]
async fn open_containing_folder(path: String) -> AppResult<()> {
    let directory = containing_directory(Path::new(&path))?;

    #[cfg(target_os = "macos")]
    StdCommand::new("open")
        .arg(&directory)
        .spawn()
        .context("打开文件所在目录失败")?;

    #[cfg(target_os = "windows")]
    StdCommand::new("explorer")
        .arg(&directory)
        .spawn()
        .context("打开文件所在目录失败")?;

    Ok(())
}

fn containing_directory(path: &Path) -> anyhow::Result<PathBuf> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("无法确定文件所在目录：{}", path.display()))
}

async fn start_proxy_child(
    app: &AppHandle,
    state: &Arc<ProcessState>,
    workspace_id: &str,
    proxy_index: usize,
) -> anyhow::Result<bool> {
    let _ = service_status(state, workspace_id).await;
    let service_key = proxy_service_key(workspace_id, proxy_index);
    if state.services.lock().await.contains_key(&service_key) {
        return Ok(false);
    }

    let runtime = current_runtime(app)?;
    let binary = PathBuf::from(&runtime.binary_path);
    if !binary.exists() {
        return Err(anyhow!("当前 frpc 二进制不存在：{}", binary.display()));
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

    let config_text = read_config_text(app, workspace_id)?;
    let (service_config_text, proxy_name) = render_single_proxy_config(&config_text, proxy_index)?;
    let config = service_config_path(app, workspace_id, proxy_index, &proxy_name)?;
    fs::write(&config, service_config_text.as_bytes()).context("写入单服务配置失败")?;

    let mut command = Command::new(&binary);
    command
        .arg("-c")
        .arg(&config)
        .current_dir(workspace_dir(app, workspace_id)?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("启动代理 {} 失败", proxy_name))?;

    let log_label = format!("[{}]", proxy_name);
    append_log(
        state,
        workspace_id,
        format!(
            "{} 已启动，PID {}",
            log_label,
            child
                .id()
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
    )
    .await;

    if let Some(stdout) = child.stdout.take() {
        let log_state = Arc::clone(state);
        let log_workspace_id = workspace_id.to_string();
        let label = log_label.clone();
        tokio::spawn(async move {
            collect_logs(stdout, log_state, log_workspace_id, label).await;
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let log_state = Arc::clone(state);
        let log_workspace_id = workspace_id.to_string();
        tokio::spawn(async move {
            collect_logs(stderr, log_state, log_workspace_id, log_label).await;
        });
    }

    state.services.lock().await.insert(
        service_key,
        ManagedService {
            workspace_id: workspace_id.to_string(),
            runtime_id: runtime.id,
            proxy_index,
            proxy_name,
            config_text: service_config_text,
            child,
        },
    );

    Ok(true)
}

async fn collect_logs<R>(reader: R, state: Arc<ProcessState>, workspace_id: String, label: String)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        append_log(&state, &workspace_id, format!("{label} {line}")).await;
    }
}

async fn append_log(state: &Arc<ProcessState>, workspace_id: &str, line: impl Into<String>) {
    clear_workspace_logs_if_due(state, workspace_id).await;
    let mut logs = state.logs.lock().await;
    let logs = logs.entry(workspace_id.to_string()).or_default();
    logs.push(line.into());
    let overflow = logs.len().saturating_sub(240);
    if overflow > 0 {
        logs.drain(0..overflow);
    }
}

fn is_valid_log_cleanup_interval(interval_minutes: u64) -> bool {
    matches!(interval_minutes, 0 | 15 | 30 | 60 | 360 | 720 | 1440)
}

fn next_log_cleanup_at(interval_minutes: u64) -> Instant {
    Instant::now() + Duration::from_secs(interval_minutes * 60)
}

async fn configure_log_cleanup_policy(
    state: &Arc<ProcessState>,
    workspace_id: &str,
    interval_minutes: u64,
) {
    let mut policies = state.log_cleanup_policies.lock().await;
    if interval_minutes == 0 {
        policies.remove(workspace_id);
        return;
    }

    match policies.get(workspace_id) {
        Some(policy) if policy.interval_minutes == interval_minutes => {}
        _ => {
            policies.insert(
                workspace_id.to_string(),
                LogCleanupPolicy {
                    interval_minutes,
                    next_cleanup_at: next_log_cleanup_at(interval_minutes),
                },
            );
        }
    }
}

async fn sync_log_cleanup_policies(state: &Arc<ProcessState>, intervals: &BTreeMap<String, u64>) {
    let mut policies = state.log_cleanup_policies.lock().await;
    policies.retain(|workspace_id, policy| {
        intervals.get(workspace_id).copied() == Some(policy.interval_minutes)
            && is_valid_log_cleanup_interval(policy.interval_minutes)
            && policy.interval_minutes > 0
    });

    for (workspace_id, interval_minutes) in intervals {
        if *interval_minutes > 0
            && is_valid_log_cleanup_interval(*interval_minutes)
            && !policies.contains_key(workspace_id)
        {
            policies.insert(
                workspace_id.clone(),
                LogCleanupPolicy {
                    interval_minutes: *interval_minutes,
                    next_cleanup_at: next_log_cleanup_at(*interval_minutes),
                },
            );
        }
    }
}

async fn clear_workspace_logs_if_due(state: &Arc<ProcessState>, workspace_id: &str) {
    let mut policies = state.log_cleanup_policies.lock().await;
    let Some(policy) = policies.get_mut(workspace_id) else {
        return;
    };
    if Instant::now() < policy.next_cleanup_at {
        return;
    }

    policy.next_cleanup_at = next_log_cleanup_at(policy.interval_minutes);
    state.logs.lock().await.remove(workspace_id);
}

async fn clear_workspace_log_buffer(state: &Arc<ProcessState>, workspace_id: &str) {
    let mut policies = state.log_cleanup_policies.lock().await;
    if let Some(policy) = policies.get_mut(workspace_id) {
        policy.next_cleanup_at = next_log_cleanup_at(policy.interval_minutes);
    }
    state.logs.lock().await.remove(workspace_id);
}

async fn terminate_all_services(app: &AppHandle, state: &Arc<ProcessState>) -> anyhow::Result<()> {
    let services = {
        let mut service_guard = state.services.lock().await;
        std::mem::take(&mut *service_guard)
    };

    for (_, service) in services {
        terminate_service(state, service).await;
    }

    terminate_managed_frpc_processes(app, state).await
}

async fn terminate_workspace_services(
    app: &AppHandle,
    state: &Arc<ProcessState>,
    workspace_id: &str,
) -> anyhow::Result<()> {
    let services = {
        let mut service_guard = state.services.lock().await;
        let service_keys = service_guard
            .iter()
            .filter(|(_, service)| service.workspace_id == workspace_id)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        service_keys
            .into_iter()
            .filter_map(|key| service_guard.remove(&key))
            .collect::<Vec<_>>()
    };

    for service in services {
        terminate_service(state, service).await;
    }

    terminate_managed_frpc_processes_for_workspace(app, state, workspace_id).await
}

async fn terminate_proxy_child(
    state: &Arc<ProcessState>,
    workspace_id: &str,
    proxy_index: usize,
) -> anyhow::Result<()> {
    let service = {
        let mut service_guard = state.services.lock().await;
        service_guard.remove(&proxy_service_key(workspace_id, proxy_index))
    };

    if let Some(service) = service {
        terminate_service(state, service).await;
    }

    Ok(())
}

async fn rollback_started_proxy_children(
    state: &Arc<ProcessState>,
    workspace_id: &str,
    proxy_indexes: Vec<usize>,
) -> anyhow::Result<()> {
    for proxy_index in proxy_indexes.into_iter().rev() {
        terminate_proxy_child(state, workspace_id, proxy_index).await?;
    }
    Ok(())
}

async fn reconcile_services_for_config(
    state: &Arc<ProcessState>,
    workspace_id: &str,
    config_text: &str,
) {
    let stale_services = {
        let mut services = state.services.lock().await;
        let stale_keys = services
            .iter()
            .filter_map(|(key, service)| {
                if service.workspace_id != workspace_id {
                    return None;
                }
                let matches_running_config =
                    render_single_proxy_config(config_text, service.proxy_index)
                        .map(|(next_config, _)| next_config == service.config_text)
                        .unwrap_or(false);
                (!matches_running_config).then(|| key.clone())
            })
            .collect::<Vec<_>>();

        stale_keys
            .into_iter()
            .filter_map(|key| services.remove(&key))
            .collect::<Vec<_>>()
    };

    for service in stale_services {
        terminate_service(state, service).await;
    }
}

async fn terminate_service(state: &Arc<ProcessState>, mut service: ManagedService) {
    let pid = service.child.id();
    let label = format!("[{}]", service.proxy_name);
    if let Err(error) = service.child.kill().await {
        append_log(
            state,
            &service.workspace_id,
            format!("{label} 停止 frpc 失败：{error}"),
        )
        .await;
    }
    let _ = service.child.wait().await;
    if let Some(pid) = pid {
        append_log(
            state,
            &service.workspace_id,
            format!("{label} frpc 已停止，PID {pid}"),
        )
        .await;
    } else {
        append_log(state, &service.workspace_id, format!("{label} frpc 已停止")).await;
    }
}

#[cfg(target_os = "windows")]
async fn terminate_managed_frpc_processes(
    app: &AppHandle,
    state: &Arc<ProcessState>,
) -> anyhow::Result<()> {
    let script = windows_frpc_cleanup_script(&app_data_dir(app)?, None);
    let mut command = Command::new("powershell.exe");
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    command.creation_flags(CREATE_NO_WINDOW);

    let output = command.output().await.context("清理残留 frpc.exe 失败")?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !stdout.is_empty() {
        append_log(state, DEFAULT_WORKSPACE_ID, stdout).await;
    }
    if !stderr.is_empty() {
        append_log(state, DEFAULT_WORKSPACE_ID, stderr.clone()).await;
    }

    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!("清理残留 frpc.exe 失败：{stderr}"))
    }
}

#[cfg(target_os = "windows")]
async fn terminate_managed_frpc_processes_for_workspace(
    app: &AppHandle,
    state: &Arc<ProcessState>,
    workspace_id: &str,
) -> anyhow::Result<()> {
    let workspace = workspace_dir(app, workspace_id)?;
    let script = windows_frpc_cleanup_script(&app_data_dir(app)?, Some(&workspace));
    let mut command = Command::new("powershell.exe");
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    command.creation_flags(CREATE_NO_WINDOW);

    let output = command
        .output()
        .await
        .context("清理工作区残留 frpc.exe 失败")?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stdout.is_empty() {
        append_log(state, workspace_id, stdout).await;
    }
    if !stderr.is_empty() {
        append_log(state, workspace_id, stderr.clone()).await;
    }
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!("清理工作区残留 frpc.exe 失败：{stderr}"))
    }
}

#[cfg(not(target_os = "windows"))]
async fn terminate_managed_frpc_processes(
    _app: &AppHandle,
    _state: &Arc<ProcessState>,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
async fn terminate_managed_frpc_processes_for_workspace(
    _app: &AppHandle,
    _state: &Arc<ProcessState>,
    _workspace_id: &str,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
fn normalize_windows_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

#[cfg(any(target_os = "windows", test))]
fn windows_frpc_cleanup_script(app_data_dir: &Path, workspace_dir: Option<&Path>) -> String {
    let app_data_dir = powershell_single_quoted(&normalize_windows_path(app_data_dir));
    let workspace_dir = workspace_dir
        .map(normalize_windows_path)
        .map(|path| powershell_single_quoted(&path))
        .unwrap_or_else(|| "$null".to_string());
    format!(
        r#"$root = {app_data_dir}
$workspace = {workspace_dir}
Get-CimInstance Win32_Process -Filter "Name = 'frpc.exe'" | Where-Object {{
  if (-not $_.ExecutablePath) {{ $false }} else {{
    $path = ($_.ExecutablePath -replace '\\', '/').TrimEnd('/').ToLowerInvariant()
    $commandLine = if ($_.CommandLine) {{ ($_.CommandLine -replace '\\', '/').ToLowerInvariant() }} else {{ '' }}
    $inWorkspace = (-not $workspace) -or $commandLine.Contains($workspace)
    $path.EndsWith('/frpc.exe') -and $path.StartsWith($root + '/') -and $inWorkspace
  }}
}} | ForEach-Object {{
  Stop-Process -Id $_.ProcessId -Force -ErrorAction Stop
  Write-Output ("已停止残留 frpc.exe，PID " + $_.ProcessId)
}}"#
    )
}

#[cfg(any(target_os = "windows", test))]
fn powershell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

async fn confirm_shutdown_and_exit<C, Fut, F>(
    state: Arc<ProcessState>,
    confirmed: bool,
    cleanup: C,
    exit: F,
) -> anyhow::Result<bool>
where
    C: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send,
    F: FnOnce() + Send + 'static,
{
    if !confirmed {
        return Ok(false);
    }

    state.shutdown_requested.store(true, Ordering::SeqCst);
    cleanup().await?;
    exit();
    Ok(true)
}

async fn service_status(state: &Arc<ProcessState>, workspace_id: &str) -> ServiceStatus {
    let mut exit_logs = Vec::new();
    let (services, workspace_running_counts) = {
        let mut service_guard = state.services.lock().await;
        let mut exited_keys = Vec::new();
        let mut services = Vec::new();
        let mut workspace_running_counts = BTreeMap::new();

        for (key, service) in service_guard.iter_mut() {
            match service.child.try_wait() {
                Ok(Some(status)) => {
                    exit_logs.push((
                        service.workspace_id.clone(),
                        format!("[{}] frpc 已退出：{status}", service.proxy_name),
                    ));
                    exited_keys.push(key.clone());
                }
                Ok(None) => {
                    *workspace_running_counts
                        .entry(service.workspace_id.clone())
                        .or_insert(0) += 1;
                    if service.workspace_id == workspace_id {
                        services.push(ProxyServiceStatus {
                            proxy_index: service.proxy_index,
                            proxy_name: service.proxy_name.clone(),
                            running: true,
                            pid: service.child.id(),
                        });
                    }
                }
                Err(error) => {
                    exit_logs.push((
                        service.workspace_id.clone(),
                        format!("[{}] 读取 frpc 状态失败：{error}", service.proxy_name),
                    ));
                    exited_keys.push(key.clone());
                }
            }
        }

        for key in exited_keys {
            service_guard.remove(&key);
        }

        (services, workspace_running_counts)
    };

    for (log_workspace_id, line) in exit_logs {
        append_log(state, &log_workspace_id, line).await;
    }

    clear_workspace_logs_if_due(state, workspace_id).await;
    let logs = state
        .logs
        .lock()
        .await
        .get(workspace_id)
        .cloned()
        .unwrap_or_default();
    let running_count = services.len();
    let pid = if running_count == 1 {
        services[0].pid
    } else {
        None
    };
    ServiceStatus {
        running: running_count > 0,
        pid,
        running_count,
        services,
        logs,
        workspace_running_counts,
    }
}

fn ensure_workspaces(app: &AppHandle) -> anyhow::Result<()> {
    fs::create_dir_all(app_data_dir(app)?).context("创建应用数据目录失败")?;
    fs::create_dir_all(workspaces_dir(app)?).context("创建工作区目录失败")?;
    fs::create_dir_all(runtimes_dir(app)?).context("创建运行时目录失败")?;

    let mut registry = read_workspace_registry(app)?;
    if registry.workspaces.is_empty() {
        registry.workspaces.push(WorkspaceRecord {
            id: DEFAULT_WORKSPACE_ID.to_string(),
            name: "工作区 1".to_string(),
        });
        write_workspace_registry(app, &registry)?;
    }

    for workspace in &registry.workspaces {
        let dir = workspace_dir(app, &workspace.id)?;
        fs::create_dir_all(&dir).context("创建工作区失败")?;
        let config = config_path(app, &workspace.id)?;
        if config.exists() {
            continue;
        }

        let legacy_config = legacy_workspace_dir(app)?.join("frpc.toml");
        if workspace.id == DEFAULT_WORKSPACE_ID && legacy_config.exists() {
            fs::copy(&legacy_config, &config).context("迁移原工作区配置失败")?;
        } else {
            fs::write(&config, default_config_text(app)?.as_bytes())
                .context("初始化 frpc.toml 失败")?;
        }
    }

    let settings = settings_path(app)?;
    recover_atomic_file(&settings)?;
    if !settings.exists() {
        write_settings(
            app,
            &AppSettings {
                current_workspace_id: Some(registry.workspaces[0].id.clone()),
                ..AppSettings::default()
            },
        )?;
    } else {
        let mut settings = read_settings(app)?;
        let current_exists = settings.current_workspace_id.as_ref().is_some_and(|id| {
            registry
                .workspaces
                .iter()
                .any(|workspace| &workspace.id == id)
        });
        if !current_exists {
            settings.current_workspace_id = Some(registry.workspaces[0].id.clone());
            write_settings(app, &settings)?;
        }
    }

    Ok(())
}

fn ensure_workspace_exists(app: &AppHandle, workspace_id: &str) -> anyhow::Result<WorkspaceRecord> {
    read_workspace_registry(app)?
        .workspaces
        .into_iter()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| anyhow!("找不到工作区：{workspace_id}"))
}

fn select_workspace(
    app: &AppHandle,
    requested_workspace_id: Option<&str>,
) -> anyhow::Result<(WorkspaceRegistry, WorkspaceRecord)> {
    let registry = read_workspace_registry(app)?;
    let mut settings = read_settings(app)?;
    let selected_id = requested_workspace_id
        .map(ToString::to_string)
        .or_else(|| settings.current_workspace_id.clone());
    let workspace = selected_id
        .as_ref()
        .and_then(|id| {
            registry
                .workspaces
                .iter()
                .find(|workspace| &workspace.id == id)
        })
        .cloned()
        .or_else(|| registry.workspaces.first().cloned())
        .ok_or_else(|| anyhow!("没有可用的工作区"))?;

    if settings.current_workspace_id.as_deref() != Some(&workspace.id) {
        settings.current_workspace_id = Some(workspace.id.clone());
        write_settings(app, &settings)?;
    }

    Ok((registry, workspace))
}

fn create_workspace_copy(
    app: &AppHandle,
    source_workspace_id: &str,
) -> anyhow::Result<WorkspaceRecord> {
    ensure_workspace_exists(app, source_workspace_id)?;
    let mut registry = read_workspace_registry(app)?;
    let mut sequence = 1;
    while registry
        .workspaces
        .iter()
        .any(|workspace| workspace.name == format!("工作区 {sequence}"))
    {
        sequence += 1;
    }
    let mut id = format!("workspace-{}", Local::now().timestamp_millis());
    let mut collision_suffix = 1;
    while registry
        .workspaces
        .iter()
        .any(|workspace| workspace.id == id)
    {
        collision_suffix += 1;
        id = format!(
            "workspace-{}-{collision_suffix}",
            Local::now().timestamp_millis()
        );
    }

    let workspace = WorkspaceRecord {
        id,
        name: format!("工作区 {sequence}"),
    };
    let dir = workspace_dir(app, &workspace.id)?;
    fs::create_dir_all(&dir).context("创建新工作区失败")?;
    fs::copy(
        config_path(app, source_workspace_id)?,
        config_path(app, &workspace.id)?,
    )
    .context("复制当前工作区配置失败")?;

    registry.workspaces.push(workspace.clone());
    write_workspace_registry(app, &registry)?;
    let mut settings = read_settings(app)?;
    if let Some(interval_minutes) = settings
        .log_cleanup_intervals
        .get(source_workspace_id)
        .copied()
    {
        settings
            .log_cleanup_intervals
            .insert(workspace.id.clone(), interval_minutes);
    }
    settings.current_workspace_id = Some(workspace.id.clone());
    write_settings(app, &settings)?;
    Ok(workspace)
}

fn workspace_infos(
    registry: &WorkspaceRegistry,
    running_counts: &BTreeMap<String, usize>,
) -> Vec<WorkspaceInfo> {
    registry
        .workspaces
        .iter()
        .map(|workspace| WorkspaceInfo {
            id: workspace.id.clone(),
            name: workspace.name.clone(),
            running_count: running_counts.get(&workspace.id).copied().unwrap_or(0),
        })
        .collect()
}

fn remove_workspace_record(
    registry: &mut WorkspaceRegistry,
    workspace_id: &str,
) -> anyhow::Result<WorkspaceRecord> {
    if registry.workspaces.len() <= 1 {
        return Err(anyhow!("至少需要保留一个工作区"));
    }
    let workspace_index = registry
        .workspaces
        .iter()
        .position(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| anyhow!("找不到工作区：{workspace_id}"))?;
    registry.workspaces.remove(workspace_index);
    Ok(registry.workspaces[workspace_index.min(registry.workspaces.len() - 1)].clone())
}

fn rename_workspace_record(
    registry: &mut WorkspaceRegistry,
    workspace_id: &str,
    name: &str,
) -> anyhow::Result<WorkspaceRecord> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("工作区名称不能为空"));
    }
    if name.chars().count() > 32 {
        return Err(anyhow!("工作区名称不能超过 32 个字符"));
    }
    if registry
        .workspaces
        .iter()
        .any(|workspace| workspace.id != workspace_id && workspace.name.eq_ignore_ascii_case(name))
    {
        return Err(anyhow!("工作区名称已存在：{name}"));
    }

    let workspace = registry
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| anyhow!("找不到工作区：{workspace_id}"))?;
    workspace.name = name.to_string();
    Ok(workspace.clone())
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

    let mut settings = read_settings(app)?;
    if settings.current_runtime_id.is_none() {
        settings.current_runtime_id = Some(runtime_id(DEFAULT_VERSION, &platform));
        write_settings(app, &settings)?;
    }

    Ok(())
}

fn path_info(
    app: &AppHandle,
    workspace_id: &str,
    runtime: Option<&RuntimeInfo>,
) -> anyhow::Result<PathInfo> {
    Ok(PathInfo {
        app_data_dir: app_data_dir(app)?.display().to_string(),
        workspace_dir: workspace_dir(app, workspace_id)?.display().to_string(),
        config_path: config_path(app, workspace_id)?.display().to_string(),
        runtimes_dir: runtimes_dir(app)?.display().to_string(),
        current_binary: runtime.map(|item| item.binary_path.clone()),
    })
}

fn app_data_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    app.path().app_data_dir().context("无法定位应用数据目录")
}

fn legacy_workspace_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(app_data_dir(app)?.join("workspace"))
}

fn workspaces_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(app_data_dir(app)?.join("workspaces"))
}

fn workspace_dir(app: &AppHandle, workspace_id: &str) -> anyhow::Result<PathBuf> {
    Ok(workspaces_dir(app)?.join(workspace_id))
}

fn config_path(app: &AppHandle, workspace_id: &str) -> anyhow::Result<PathBuf> {
    Ok(workspace_dir(app, workspace_id)?.join("frpc.toml"))
}

fn service_configs_dir(app: &AppHandle, workspace_id: &str) -> anyhow::Result<PathBuf> {
    Ok(workspace_dir(app, workspace_id)?.join("services"))
}

fn service_config_path(
    app: &AppHandle,
    workspace_id: &str,
    proxy_index: usize,
    proxy_name: &str,
) -> anyhow::Result<PathBuf> {
    let dir = service_configs_dir(app, workspace_id)?;
    fs::create_dir_all(&dir).context("创建单服务配置目录失败")?;
    Ok(dir.join(format!(
        "{:03}-{}.toml",
        proxy_index + 1,
        sanitize_file_segment(proxy_name)
    )))
}

fn runtimes_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(app_data_dir(app)?.join("runtimes"))
}

fn settings_path(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(app_data_dir(app)?.join("settings.json"))
}

fn workspace_registry_path(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(app_data_dir(app)?.join("workspaces.json"))
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

fn read_config_text(app: &AppHandle, workspace_id: &str) -> anyhow::Result<String> {
    fs::read_to_string(config_path(app, workspace_id)?).context("读取 frpc.toml 失败")
}

fn validate_config_text(config_text: &str) -> anyhow::Result<()> {
    toml::from_str::<toml::Value>(config_text).context("TOML 格式校验失败")?;
    Ok(())
}

fn render_single_proxy_config(
    config_text: &str,
    proxy_index: usize,
) -> anyhow::Result<(String, String)> {
    let mut value = toml::from_str::<toml::Value>(config_text).context("TOML 格式校验失败")?;
    let proxy = value
        .get("proxies")
        .and_then(toml::Value::as_array)
        .and_then(|proxies| proxies.get(proxy_index))
        .cloned()
        .ok_or_else(|| anyhow!("找不到第 {} 个代理配置", proxy_index + 1))?;
    let proxy_name =
        get_string(&proxy, &["name"]).unwrap_or_else(|| format!("proxy-{}", proxy_index + 1));

    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow!("frpc.toml 必须是 TOML 表结构"))?;
    table.insert("proxies".to_string(), toml::Value::Array(vec![proxy]));

    let config_text = toml::to_string_pretty(&value).context("生成单服务配置失败")?;
    Ok((config_text, proxy_name))
}

fn parse_config_summary(config_text: &str) -> ConfigSummary {
    let value = toml::from_str::<toml::Value>(config_text)
        .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()));

    let server_addr = get_string(&value, &["serverAddr"]).unwrap_or_default();
    let server_port = get_integer(&value, &["serverPort"]).map(|port| port as u16);
    let auth_method =
        get_string(&value, &["auth", "method"]).unwrap_or_else(|| "token".to_string());
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
                    local_ip: get_string(item, &["localIP"])
                        .unwrap_or_else(|| "127.0.0.1".to_string()),
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

fn proxy_service_key(workspace_id: &str, proxy_index: usize) -> String {
    format!("{workspace_id}:{proxy_index}")
}

fn sanitize_file_segment(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "proxy".to_string()
    } else {
        sanitized.to_string()
    }
}

fn read_workspace_registry(app: &AppHandle) -> anyhow::Result<WorkspaceRegistry> {
    let path = workspace_registry_path(app)?;
    recover_atomic_file(&path)?;
    if !path.exists() {
        return Ok(WorkspaceRegistry::default());
    }

    let text = fs::read_to_string(path).context("读取工作区列表失败")?;
    serde_json::from_str(&text).context("解析工作区列表失败")
}

fn write_workspace_registry(app: &AppHandle, registry: &WorkspaceRegistry) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(registry).context("序列化工作区列表失败")?;
    write_file_atomically(&workspace_registry_path(app)?, &bytes).context("写入工作区列表失败")
}

fn read_settings(app: &AppHandle) -> anyhow::Result<AppSettings> {
    let path = settings_path(app)?;
    recover_atomic_file(&path)?;
    if !path.exists() {
        return Ok(AppSettings::default());
    }

    let text = fs::read_to_string(path).context("读取设置失败")?;
    serde_json::from_str(&text).context("解析设置失败")
}

fn write_settings(app: &AppHandle, settings: &AppSettings) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(settings).context("序列化设置失败")?;
    write_file_atomically(&settings_path(app)?, &bytes).context("写入设置失败")
}

fn atomic_sidecar_path(path: &Path, suffix: &str) -> anyhow::Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("原子写入路径无效：{}", path.display()))?;
    Ok(path.with_file_name(format!(".{file_name}.{suffix}")))
}

fn recover_atomic_file(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let backup = atomic_sidecar_path(path, "backup")?;
    if backup.exists() {
        fs::rename(&backup, path)
            .with_context(|| format!("恢复原子写入备份失败：{}", path.display()))?;
    }
    Ok(())
}

fn write_file_atomically(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("原子写入路径缺少父目录：{}", path.display()))?;
    fs::create_dir_all(parent).context("创建原子写入目录失败")?;

    let temporary = atomic_sidecar_path(path, "temporary")?;
    let backup = atomic_sidecar_path(path, "backup")?;
    if temporary.exists() {
        fs::remove_file(&temporary).context("清理原子写入临时文件失败")?;
    }
    if backup.exists() {
        fs::remove_file(&backup).context("清理原子写入旧备份失败")?;
    }

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .context("创建原子写入临时文件失败")?;
    file.write_all(bytes).context("写入原子临时文件失败")?;
    file.sync_all().context("同步原子临时文件失败")?;
    drop(file);

    let had_original = path.exists();
    if had_original {
        fs::rename(path, &backup).context("备份原文件失败")?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if had_original {
            let _ = fs::rename(&backup, path);
        }
        return Err(error).context("提交原子写入失败");
    }
    if had_original {
        let _ = fs::remove_file(backup);
    }
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn current_runtime(app: &AppHandle) -> anyhow::Result<RuntimeInfo> {
    let runtimes = list_runtimes(app)?;
    let settings = read_settings(app)?;
    select_current_runtime(&runtimes, &settings).ok_or_else(|| anyhow!("没有可用的 frpc 运行时"))
}

fn select_current_runtime(runtimes: &[RuntimeInfo], settings: &AppSettings) -> Option<RuntimeInfo> {
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
        if !entry
            .file_type()
            .context("读取运行时文件类型失败")?
            .is_dir()
        {
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

async fn list_runtimes_with_usage(
    app: &AppHandle,
    state: &Arc<ProcessState>,
) -> anyhow::Result<Vec<RuntimeInfo>> {
    let mut runtimes = list_runtimes(app)?;
    let active_runtime_ids = state
        .services
        .lock()
        .await
        .values()
        .map(|service| service.runtime_id.clone())
        .collect::<BTreeSet<_>>();
    mark_runtime_usage(&mut runtimes, &active_runtime_ids);
    Ok(runtimes)
}

fn mark_runtime_usage(runtimes: &mut [RuntimeInfo], active_runtime_ids: &BTreeSet<String>) {
    for runtime in runtimes {
        runtime.is_in_use = active_runtime_ids.contains(&runtime.id);
        runtime.can_delete = !runtime.is_current && !runtime.is_in_use;
    }
}

async fn ensure_runtime_not_in_use(
    state: &Arc<ProcessState>,
    runtime_id: &str,
) -> anyhow::Result<()> {
    let is_in_use = state
        .services
        .lock()
        .await
        .values()
        .any(|service| service.runtime_id == runtime_id);
    if is_in_use {
        Err(anyhow!(
            "该 FRPC 版本正被运行中的服务使用，请先停止相关服务"
        ))
    } else {
        Ok(())
    }
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
        is_in_use: false,
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
            .map(|name| name.eq_ignore_ascii_case(expected))
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
            .map(|name| name.eq_ignore_ascii_case(expected))
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
        .map(|time| {
            time.with_timezone(&Local)
                .format("%Y/%m/%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| timestamp.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_running_command() -> Command {
        #[cfg(target_os = "windows")]
        {
            let mut command = Command::new("ping");
            command.arg("-n").arg("30").arg("127.0.0.1");
            command
        }

        #[cfg(not(target_os = "windows"))]
        {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        }
    }

    async fn insert_test_service(state: &Arc<ProcessState>, proxy_index: usize) {
        insert_test_service_with_config(state, DEFAULT_WORKSPACE_ID, proxy_index, String::new())
            .await;
    }

    async fn insert_test_service_with_config(
        state: &Arc<ProcessState>,
        workspace_id: &str,
        proxy_index: usize,
        config_text: String,
    ) {
        insert_test_service_with_runtime_and_config(
            state,
            workspace_id,
            "test-runtime",
            proxy_index,
            config_text,
        )
        .await;
    }

    async fn insert_test_service_with_runtime_and_config(
        state: &Arc<ProcessState>,
        workspace_id: &str,
        runtime_id: &str,
        proxy_index: usize,
        config_text: String,
    ) {
        let child = long_running_command()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("test child should start");

        state.services.lock().await.insert(
            proxy_service_key(workspace_id, proxy_index),
            ManagedService {
                workspace_id: workspace_id.to_string(),
                runtime_id: runtime_id.to_string(),
                proxy_index,
                proxy_name: format!("proxy-{proxy_index}"),
                config_text,
                child,
            },
        );
    }

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
    fn single_proxy_config_keeps_only_selected_proxy() {
        let config_text = render_quick_config(&SaveQuickConfigRequest {
            server_addr: "203.0.113.10".to_string(),
            server_port: 7000,
            auth_method: "token".to_string(),
            auth_token: "secret".to_string(),
            proxies: vec![
                QuickProxyRequest {
                    name: "ip1-web".to_string(),
                    proxy_type: "tcp".to_string(),
                    local_ip: "192.168.1.10".to_string(),
                    local_port: 8080,
                    remote_port: 18080,
                },
                QuickProxyRequest {
                    name: "ip2-api".to_string(),
                    proxy_type: "tcp".to_string(),
                    local_ip: "192.168.1.11".to_string(),
                    local_port: 9000,
                    remote_port: 19000,
                },
            ],
        });

        let (single_config, proxy_name) =
            render_single_proxy_config(&config_text, 1).expect("single proxy config should render");
        let summary = parse_config_summary(&single_config);

        assert_eq!(proxy_name, "ip2-api");
        assert_eq!(summary.server_addr, "203.0.113.10");
        assert_eq!(summary.proxies.len(), 1);
        assert_eq!(summary.proxies[0].local_ip, "192.168.1.11");
        assert_eq!(summary.proxies[0].remote_port, Some(19000));
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

    #[test]
    fn workspace_removal_selects_neighbor_and_keeps_one_workspace() {
        let mut registry = WorkspaceRegistry {
            workspaces: vec![
                WorkspaceRecord {
                    id: "workspace-a".to_string(),
                    name: "工作区 A".to_string(),
                },
                WorkspaceRecord {
                    id: "workspace-b".to_string(),
                    name: "工作区 B".to_string(),
                },
            ],
        };

        let next = remove_workspace_record(&mut registry, "workspace-a")
            .expect("workspace A should be removable");
        assert_eq!(next.id, "workspace-b");
        assert_eq!(registry.workspaces.len(), 1);
        assert!(remove_workspace_record(&mut registry, "workspace-b").is_err());
    }

    #[test]
    fn workspace_rename_validates_and_updates_registry() {
        let mut registry = WorkspaceRegistry {
            workspaces: vec![
                WorkspaceRecord {
                    id: "workspace-a".to_string(),
                    name: "工作区 A".to_string(),
                },
                WorkspaceRecord {
                    id: "workspace-b".to_string(),
                    name: "工作区 B".to_string(),
                },
            ],
        };

        let renamed = rename_workspace_record(&mut registry, "workspace-a", " 办公网 ")
            .expect("workspace should be renamed");
        assert_eq!(renamed.name, "办公网");
        assert!(rename_workspace_record(&mut registry, "workspace-a", "").is_err());
        assert!(rename_workspace_record(&mut registry, "workspace-a", "工作区 B").is_err());
    }

    #[test]
    fn containing_directory_returns_file_parent() {
        let directory = containing_directory(Path::new("/tmp/frpc/config/frpc.toml"))
            .expect("file path should have a parent directory");
        assert_eq!(directory, PathBuf::from("/tmp/frpc/config"));
        assert!(containing_directory(Path::new("frpc.toml")).is_err());
    }

    #[test]
    fn atomic_file_write_replaces_and_recovers_backup() {
        let root = std::env::temp_dir().join(format!(
            "frpc-atomic-write-{}-{}",
            std::process::id(),
            Local::now().timestamp_millis()
        ));
        fs::create_dir_all(&root).expect("test directory should be created");
        let path = root.join("settings.json");

        write_file_atomically(&path, b"first").expect("first write should succeed");
        write_file_atomically(&path, b"second").expect("replacement should succeed");
        assert_eq!(fs::read(&path).expect("file should be readable"), b"second");

        let backup = atomic_sidecar_path(&path, "backup").expect("backup path should resolve");
        fs::rename(&path, &backup).expect("backup should be staged");
        recover_atomic_file(&path).expect("backup should be recovered");
        assert_eq!(
            fs::read(&path).expect("recovered file should be readable"),
            b"second"
        );

        fs::remove_dir_all(root).expect("test directory should be removed");
    }

    #[test]
    fn log_cleanup_intervals_are_restricted_to_supported_options() {
        for interval in [0, 15, 30, 60, 360, 720, 1440] {
            assert!(is_valid_log_cleanup_interval(interval));
        }
        assert!(!is_valid_log_cleanup_interval(1));
        assert!(!is_valid_log_cleanup_interval(1441));
    }

    #[test]
    fn legacy_settings_default_to_disabled_log_cleanup() {
        let settings: AppSettings =
            serde_json::from_str(r#"{"current_runtime_id":null,"current_workspace_id":"default"}"#)
                .expect("legacy settings should remain readable");
        assert!(settings.log_cleanup_intervals.is_empty());
    }

    #[tokio::test]
    async fn workspace_logs_can_be_cleared_manually_and_on_schedule() {
        let state = Arc::new(ProcessState::default());
        let workspace_id = "workspace-a";
        let other_workspace_id = "workspace-b";
        configure_log_cleanup_policy(&state, workspace_id, 15).await;
        configure_log_cleanup_policy(&state, other_workspace_id, 30).await;
        append_log(&state, workspace_id, "first").await;
        append_log(&state, other_workspace_id, "other").await;

        {
            let mut policies = state.log_cleanup_policies.lock().await;
            policies
                .get_mut(workspace_id)
                .expect("cleanup policy should exist")
                .next_cleanup_at = Instant::now() - Duration::from_secs(1);
        }
        append_log(&state, workspace_id, "second").await;
        assert_eq!(
            state.logs.lock().await.get(workspace_id).cloned(),
            Some(vec!["second".to_string()])
        );
        assert_eq!(
            state.logs.lock().await.get(other_workspace_id).cloned(),
            Some(vec!["other".to_string()])
        );

        clear_workspace_log_buffer(&state, workspace_id).await;
        assert!(!state.logs.lock().await.contains_key(workspace_id));
        assert!(state.logs.lock().await.contains_key(other_workspace_id));
        assert!(
            state
                .log_cleanup_policies
                .lock()
                .await
                .get(workspace_id)
                .expect("cleanup policy should remain enabled")
                .next_cleanup_at
                > Instant::now()
        );
    }

    #[test]
    fn windows_cleanup_script_filters_to_app_data_frpc_executable() {
        let app_data_dir =
            Path::new(r"C:\Users\Owner O'Neil\AppData\Roaming\com.dalong.frpc.tunnel-manager");
        let script = windows_frpc_cleanup_script(app_data_dir, None);

        assert!(script
            .contains("'c:/users/owner o''neil/appdata/roaming/com.dalong.frpc.tunnel-manager'"));
        assert!(script.contains("Name = 'frpc.exe'"));
        assert!(script.contains("$path.EndsWith('/frpc.exe')"));
        assert!(script.contains("StartsWith($root + '/')"));
        assert!(script.contains("Stop-Process"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_proxy_child_returns_stopped_status() {
        let state = Arc::new(ProcessState::default());
        insert_test_service(&state, 0).await;

        tokio::time::timeout(
            Duration::from_secs(2),
            terminate_proxy_child(&state, DEFAULT_WORKSPACE_ID, 0),
        )
        .await
        .expect("stop should not deadlock")
        .expect("stop should succeed");

        let status = tokio::time::timeout(
            Duration::from_secs(2),
            service_status(&state, DEFAULT_WORKSPACE_ID),
        )
        .await
        .expect("status should not deadlock");

        assert!(!status.running);
        assert_eq!(status.pid, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rollback_stops_every_proxy_started_by_batch() {
        let state = Arc::new(ProcessState::default());
        insert_test_service(&state, 0).await;
        insert_test_service(&state, 1).await;

        tokio::time::timeout(
            Duration::from_secs(2),
            rollback_started_proxy_children(&state, DEFAULT_WORKSPACE_ID, vec![0, 1]),
        )
        .await
        .expect("rollback should not deadlock")
        .expect("rollback should succeed");

        let status = service_status(&state, DEFAULT_WORKSPACE_ID).await;
        assert!(!status.running);
        assert_eq!(status.running_count, 0);
        assert!(status.services.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn config_reconciliation_stops_only_changed_proxy() {
        let state = Arc::new(ProcessState::default());
        let original = render_quick_config(&SaveQuickConfigRequest {
            server_addr: "203.0.113.10".to_string(),
            server_port: 7000,
            auth_method: "token".to_string(),
            auth_token: "secret".to_string(),
            proxies: vec![
                QuickProxyRequest {
                    name: "ip1-web".to_string(),
                    proxy_type: "tcp".to_string(),
                    local_ip: "192.168.1.10".to_string(),
                    local_port: 8080,
                    remote_port: 18080,
                },
                QuickProxyRequest {
                    name: "ip2-api".to_string(),
                    proxy_type: "tcp".to_string(),
                    local_ip: "192.168.1.11".to_string(),
                    local_port: 9000,
                    remote_port: 19000,
                },
            ],
        });
        let changed = original.replace("localPort = 9000", "localPort = 9001");

        for proxy_index in 0..2 {
            let (service_config, _) = render_single_proxy_config(&original, proxy_index)
                .expect("service config should render");
            insert_test_service_with_config(
                &state,
                DEFAULT_WORKSPACE_ID,
                proxy_index,
                service_config,
            )
            .await;
        }

        reconcile_services_for_config(&state, DEFAULT_WORKSPACE_ID, &changed).await;

        let status = service_status(&state, DEFAULT_WORKSPACE_ID).await;
        assert_eq!(status.running_count, 1);
        assert_eq!(status.services[0].proxy_index, 0);

        terminate_proxy_child(&state, DEFAULT_WORKSPACE_ID, 0)
            .await
            .expect("unchanged service should be cleaned up");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn same_proxy_index_is_isolated_between_workspaces() {
        let state = Arc::new(ProcessState::default());
        insert_test_service_with_config(&state, "workspace-a", 0, String::new()).await;
        insert_test_service_with_config(&state, "workspace-b", 0, String::new()).await;

        terminate_proxy_child(&state, "workspace-a", 0)
            .await
            .expect("workspace A service should stop");

        let workspace_a = service_status(&state, "workspace-a").await;
        let workspace_b = service_status(&state, "workspace-b").await;
        assert!(!workspace_a.running);
        assert!(workspace_b.running);
        assert_eq!(workspace_b.running_count, 1);
        assert_eq!(
            workspace_b.workspace_running_counts.get("workspace-b"),
            Some(&1)
        );

        terminate_proxy_child(&state, "workspace-b", 0)
            .await
            .expect("workspace B service should be cleaned up");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn active_runtime_cannot_be_reinstalled_or_deleted() {
        let state = Arc::new(ProcessState::default());
        insert_test_service_with_runtime_and_config(
            &state,
            DEFAULT_WORKSPACE_ID,
            "runtime-a",
            0,
            String::new(),
        )
        .await;

        assert!(ensure_runtime_not_in_use(&state, "runtime-a")
            .await
            .is_err());
        assert!(ensure_runtime_not_in_use(&state, "runtime-b").await.is_ok());

        let mut runtimes = vec![RuntimeInfo {
            id: "runtime-a".to_string(),
            version: "v1".to_string(),
            platform: "darwin-arm64".to_string(),
            imported_at: String::new(),
            archive_name: String::new(),
            binary_path: String::new(),
            is_current: false,
            is_in_use: false,
            can_delete: true,
        }];
        mark_runtime_usage(&mut runtimes, &BTreeSet::from(["runtime-a".to_string()]));
        assert!(runtimes[0].is_in_use);
        assert!(!runtimes[0].can_delete);

        terminate_proxy_child(&state, DEFAULT_WORKSPACE_ID, 0)
            .await
            .expect("test service should be cleaned up");
    }

    #[tokio::test]
    async fn service_operation_lock_serializes_mutations() {
        let state = Arc::new(ProcessState::default());
        let operation = state.service_operation.lock().await;
        let waiting_state = Arc::clone(&state);
        let waiting = tokio::spawn(async move {
            let _operation = waiting_state.service_operation.lock().await;
        });

        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        drop(operation);
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .expect("waiting operation should acquire the lock")
            .expect("waiting operation should complete");
    }

    #[tokio::test]
    async fn cancelled_shutdown_keeps_service_running_and_does_not_exit() {
        let state = Arc::new(ProcessState::default());
        let cleaned_up = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        insert_test_service(&state, 0).await;

        let cleaned_up_for_closure = Arc::clone(&cleaned_up);
        let exited_for_closure = Arc::clone(&exited);
        let result = confirm_shutdown_and_exit(
            Arc::clone(&state),
            false,
            move || async move {
                cleaned_up_for_closure.store(true, Ordering::SeqCst);
                Ok(())
            },
            move || {
                exited_for_closure.store(true, Ordering::SeqCst);
            },
        )
        .await
        .expect("cancelled shutdown should not fail");

        assert!(!result);
        assert!(!cleaned_up.load(Ordering::SeqCst));
        assert!(!exited.load(Ordering::SeqCst));
        assert!(service_status(&state, DEFAULT_WORKSPACE_ID).await.running);

        terminate_proxy_child(&state, DEFAULT_WORKSPACE_ID, 0)
            .await
            .expect("test child should be cleaned up");
    }

    #[tokio::test]
    async fn confirmed_shutdown_stops_service_before_exit() {
        let state = Arc::new(ProcessState::default());
        let exited_after_stop = Arc::new(AtomicBool::new(false));
        insert_test_service(&state, 0).await;

        let state_for_closure = Arc::clone(&state);
        let exited_for_closure = Arc::clone(&exited_after_stop);
        let state_for_cleanup = Arc::clone(&state);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            confirm_shutdown_and_exit(
                Arc::clone(&state),
                true,
                move || async move {
                    terminate_proxy_child(&state_for_cleanup, DEFAULT_WORKSPACE_ID, 0).await
                },
                move || {
                    let is_stopped = state_for_closure
                        .services
                        .try_lock()
                        .map(|services| services.is_empty())
                        .unwrap_or(false);
                    exited_for_closure.store(is_stopped, Ordering::SeqCst);
                },
            ),
        )
        .await
        .expect("confirmed shutdown should not hang")
        .expect("confirmed shutdown should not fail");

        assert!(result);
        assert!(exited_after_stop.load(Ordering::SeqCst));
        assert!(!service_status(&state, DEFAULT_WORKSPACE_ID).await.running);
    }
}
