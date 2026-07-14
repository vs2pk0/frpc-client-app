import React, { useCallback, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  Check,
  CheckCircle2,
  CircleStop,
  Clock3,
  CopyPlus,
  Download,
  ExternalLink,
  FileCode2,
  FolderOpen,
  Loader2,
  Pencil,
  Plus,
  Play,
  RefreshCw,
  RotateCcw,
  Save,
  Settings2,
  Trash2,
  Wand2,
  X
} from "lucide-react";
import "./styles.css";

type ProxySummary = {
  name: string;
  proxy_type: string;
  local_ip: string;
  local_port: number | null;
  remote_port: number | null;
};

type ConfigSummary = {
  server_addr: string;
  server_port: number | null;
  auth_method: string;
  auth_token: string;
  proxies: ProxySummary[];
};

type RuntimeInfo = {
  id: string;
  version: string;
  platform: string;
  imported_at: string;
  archive_name: string;
  binary_path: string;
  is_current: boolean;
  is_in_use: boolean;
  can_delete: boolean;
};

type ReleaseAsset = {
  name: string;
  download_url: string;
  platform: string | null;
};

type ReleaseInfo = {
  version: string;
  html_url: string;
  assets: ReleaseAsset[];
};

type ServiceStatus = {
  running: boolean;
  pid: number | null;
  running_count: number;
  services: ProxyServiceStatus[];
  logs: string[];
  workspace_running_counts: Record<string, number>;
};

type WorkspaceRecord = {
  id: string;
  name: string;
};

type WorkspaceInfo = WorkspaceRecord & {
  running_count: number;
};

type ProxyServiceStatus = {
  proxy_index: number;
  proxy_name: string;
  running: boolean;
  pid: number | null;
};

type PathInfo = {
  app_data_dir: string;
  workspace_dir: string;
  config_path: string;
  runtimes_dir: string;
  current_binary: string | null;
};

type DashboardState = {
  workspaces: WorkspaceInfo[];
  current_workspace: WorkspaceRecord;
  paths: PathInfo;
  current_platform: string;
  config_text: string;
  config_summary: ConfigSummary;
  runtimes: RuntimeInfo[];
  current_runtime: RuntimeInfo | null;
  service: ServiceStatus;
  log_cleanup_interval_minutes: number;
  latest_release: ReleaseInfo | null;
};

type QuickConfig = {
  server_addr: string;
  server_port: number;
  auth_method: string;
  auth_token: string;
  proxies: QuickProxy[];
};

type QuickProxy = {
  name: string;
  proxy_type: string;
  local_ip: string;
  local_port: number;
  remote_port: number;
};

const defaultQuickProxy: QuickProxy = {
  name: "web",
  proxy_type: "tcp",
  local_ip: "127.0.0.1",
  local_port: 8080,
  remote_port: 8080
};

const defaultQuickConfig: QuickConfig = {
  server_addr: "127.0.0.1",
  server_port: 7000,
  auth_method: "token",
  auth_token: "",
  proxies: [defaultQuickProxy]
};

const logCleanupOptions = [
  { value: 0, label: "不自动清空" },
  { value: 15, label: "每 15 分钟" },
  { value: 30, label: "每 30 分钟" },
  { value: 60, label: "每 1 小时" },
  { value: 360, label: "每 6 小时" },
  { value: 720, label: "每 12 小时" },
  { value: 1440, label: "每 24 小时" }
] as const;

function App() {
  const [dashboard, setDashboard] = useState<DashboardState | null>(null);
  const [configText, setConfigText] = useState("");
  const [quickConfig, setQuickConfig] = useState<QuickConfig>(defaultQuickConfig);
  const [release, setRelease] = useState<ReleaseInfo | null>(null);
  const [busy, setBusy] = useState<string | null>("正在初始化");
  const [notice, setNotice] = useState<string>("正在读取本地工作区");
  const [workspaceError, setWorkspaceError] = useState<string>("");
  const [runtimeError, setRuntimeError] = useState<string>("");
  const [tab, setTab] = useState<"quick" | "editor" | "logs">("quick");
  const [dirtyConfigTab, setDirtyConfigTab] = useState<"quick" | "editor" | null>(null);
  const [renamingWorkspace, setRenamingWorkspace] = useState(false);
  const [workspaceNameDraft, setWorkspaceNameDraft] = useState("");
  const [statusScope, setStatusScope] = useState<"workspace" | "runtime">("workspace");
  const configDirty = dirtyConfigTab !== null;

  const loadDashboard = useCallback(async (workspaceId?: string) => {
    setBusy("刷新中");
    setWorkspaceError("");
    try {
      const data = await invoke<DashboardState>("get_dashboard_state", {
        request: { workspace_id: workspaceId ?? null }
      });
      const summary = parseConfigTextSummary(data.config_text, data.config_summary);
      const syncedData = { ...data, config_summary: summary };
      setDashboard(syncedData);
      setConfigText(data.config_text);
      setQuickConfig(toQuickConfig(summary));
      setDirtyConfigTab(null);
      setRenamingWorkspace(false);
      setStatusScope("workspace");
      setNotice(`${data.current_workspace.name} 已加载`);
    } catch (err) {
      setWorkspaceError(readError(err));
    } finally {
      setBusy(null);
    }
  }, []);

  useEffect(() => {
    loadDashboard();
  }, [loadDashboard]);

  const activeWorkspaceId = dashboard?.current_workspace.id ?? "";
  const hasRunningWorkspaces = Boolean(
    dashboard?.workspaces.some((workspace) => workspace.running_count > 0)
  );
  const shouldPollService =
    hasRunningWorkspaces ||
    (tab === "logs" && (dashboard?.log_cleanup_interval_minutes ?? 0) > 0);

  const applyServiceStatus = useCallback((service: ServiceStatus, workspaceId: string) => {
    setDashboard((current) =>
      current
        ? {
            ...current,
            service: current.current_workspace.id === workspaceId ? service : current.service,
            workspaces: current.workspaces.map((workspace) => ({
              ...workspace,
              running_count: service.workspace_running_counts[workspace.id] ?? 0
            }))
          }
        : current
    );
  }, []);

  useEffect(() => {
    if (!activeWorkspaceId || !shouldPollService) {
      return;
    }
    let cancelled = false;
    const refreshStatus = async () => {
      try {
        const service = await invoke<ServiceStatus>("get_service_status", {
          request: { workspace_id: activeWorkspaceId }
        });
        if (!cancelled) {
          applyServiceStatus(service, activeWorkspaceId);
        }
      } catch (err) {
        if (!cancelled) {
          setWorkspaceError(readError(err));
        }
      }
    };
    const timer = window.setInterval(refreshStatus, 1800);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [activeWorkspaceId, applyServiceStatus, shouldPollService]);

  const selectedAsset = useMemo(() => {
    if (!release || !dashboard) {
      return null;
    }
    const preferredPlatform = dashboard.current_platform || dashboard.current_runtime?.platform;
    return (
      release.assets.find((asset) => asset.platform === preferredPlatform) ??
      release.assets[0] ??
      null
    );
  }, [dashboard, release]);

  const runtimeBusy = statusScope === "runtime" && Boolean(busy);
  const showRuntimeStatus = statusScope === "runtime";

  const runAction = async (
    label: string,
    action: () => Promise<void>,
    scope: "workspace" | "runtime" = "workspace"
  ) => {
    setStatusScope(scope);
    setBusy(label);
    if (scope === "runtime") {
      setRuntimeError("");
    } else {
      setWorkspaceError("");
    }
    try {
      await action();
    } catch (err) {
      if (scope === "runtime") {
        setRuntimeError(readError(err));
      } else {
        setWorkspaceError(readError(err));
      }
    } finally {
      setStatusScope(scope);
      setBusy(null);
    }
  };

  const saveConfig = () =>
    runAction("保存中", async () => {
      const summary = await invoke<ConfigSummary>("save_config", {
        request: { workspace_id: activeWorkspaceId, config_text: configText }
      });
      const service = await invoke<ServiceStatus>("get_service_status", {
        request: { workspace_id: activeWorkspaceId }
      });
      const syncedSummary = parseConfigTextSummary(configText, summary);
      setDashboard((current) =>
        current
          ? { ...current, config_text: configText, config_summary: syncedSummary, service }
          : current
      );
      setQuickConfig(toQuickConfig(syncedSummary));
      setDirtyConfigTab(null);
      applyServiceStatus(service, activeWorkspaceId);
      setNotice("配置已保存，已停止发生变更的代理服务");
    });

  const saveQuickConfig = () =>
    runAction("生成配置中", async () => {
      const [nextText, summary] = await invoke<[string, ConfigSummary]>("save_quick_config", {
        request: { workspace_id: activeWorkspaceId, config: quickConfig }
      });
      const service = await invoke<ServiceStatus>("get_service_status", {
        request: { workspace_id: activeWorkspaceId }
      });
      const syncedSummary = parseConfigTextSummary(nextText, summary);
      setConfigText(nextText);
      setDashboard((current) =>
        current
          ? {
              ...current,
              config_text: nextText,
              config_summary: syncedSummary,
              service
            }
          : current
      );
      setQuickConfig(toQuickConfig(syncedSummary));
      setDirtyConfigTab(null);
      applyServiceStatus(service, activeWorkspaceId);
      setNotice("快捷配置已写入，已停止发生变更的代理服务");
    });

  const resetConfig = () => {
    if (configDirty && !window.confirm("恢复默认配置会丢弃当前未保存的修改，是否继续？")) {
      return;
    }
    void runAction("恢复中", async () => {
      const text = await invoke<string>("reset_config", {
        request: { workspace_id: activeWorkspaceId }
      });
      setConfigText(text);
      await loadDashboard(activeWorkspaceId);
      setNotice("已恢复默认配置，已停止发生变更的代理服务");
    });
  };

  const startService = () =>
    runAction("启动当前工作区中", async () => {
      const service = await invoke<ServiceStatus>("start_frpc", {
        request: { workspace_id: activeWorkspaceId }
      });
      applyServiceStatus(service, activeWorkspaceId);
      setNotice(`${dashboard?.current_workspace.name ?? "当前工作区"} 已启动`);
    });

  const stopService = () =>
    runAction("停止当前工作区中", async () => {
      const service = await invoke<ServiceStatus>("stop_frpc", {
        request: { workspace_id: activeWorkspaceId }
      });
      applyServiceStatus(service, activeWorkspaceId);
      setNotice(`${dashboard?.current_workspace.name ?? "当前工作区"} 已停止`);
    });

  const startProxyService = (proxyIndex: number, proxyName: string) =>
    runAction(`启动 ${proxyName || `#${proxyIndex + 1}`}`, async () => {
      const service = await invoke<ServiceStatus>("start_proxy_service", {
        request: { workspace_id: activeWorkspaceId, proxy_index: proxyIndex }
      });
      applyServiceStatus(service, activeWorkspaceId);
      setNotice(`${proxyName || `#${proxyIndex + 1}`} 已启动`);
    });

  const stopProxyService = (proxyIndex: number, proxyName: string) =>
    runAction(`停止 ${proxyName || `#${proxyIndex + 1}`}`, async () => {
      const service = await invoke<ServiceStatus>("stop_proxy_service", {
        request: { workspace_id: activeWorkspaceId, proxy_index: proxyIndex }
      });
      applyServiceStatus(service, activeWorkspaceId);
      setNotice(`${proxyName || `#${proxyIndex + 1}`} 已停止`);
    });

  const restartProxyService = (proxyIndex: number, proxyName: string) =>
    runAction(`重启 ${proxyName || `#${proxyIndex + 1}`}`, async () => {
      const service = await invoke<ServiceStatus>("restart_proxy_service", {
        request: { workspace_id: activeWorkspaceId, proxy_index: proxyIndex }
      });
      applyServiceStatus(service, activeWorkspaceId);
      setNotice(`${proxyName || `#${proxyIndex + 1}`} 已重启`);
    });

  const clearWorkspaceLogs = () =>
    runAction("清空日志中", async () => {
      const service = await invoke<ServiceStatus>("clear_workspace_logs", {
        request: { workspace_id: activeWorkspaceId }
      });
      applyServiceStatus(service, activeWorkspaceId);
      setNotice(`${dashboard?.current_workspace.name ?? "当前工作区"} 的运行日志已清空`);
    });

  const setLogCleanupInterval = (intervalMinutes: number) =>
    runAction("保存日志清理设置中", async () => {
      const savedInterval = await invoke<number>("set_log_cleanup_interval", {
        request: {
          workspace_id: activeWorkspaceId,
          interval_minutes: intervalMinutes
        }
      });
      setDashboard((current) =>
        current && current.current_workspace.id === activeWorkspaceId
          ? { ...current, log_cleanup_interval_minutes: savedInterval }
          : current
      );
      setNotice(
        savedInterval === 0
          ? "已关闭运行日志定时清空"
          : `运行日志将${formatLogCleanupInterval(savedInterval)}自动清空`
      );
    });

  const canLeaveWorkspace = () =>
    !configDirty || window.confirm("当前工作区有未保存的配置，继续后将丢弃这些修改。是否继续？");

  const restoreSavedConfigDraft = () => {
    if (!dashboard) {
      return;
    }
    setConfigText(dashboard.config_text);
    setQuickConfig(toQuickConfig(dashboard.config_summary));
    setDirtyConfigTab(null);
  };

  const switchViewTab = (nextTab: "quick" | "editor" | "logs") => {
    const switchingEditors =
      nextTab !== "logs" && dirtyConfigTab !== null && dirtyConfigTab !== nextTab;
    if (
      switchingEditors &&
      !window.confirm("另一种配置编辑方式中有未保存修改，切换后将丢弃这些修改。是否继续？")
    ) {
      return;
    }
    if (switchingEditors) {
      restoreSavedConfigDraft();
    }
    setTab(nextTab);
  };

  const refreshWorkspace = () => {
    if (canLeaveWorkspace()) {
      void loadDashboard(activeWorkspaceId);
    }
  };

  const switchWorkspace = (workspaceId: string) => {
    if (workspaceId === activeWorkspaceId || !canLeaveWorkspace()) {
      return;
    }
    void loadDashboard(workspaceId);
  };

  const createWorkspace = () => {
    if (!activeWorkspaceId || !canLeaveWorkspace()) {
      return;
    }
    void runAction("复制工作区中", async () => {
      const workspace = await invoke<WorkspaceRecord>("create_workspace", {
        request: { source_workspace_id: activeWorkspaceId }
      });
      await loadDashboard(workspace.id);
      setNotice(`${workspace.name} 已从当前工作区复制创建`);
    });
  };

  const beginWorkspaceRename = () => {
    if (!dashboard) {
      return;
    }
    setWorkspaceNameDraft(dashboard.current_workspace.name);
    setRenamingWorkspace(true);
  };

  const cancelWorkspaceRename = () => {
    setRenamingWorkspace(false);
    setWorkspaceNameDraft("");
  };

  const renameWorkspace = (event: React.FormEvent) => {
    event.preventDefault();
    if (!dashboard) {
      return;
    }
    const workspaceId = dashboard.current_workspace.id;
    void runAction("重命名工作区中", async () => {
      const workspace = await invoke<WorkspaceRecord>("rename_workspace", {
        request: { workspace_id: workspaceId, name: workspaceNameDraft }
      });
      setDashboard((current) =>
        current
          ? {
              ...current,
              current_workspace: workspace,
              workspaces: current.workspaces.map((item) =>
                item.id === workspace.id ? { ...item, name: workspace.name } : item
              )
            }
          : current
      );
      setRenamingWorkspace(false);
      setNotice(`工作区已重命名为 ${workspace.name}`);
    });
  };

  const deleteWorkspace = async () => {
    if (!dashboard || dashboard.workspaces.length <= 1) {
      return;
    }
    const workspace = dashboard.current_workspace;
    const dirtyWarning = configDirty ? "未保存的修改也会丢失。" : "";
    let confirmed = false;
    try {
      confirmed = await confirm(
        `确定删除“${workspace.name}”吗？\n\n${dirtyWarning}该工作区的配置和日志将被永久删除，此操作无法撤销。`,
        {
          title: "删除工作区",
          kind: "warning",
          okLabel: "删除",
          cancelLabel: "取消"
        }
      );
    } catch (err) {
      setWorkspaceError(readError(err));
      return;
    }
    if (!confirmed) {
      return;
    }

    void runAction("删除工作区中", async () => {
      const nextWorkspace = await invoke<WorkspaceRecord>("delete_workspace", {
        request: { workspace_id: workspace.id }
      });
      await loadDashboard(nextWorkspace.id);
      setNotice(`${workspace.name} 已删除`);
    });
  };

  const checkLatest = () =>
    runAction("检测更新中", async () => {
      const latest = await invoke<ReleaseInfo>("check_latest_release");
      setRelease(latest);
      setNotice(`检测到最新版本 ${latest.version}`);
    }, "runtime");

  const downloadRuntime = () => {
    if (!canLeaveWorkspace()) {
      return;
    }
    void runAction("下载导入中", async () => {
      if (!selectedAsset) {
        throw new Error("没有可下载的 Windows/macOS 版本包");
      }
      const runtimes = await invoke<RuntimeInfo[]>("download_and_import_runtime", {
        request: {
          download_url: selectedAsset.download_url,
          archive_name: selectedAsset.name
        }
      });
      setDashboard((current) => (current ? { ...current, runtimes } : current));
      await loadDashboard();
      setNotice(`${selectedAsset.name} 已下载并导入`);
    }, "runtime");
  };

  const setCurrentRuntime = (id: string) => {
    if (!canLeaveWorkspace()) {
      return;
    }
    void runAction("切换版本中", async () => {
      const runtimes = await invoke<RuntimeInfo[]>("set_current_runtime", {
        request: { id }
      });
      setDashboard((current) => (current ? { ...current, runtimes } : current));
      await loadDashboard();
      setNotice("当前版本已切换");
    }, "runtime");
  };

  const deleteRuntime = (id: string) =>
    runAction("删除版本中", async () => {
      const runtimes = await invoke<RuntimeInfo[]>("delete_runtime", { id });
      setDashboard((current) => (current ? { ...current, runtimes } : current));
      setNotice("版本已删除");
    }, "runtime");

  const openLocalPath = (path: string) =>
    runAction("打开路径中", async () => {
      await invoke("open_path", { path });
      setNotice("已请求系统打开路径");
    });

  const openContainingFolder = (path: string) =>
    runAction("打开所在目录中", async () => {
      await invoke("open_containing_folder", { path });
      setNotice("已请求系统打开文件所在目录");
    });

  const openMappedUrl = (url: string) =>
    runAction("打开映射地址中", async () => {
      await invoke("open_path", { path: url });
      setNotice("已请求浏览器打开映射地址");
    });

  if (!dashboard) {
    return (
      <main className="boot-screen">
        <Loader2 className="spin" size={34} />
        <p>{busy ?? "正在启动"}...</p>
      </main>
    );
  }

  return (
    <main className="app-frame">
      <aside className="workspace-sidebar">
        <div className="sidebar-brand">
          <span className="brand-mark">
            <FileCode2 size={19} />
          </span>
          <div>
            <strong>FRPC Manager</strong>
            <small>隧道工作台</small>
          </div>
        </div>

        <div className="sidebar-section-heading">
          <span>工作区</span>
          <button className="sidebar-icon-button" onClick={createWorkspace} disabled={Boolean(busy)} title="复制当前配置新建工作区">
            <CopyPlus size={17} />
          </button>
        </div>

        <nav className="workspace-list" aria-label="工作区切换">
          {dashboard.workspaces.map((workspace) => {
            const active = workspace.id === dashboard.current_workspace.id;
            return (
              <div className={`workspace-list-item ${active ? "active" : ""}`} key={workspace.id}>
                <button
                  className="workspace-select"
                  aria-current={active ? "page" : undefined}
                  title={workspace.name}
                  onClick={() => switchWorkspace(workspace.id)}
                  disabled={Boolean(busy)}
                >
                  <span className={`workspace-status-dot ${workspace.running_count > 0 ? "running" : ""}`} />
                  <span className="workspace-list-copy">
                    <strong>{workspace.name}</strong>
                    <small>{workspace.running_count > 0 ? `${workspace.running_count} 个服务运行中` : "全部已停止"}</small>
                  </span>
                </button>
                {active && (
                  <button className="workspace-rename-button" onClick={beginWorkspaceRename} disabled={Boolean(busy)} title="重命名当前工作区">
                    <Pencil size={14} />
                  </button>
                )}
              </div>
            );
          })}
        </nav>

        <div className="sidebar-runtime">
          <div>
            <span>共享运行时</span>
            <strong>{dashboard.current_runtime?.version ?? "未安装"}</strong>
            <small>{dashboard.current_runtime?.platform ?? dashboard.current_platform}</small>
          </div>
        </div>
      </aside>

      <section className="app-main">
        <header className="workspace-context-bar">
          <div className="context-workspace">
            <span>当前工作区</span>
            {renamingWorkspace ? (
              <form className="workspace-rename-form" onSubmit={renameWorkspace}>
                <input
                  value={workspaceNameDraft}
                  onChange={(event) => setWorkspaceNameDraft(event.target.value)}
                  onKeyDown={(event) => event.key === "Escape" && cancelWorkspaceRename()}
                  maxLength={32}
                  autoFocus
                  aria-label="工作区名称"
                />
                <button className="icon-command confirm" type="submit" disabled={Boolean(busy) || !workspaceNameDraft.trim()} title="保存名称">
                  <Check size={16} />
                </button>
                <button className="icon-command" type="button" onClick={cancelWorkspaceRename} title="取消改名">
                  <X size={16} />
                </button>
              </form>
            ) : (
              <div className="workspace-context-name">
                <h1>{dashboard.current_workspace.name}</h1>
                <button className="icon-command" onClick={beginWorkspaceRename} disabled={Boolean(busy)} title="重命名工作区">
                  <Pencil size={15} />
                </button>
              </div>
            )}
          </div>

          <div className="context-stat server-context-stat">
            <span>服务器</span>
            <strong className="server-endpoint">
              {dashboard.config_summary.server_addr
                ? `${dashboard.config_summary.server_addr}:${dashboard.config_summary.server_port ?? "-"}`
                : "未配置"}
            </strong>
          </div>
          <div className="context-stat">
            <span>工作区状态</span>
            <strong className={dashboard.service.running ? "status-running" : ""}>
              {dashboard.service.running ? `${dashboard.service.running_count} 个运行中` : "已停止"}
            </strong>
            <small>共 {dashboard.config_summary.proxies.length} 个代理</small>
          </div>

          <div className="context-actions">
            <button
              className="primary"
              onClick={startService}
              disabled={
                Boolean(busy) ||
                dashboard.service.running_count >= dashboard.config_summary.proxies.length ||
                !dashboard.current_runtime ||
                dashboard.config_summary.proxies.length === 0
              }
            >
              <Play size={17} />
              启动工作区
            </button>
            <button className="danger-outline" onClick={stopService} disabled={Boolean(busy) || !dashboard.service.running}>
              <CircleStop size={17} />
              停止
            </button>
            <button className="icon-command" onClick={refreshWorkspace} disabled={Boolean(busy)} title="刷新">
              <RefreshCw size={17} />
            </button>
            <button className="icon-command" onClick={() => openLocalPath(dashboard.paths.workspace_dir)} title="打开工作区目录">
              <FolderOpen size={17} />
            </button>
            <button
              className="icon-command delete-command"
              onClick={deleteWorkspace}
              disabled={Boolean(busy) || dashboard.workspaces.length <= 1}
              title={dashboard.workspaces.length <= 1 ? "至少保留一个工作区" : "删除当前工作区"}
            >
              <Trash2 size={17} />
            </button>
          </div>
        </header>

        {(!showRuntimeStatus || workspaceError) && (
          <StatusBar
            notice={notice}
            error={workspaceError}
            busy={showRuntimeStatus ? null : busy}
          />
        )}

        <div className="view-tabs" role="tablist" aria-label="工作区视图">
          <button role="tab" aria-selected={tab === "quick"} onClick={() => switchViewTab("quick")}>
            <Wand2 size={16} />
            快捷配置
          </button>
          <button role="tab" aria-selected={tab === "editor"} onClick={() => switchViewTab("editor")}>
            <FileCode2 size={16} />
            TOML
          </button>
          <button role="tab" aria-selected={tab === "logs"} onClick={() => switchViewTab("logs")}>
            <Settings2 size={16} />
            运行日志
          </button>
        </div>

        <div className="workspace-content">
          <section className="surface-panel config-panel">
            <div className="section-heading">
              <div>
                <h2>{tab === "quick" ? "服务器与代理配置" : tab === "editor" ? "原始 TOML" : "工作区日志"}</h2>
                <p>{dashboard.paths.config_path}</p>
              </div>
              {tab === "logs" ? (
                <LogToolbar
                  intervalMinutes={dashboard.log_cleanup_interval_minutes}
                  hasLogs={dashboard.service.logs.length > 0}
                  disabled={Boolean(busy)}
                  onClear={clearWorkspaceLogs}
                  onIntervalChange={setLogCleanupInterval}
                />
              ) : (
                configDirty && <span className="dirty-indicator">有未保存修改</span>
              )}
            </div>

            {tab === "quick" && (
              <QuickEditor
                value={quickConfig}
                onChange={(value) => {
                  setQuickConfig(value);
                  setDirtyConfigTab("quick");
                }}
                onSave={saveQuickConfig}
                disabled={Boolean(busy)}
              />
            )}

            {tab === "editor" && (
              <div className="editor-wrap">
                <textarea
                  spellCheck={false}
                  value={configText}
                  onChange={(event) => {
                    setConfigText(event.target.value);
                    setDirtyConfigTab("editor");
                  }}
                />
                <div className="editor-actions">
                  <button className="ghost" onClick={resetConfig} disabled={Boolean(busy)}>
                    <RotateCcw size={17} />
                    恢复默认
                  </button>
                  <button className="primary" onClick={saveConfig} disabled={Boolean(busy)}>
                    <Save size={17} />
                    保存配置
                  </button>
                </div>
              </div>
            )}

            {tab === "logs" && <LogViewer logs={dashboard.service.logs} />}
          </section>

          <section className="surface-panel services-panel">
            <div className="section-heading">
              <div>
                <h2>工作区服务</h2>
                <p>每个代理可单独启动、重启或停止</p>
              </div>
              <span className={`pill ${dashboard.service.running ? "green" : "gray"}`}>
                {dashboard.service.running ? `${dashboard.service.running_count} 运行中` : "全部停止"}
              </span>
            </div>
            <div className="service-list">
              {dashboard.config_summary.proxies.map((proxy, index) => {
                const mappedUrl = buildMappedUrl(dashboard.config_summary.server_addr, proxy);
                const service = findProxyService(dashboard.service, index);
                const running = Boolean(service?.running);
                return (
                  <div className="service-row" key={`${proxy.name}-${index}`}>
                    <div className="service-main">
                      <span className={`service-dot ${running ? "green" : "gray"}`} />
                      <div>
                        <strong>{proxy.name || `proxy-${index + 1}`}</strong>
                        <code>{proxy.local_ip}:{proxy.local_port ?? "-"} → {proxy.remote_port ?? "-"}</code>
                      </div>
                    </div>
                    <span className="proxy-type">{proxy.proxy_type}</span>
                    <span className={`pill compact ${running ? "green" : "gray"}`}>
                      {running ? `PID ${service?.pid ?? "-"}` : "已停止"}
                    </span>
                    <button className="mapped-url-button" title={mappedUrl || "未配置映射地址"} onClick={() => mappedUrl && openMappedUrl(mappedUrl)} disabled={!mappedUrl}>
                      {mappedUrl || "未配置映射地址"}
                    </button>
                    <div className="row-actions">
                      <button className="soft icon-only" title="启动服务" onClick={() => startProxyService(index, proxy.name)} disabled={Boolean(busy) || running || !dashboard.current_runtime}>
                        <Play size={16} />
                      </button>
                      <button className="soft icon-only" title="重启服务" onClick={() => restartProxyService(index, proxy.name)} disabled={Boolean(busy) || !dashboard.current_runtime}>
                        <RefreshCw size={16} />
                      </button>
                      <button className="delete icon-only" title="停止服务" onClick={() => stopProxyService(index, proxy.name)} disabled={Boolean(busy) || !running}>
                        <CircleStop size={16} />
                      </button>
                    </div>
                  </div>
                );
              })}
              {dashboard.config_summary.proxies.length === 0 && <p className="empty">暂无代理配置</p>}
            </div>
          </section>

          <section className="surface-panel resource-panel">
            <div className="section-heading">
              <div>
                <h2>资源与路径</h2>
                <p>运行时全局共享，配置按工作区独立保存</p>
              </div>
            </div>
            <div className="resource-strip">
              <PathRow label="工作区目录" value={dashboard.paths.workspace_dir} onOpen={openLocalPath} />
              <PathRow label="配置文件" value={dashboard.paths.config_path} onOpen={openContainingFolder} />
              <PathRow label="当前二进制" value={dashboard.paths.current_binary ?? "未选择"} onOpen={openContainingFolder} />
            </div>
          </section>

          <section className="surface-panel versions-panel">
            <div className="section-heading">
              <div>
                <h2>共享运行时 <span>{dashboard.runtimes.length}</span></h2>
                <p>{release ? `GitHub 最新版本 ${release.version}` : "所有工作区共用已安装的 FRPC 版本"}</p>
              </div>
              <div className="button-row">
                {release && (
                  <button className="ghost" onClick={() => openLocalPath(release.html_url)} title="打开源码地址">
                    <ExternalLink size={17} />
                  </button>
                )}
                <button className="ghost" onClick={checkLatest} disabled={Boolean(busy)}>
                  <RefreshCw size={17} />
                  检测更新
                </button>
                <button className="primary muted" onClick={downloadRuntime} disabled={Boolean(busy) || !selectedAsset}>
                  <Download size={17} />
                  下载导入
                </button>
              </div>
            </div>

            {showRuntimeStatus && (
              <StatusBar
                notice={notice}
                error={runtimeError}
                busy={runtimeBusy ? busy : null}
                className="runtime-status-bar"
              />
            )}

            <div className="table">
              <div className="table-head">
                <span>版本</span>
                <span>平台</span>
                <span>导入时间</span>
                <span>包文件</span>
                <span>状态</span>
                <span>操作</span>
              </div>
              {dashboard.runtimes.map((runtime) => (
                <div className="table-row" key={runtime.id}>
                  <strong>{runtime.version}</strong>
                  <span>{runtime.platform}</span>
                  <span>{formatTime(runtime.imported_at)}</span>
                  <span className="archive-name" title={runtime.archive_name}>{runtime.archive_name}</span>
                  <span>
                    {runtime.is_current ? (
                      <span className="pill green">
                        <CheckCircle2 size={16} />
                        当前
                      </span>
                    ) : runtime.is_in_use ? (
                      <span className="pill gray" title="停止使用此版本的服务后才可删除">
                        服务使用中
                      </span>
                    ) : (
                      <button className="soft" onClick={() => setCurrentRuntime(runtime.id)} disabled={Boolean(busy)}>设为当前</button>
                    )}
                  </span>
                  <span>
                    <button className="delete" onClick={() => deleteRuntime(runtime.id)} disabled={Boolean(busy) || !runtime.can_delete}>
                      <Trash2 size={16} />
                      删除
                    </button>
                  </span>
                </div>
              ))}
            </div>
          </section>
        </div>
      </section>
    </main>
  );
}

function QuickEditor({
  value,
  onChange,
  onSave,
  disabled
}: {
  value: QuickConfig;
  onChange: (value: QuickConfig) => void;
  onSave: () => void;
  disabled: boolean;
}) {
  const setValue = <K extends keyof Omit<QuickConfig, "proxies">>(key: K, nextValue: QuickConfig[K]) => {
    onChange({ ...value, [key]: nextValue });
  };

  const updateProxy = <K extends keyof QuickProxy>(index: number, key: K, nextValue: QuickProxy[K]) => {
    const proxies = value.proxies.map((proxy, proxyIndex) =>
      proxyIndex === index ? { ...proxy, [key]: nextValue } : proxy
    );
    onChange({ ...value, proxies });
  };

  const addProxy = () => {
    const nextIndex = value.proxies.length + 1;
    onChange({
      ...value,
      proxies: [
        ...value.proxies,
        {
          ...defaultQuickProxy,
          name: `proxy-${nextIndex}`,
          local_port: 8000 + nextIndex,
          remote_port: 18000 + nextIndex
        }
      ]
    });
  };

  const removeProxy = (index: number) => {
    if (value.proxies.length <= 1) {
      return;
    }
    onChange({
      ...value,
      proxies: value.proxies.filter((_, proxyIndex) => proxyIndex !== index)
    });
  };

  return (
    <div className="quick-editor">
      <div className="server-grid">
        <Field label="服务器地址">
          <input value={value.server_addr} onChange={(event) => setValue("server_addr", event.target.value)} />
        </Field>
        <Field label="服务器端口">
          <input
            type="number"
            min={1}
            max={65535}
            value={value.server_port}
            onChange={(event) => setValue("server_port", Number(event.target.value))}
          />
        </Field>
        <Field label="认证方式">
          <select value={value.auth_method} onChange={(event) => setValue("auth_method", event.target.value)}>
            <option value="token">token</option>
            <option value="oidc">oidc</option>
          </select>
        </Field>
        <Field label="认证 Token">
          <input value={value.auth_token} onChange={(event) => setValue("auth_token", event.target.value)} />
        </Field>
      </div>

      <div className="proxy-editor-head">
        <div>
          <h3>代理列表</h3>
          <p>每一行会生成一个 [[proxies]] 配置块</p>
        </div>
        <button className="ghost" onClick={addProxy} disabled={disabled}>
          <Plus size={18} />
          添加代理
        </button>
      </div>

      <div className="proxy-editor-list">
        {value.proxies.map((proxy, index) => (
          <div className="proxy-editor-row" key={index}>
            <span className="proxy-index">#{index + 1}</span>
            <Field label="代理名称" className="proxy-name-field">
              <input value={proxy.name} onChange={(event) => updateProxy(index, "name", event.target.value)} />
            </Field>
            <Field label="代理类型" className="proxy-type-field">
              <select value={proxy.proxy_type} onChange={(event) => updateProxy(index, "proxy_type", event.target.value)}>
                <option value="tcp">tcp</option>
                <option value="udp">udp</option>
                <option value="http">http</option>
                <option value="https">https</option>
              </select>
            </Field>
            <Field label="本地 IP" className="proxy-local-ip-field">
              <input
                value={proxy.local_ip}
                placeholder="127.0.0.1"
                inputMode="decimal"
                spellCheck={false}
                onChange={(event) => updateProxy(index, "local_ip", event.target.value)}
              />
            </Field>
            <Field label="本地端口" className="proxy-local-port-field">
              <input
                type="number"
                min={1}
                max={65535}
                value={proxy.local_port}
                onChange={(event) => updateProxy(index, "local_port", Number(event.target.value))}
              />
            </Field>
            <Field label="远程端口" className="proxy-remote-port-field">
              <input
                type="number"
                min={1}
                max={65535}
                value={proxy.remote_port}
                onChange={(event) => updateProxy(index, "remote_port", Number(event.target.value))}
              />
            </Field>
            <button
              className="delete icon-only proxy-delete-button"
              title="删除代理"
              onClick={() => removeProxy(index)}
              disabled={disabled || value.proxies.length <= 1}
            >
              <Trash2 size={16} />
            </button>
          </div>
        ))}
      </div>

      <div className="quick-footer">
        <button className="primary" onClick={onSave} disabled={disabled}>
          <Save size={18} />
          写入配置
        </button>
      </div>
    </div>
  );
}

function Field({
  label,
  children,
  className = ""
}: {
  label: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <label className={`field ${className}`.trim()}>
      <span>{label}</span>
      {children}
    </label>
  );
}

function StatusBar({
  notice,
  error,
  busy,
  className = ""
}: {
  notice: string;
  error: string;
  busy: string | null;
  className?: string;
}) {
  return (
    <div className={`status-bar ${error ? "has-error" : ""} ${className}`.trim()}>
      <span>{error || notice}</span>
      {busy && (
        <span className="busy">
          <Loader2 className="spin" size={16} />
          {busy}
        </span>
      )}
    </div>
  );
}

function PathRow({
  label,
  value,
  onOpen
}: {
  label: string;
  value: string;
  onOpen: (path: string) => void;
}) {
  return (
    <div className="path-row">
      <div>
        <span>{label}</span>
        <code>{value}</code>
      </div>
      <button className="ghost icon-text" onClick={() => onOpen(value)} disabled={!value || value === "未选择"}>
        <FolderOpen size={17} />
        打开
      </button>
    </div>
  );
}

function LogViewer({ logs }: { logs: string[] }) {
  return (
    <pre className="log-viewer">
      {logs.length > 0 ? logs.join("\n") : "暂无运行日志。启动 frpc 后会在这里显示 stdout/stderr。"}
    </pre>
  );
}

function LogToolbar({
  intervalMinutes,
  hasLogs,
  disabled,
  onClear,
  onIntervalChange
}: {
  intervalMinutes: number;
  hasLogs: boolean;
  disabled: boolean;
  onClear: () => void;
  onIntervalChange: (intervalMinutes: number) => void;
}) {
  return (
    <div className="log-toolbar">
      <label className="log-cleanup-control" title="设置当前工作区运行日志的自动清空周期">
        <Clock3 size={16} />
        <select
          aria-label="定时清空日志"
          value={intervalMinutes}
          onChange={(event) => onIntervalChange(Number(event.target.value))}
          disabled={disabled}
        >
          {logCleanupOptions.map((option) => (
            <option value={option.value} key={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      </label>
      <button className="ghost log-clear-button" onClick={onClear} disabled={disabled || !hasLogs}>
        <Trash2 size={16} />
        清空日志
      </button>
    </div>
  );
}

function formatLogCleanupInterval(intervalMinutes: number) {
  return logCleanupOptions.find((option) => option.value === intervalMinutes)?.label ?? "按设定周期";
}

function toQuickConfig(summary: ConfigSummary): QuickConfig {
  return {
    server_addr: summary.server_addr || defaultQuickConfig.server_addr,
    server_port: summary.server_port ?? defaultQuickConfig.server_port,
    auth_method: summary.auth_method || defaultQuickConfig.auth_method,
    auth_token: summary.auth_token ?? "",
    proxies:
      summary.proxies.length > 0
        ? summary.proxies.map((proxy, index) => ({
            name: proxy.name || `proxy-${index + 1}`,
            proxy_type: proxy.proxy_type || defaultQuickProxy.proxy_type,
            local_ip: proxy.local_ip || defaultQuickProxy.local_ip,
            local_port: proxy.local_port ?? defaultQuickProxy.local_port,
            remote_port: proxy.remote_port ?? defaultQuickProxy.remote_port
          }))
        : [defaultQuickProxy]
  };
}

function parseConfigTextSummary(configText: string, fallback: ConfigSummary): ConfigSummary {
  const parsed: ConfigSummary = {
    server_addr: fallback.server_addr || "",
    server_port: fallback.server_port ?? null,
    auth_method: fallback.auth_method || "token",
    auth_token: fallback.auth_token || "",
    proxies: fallback.proxies ?? []
  };
  const proxies: ProxySummary[] = [];
  let currentProxy: ProxySummary | null = null;
  let section: "root" | "auth" | "proxy" = "root";

  for (const rawLine of configText.split(/\r?\n/)) {
    const line = stripTomlComment(rawLine).trim();
    if (!line) {
      continue;
    }

    if (line === "[[proxies]]") {
      currentProxy = {
        name: "",
        proxy_type: "tcp",
        local_ip: "127.0.0.1",
        local_port: null,
        remote_port: null
      };
      proxies.push(currentProxy);
      section = "proxy";
      continue;
    }

    if (line === "[auth]") {
      section = "auth";
      continue;
    }

    if (line.startsWith("[") && line.endsWith("]")) {
      section = "root";
      currentProxy = null;
      continue;
    }

    const equalIndex = line.indexOf("=");
    if (equalIndex < 0) {
      continue;
    }

    const key = line.slice(0, equalIndex).trim();
    const value = parseTomlScalar(line.slice(equalIndex + 1).trim());
    const scopedKey = section === "auth" && !key.includes(".") ? `auth.${key}` : key;

    if (section === "proxy" && currentProxy) {
      applyProxyField(currentProxy, key, value);
    } else {
      applyRootField(parsed, scopedKey, value);
    }
  }

  if (proxies.length > 0) {
    parsed.proxies = proxies;
  }

  return parsed;
}

function applyRootField(summary: ConfigSummary, key: string, value: string) {
  switch (key) {
    case "serverAddr":
      summary.server_addr = value;
      break;
    case "serverPort":
      summary.server_port = toPort(value);
      break;
    case "auth.method":
      summary.auth_method = value || "token";
      break;
    case "auth.token":
      summary.auth_token = value;
      break;
  }
}

function applyProxyField(proxy: ProxySummary, key: string, value: string) {
  switch (key) {
    case "name":
      proxy.name = value;
      break;
    case "type":
      proxy.proxy_type = value || "tcp";
      break;
    case "localIP":
      proxy.local_ip = value || "127.0.0.1";
      break;
    case "localPort":
      proxy.local_port = toPort(value);
      break;
    case "remotePort":
      proxy.remote_port = toPort(value);
      break;
  }
}

function stripTomlComment(line: string) {
  let inString = false;
  let escaped = false;
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (char === "\\") {
      escaped = true;
      continue;
    }
    if (char === '"') {
      inString = !inString;
      continue;
    }
    if (char === "#" && !inString) {
      return line.slice(0, index);
    }
  }
  return line;
}

function parseTomlScalar(value: string) {
  const trimmed = value.trim();
  if (trimmed.startsWith('"') && trimmed.endsWith('"')) {
    return trimmed.slice(1, -1).replace(/\\"/g, '"').replace(/\\\\/g, "\\");
  }
  return trimmed;
}

function toPort(value: string) {
  const port = Number(value);
  return Number.isFinite(port) && port > 0 ? port : null;
}

function formatTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit"
  }).format(date);
}

function buildMappedUrl(serverAddr: string, proxy: ProxySummary) {
  if (!serverAddr || !proxy.remote_port) {
    return "";
  }
  const scheme = proxy.proxy_type === "https" ? "https" : "http";
  const host = normalizeUrlHost(serverAddr);
  return host ? `${scheme}://${host}:${proxy.remote_port}` : "";
}

function findProxyService(service: ServiceStatus, proxyIndex: number) {
  return service.services.find((item) => item.proxy_index === proxyIndex);
}

function normalizeUrlHost(value: string) {
  const host = value
    .trim()
    .replace(/^https?:\/\//i, "")
    .replace(/\/.*$/, "");
  if (!host) {
    return "";
  }
  return host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
}

function readError(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === "string") {
    return error;
  }
  return "操作失败，请检查运行日志";
}

createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
