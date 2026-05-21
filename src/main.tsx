import React, { useCallback, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  CheckCircle2,
  CircleStop,
  Download,
  ExternalLink,
  FileArchive,
  FileCode2,
  FolderOpen,
  Loader2,
  Plus,
  Play,
  RefreshCw,
  RotateCcw,
  Save,
  Settings2,
  Trash2,
  Wand2
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
  logs: string[];
};

type PathInfo = {
  app_data_dir: string;
  workspace_dir: string;
  config_path: string;
  runtimes_dir: string;
  current_binary: string | null;
};

type DashboardState = {
  paths: PathInfo;
  config_text: string;
  config_summary: ConfigSummary;
  runtimes: RuntimeInfo[];
  current_runtime: RuntimeInfo | null;
  service: ServiceStatus;
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

function App() {
  const [dashboard, setDashboard] = useState<DashboardState | null>(null);
  const [configText, setConfigText] = useState("");
  const [quickConfig, setQuickConfig] = useState<QuickConfig>(defaultQuickConfig);
  const [release, setRelease] = useState<ReleaseInfo | null>(null);
  const [busy, setBusy] = useState<string | null>("正在初始化");
  const [notice, setNotice] = useState<string>("正在读取本地工作区");
  const [error, setError] = useState<string>("");
  const [tab, setTab] = useState<"quick" | "editor" | "logs">("quick");

  const loadDashboard = useCallback(async () => {
    setBusy("刷新中");
    setError("");
    try {
      const data = await invoke<DashboardState>("get_dashboard_state");
      const summary = parseConfigTextSummary(data.config_text, data.config_summary);
      const syncedData = { ...data, config_summary: summary };
      setDashboard(syncedData);
      setConfigText(data.config_text);
      setQuickConfig(toQuickConfig(summary));
      setNotice("本地状态已刷新");
    } catch (err) {
      setError(readError(err));
    } finally {
      setBusy(null);
    }
  }, []);

  useEffect(() => {
    loadDashboard();
  }, [loadDashboard]);

  useEffect(() => {
    if (!dashboard?.service.running) {
      return;
    }
    const timer = window.setInterval(async () => {
      try {
        const service = await invoke<ServiceStatus>("get_service_status");
        setDashboard((current) => (current ? { ...current, service } : current));
      } catch (err) {
        setError(readError(err));
      }
    }, 1800);
    return () => window.clearInterval(timer);
  }, [dashboard?.service.running]);

  const selectedAsset = useMemo(() => {
    if (!release || !dashboard?.current_runtime) {
      return null;
    }
    return (
      release.assets.find((asset) => asset.platform === dashboard.current_runtime?.platform) ??
      release.assets[0] ??
      null
    );
  }, [dashboard?.current_runtime, release]);

  const runAction = async (label: string, action: () => Promise<void>) => {
    setBusy(label);
    setError("");
    try {
      await action();
    } catch (err) {
      setError(readError(err));
    } finally {
      setBusy(null);
    }
  };

  const saveConfig = () =>
    runAction("保存中", async () => {
      const summary = await invoke<ConfigSummary>("save_config", {
        request: { config_text: configText }
      });
      const syncedSummary = parseConfigTextSummary(configText, summary);
      setDashboard((current) => (current ? { ...current, config_summary: syncedSummary } : current));
      setQuickConfig(toQuickConfig(syncedSummary));
      setNotice("配置已保存并通过 TOML 校验");
    });

  const saveQuickConfig = () =>
    runAction("生成配置中", async () => {
      const [nextText, summary] = await invoke<[string, ConfigSummary]>("save_quick_config", {
        request: quickConfig
      });
      const syncedSummary = parseConfigTextSummary(nextText, summary);
      setConfigText(nextText);
      setDashboard((current) =>
        current
          ? {
              ...current,
              config_text: nextText,
              config_summary: syncedSummary
            }
          : current
      );
      setQuickConfig(toQuickConfig(syncedSummary));
      setNotice("快捷配置已写入 frpc.toml");
    });

  const resetConfig = () =>
    runAction("恢复中", async () => {
      const text = await invoke<string>("reset_config");
      setConfigText(text);
      await loadDashboard();
      setNotice("已恢复默认配置");
    });

  const startService = () =>
    runAction("启动中", async () => {
      const service = await invoke<ServiceStatus>("start_frpc");
      setDashboard((current) => (current ? { ...current, service } : current));
      setNotice("frpc 已启动");
    });

  const stopService = () =>
    runAction("停止中", async () => {
      const service = await invoke<ServiceStatus>("stop_frpc");
      setDashboard((current) => (current ? { ...current, service } : current));
      setNotice("frpc 已停止");
    });

  const checkLatest = () =>
    runAction("检测更新中", async () => {
      const latest = await invoke<ReleaseInfo>("check_latest_release");
      setRelease(latest);
      setNotice(`检测到最新版本 ${latest.version}`);
    });

  const importRuntime = () =>
    runAction("导入中", async () => {
      const selected = await open({
        multiple: false,
        filters: [
          {
            name: "FRP 版本包",
            extensions: ["gz", "tgz", "zip"]
          }
        ]
      });
      if (typeof selected !== "string") {
        setNotice("已取消导入");
        return;
      }
      const runtimes = await invoke<RuntimeInfo[]>("import_runtime", {
        request: { archive_path: selected }
      });
      setDashboard((current) => (current ? { ...current, runtimes } : current));
      await loadDashboard();
      setNotice("版本包已导入");
    });

  const downloadRuntime = () =>
    runAction("下载导入中", async () => {
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
    });

  const setCurrentRuntime = (id: string) =>
    runAction("切换版本中", async () => {
      const runtimes = await invoke<RuntimeInfo[]>("set_current_runtime", {
        request: { id }
      });
      setDashboard((current) => (current ? { ...current, runtimes } : current));
      await loadDashboard();
      setNotice("当前版本已切换");
    });

  const deleteRuntime = (id: string) =>
    runAction("删除版本中", async () => {
      const runtimes = await invoke<RuntimeInfo[]>("delete_runtime", { id });
      setDashboard((current) => (current ? { ...current, runtimes } : current));
      setNotice("版本已删除");
    });

  const openLocalPath = (path: string) =>
    runAction("打开路径中", async () => {
      await invoke("open_path", { path });
      setNotice("已请求系统打开路径");
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
      <main className="app-shell">
      <header className="hero">
        <div>
          <h1>FRPC Tunnel Manager</h1>
          <p>frpc 运行时、配置编辑与本地服务控制台</p>
        </div>
        <div className="hero-actions">
          <button className="ghost" onClick={loadDashboard} disabled={Boolean(busy)}>
            <RefreshCw size={18} />
            刷新
          </button>
          <button className="ghost" onClick={() => openLocalPath(dashboard.paths.workspace_dir)}>
            <FolderOpen size={18} />
            工作区
          </button>
          <button className="primary" onClick={importRuntime} disabled={Boolean(busy)}>
            <FileArchive size={18} />
            导入版本包
          </button>
        </div>
      </header>

      <StatusBar notice={notice} error={error} busy={busy} />

      <section className="metrics-grid">
        <MetricCard
          label="当前版本"
          value={dashboard.current_runtime?.version ?? "未安装"}
          detail={dashboard.current_runtime?.platform ?? "等待导入"}
        />
        <MetricCard
          label="服务状态"
          value={dashboard.service.running ? "运行中" : "已停止"}
          detail={dashboard.service.pid ? `PID ${dashboard.service.pid}` : "本地进程未启动"}
          tone={dashboard.service.running ? "good" : "idle"}
        />
        <MetricCard
          label="服务器"
          value={dashboard.config_summary.server_addr || "未配置"}
          detail={`端口 ${dashboard.config_summary.server_port ?? "-"}`}
        />
        <MetricCard
          label="代理数量"
          value={String(dashboard.config_summary.proxies.length)}
          detail={dashboard.config_summary.proxies[0]?.name ?? "尚无代理"}
        />
      </section>

      <section className="panel config-panel">
        <div className="panel-heading">
          <div>
            <h2>配置文件</h2>
            <p>{dashboard.paths.config_path}</p>
          </div>
          <div className="button-row">
            <button className="ghost" onClick={() => setTab("quick")} data-active={tab === "quick"}>
              <Wand2 size={18} />
              快捷编辑
            </button>
            <button className="ghost" onClick={() => setTab("editor")} data-active={tab === "editor"}>
              <FileCode2 size={18} />
              TOML
            </button>
            <button className="ghost" onClick={() => setTab("logs")} data-active={tab === "logs"}>
              <Settings2 size={18} />
              日志
            </button>
          </div>
        </div>

        {tab === "quick" && (
          <QuickEditor
            value={quickConfig}
            onChange={setQuickConfig}
            onSave={saveQuickConfig}
            disabled={Boolean(busy)}
          />
        )}

        {tab === "editor" && (
          <div className="editor-wrap">
            <textarea
              spellCheck={false}
              value={configText}
              onChange={(event) => setConfigText(event.target.value)}
            />
            <div className="editor-actions">
              <button className="ghost" onClick={resetConfig} disabled={Boolean(busy)}>
                <RotateCcw size={18} />
                恢复默认
              </button>
              <button className="primary" onClick={saveConfig} disabled={Boolean(busy)}>
                <Save size={18} />
                保存
              </button>
            </div>
          </div>
        )}

        {tab === "logs" && <LogViewer logs={dashboard.service.logs} />}
      </section>

      <section className="two-column">
        <div className="panel">
          <div className="panel-heading compact">
            <div>
              <h2>服务</h2>
              <p>{dashboard.service.running ? `PID ${dashboard.service.pid}` : "当前未运行"}</p>
            </div>
            <span className={`pill ${dashboard.service.running ? "green" : "gray"}`}>
              {dashboard.service.running ? "运行中" : "已停止"}
            </span>
          </div>
          <div className="service-actions">
            <button className="primary" onClick={startService} disabled={Boolean(busy) || dashboard.service.running}>
              <Play size={19} />
              启动
            </button>
            <button className="danger" onClick={stopService} disabled={Boolean(busy) || !dashboard.service.running}>
              <CircleStop size={19} />
              停止
            </button>
          </div>
          <div className="proxy-list">
            {dashboard.config_summary.proxies.map((proxy) => {
              const mappedUrl = buildMappedUrl(dashboard.config_summary.server_addr, proxy);
              return (
                <div className="proxy-row" key={proxy.name}>
                  <strong>{proxy.name}</strong>
                  <span>{proxy.proxy_type}</span>
                  <code>
                    {proxy.local_ip}:{proxy.local_port ?? "-"} → {proxy.remote_port ?? "-"}
                  </code>
                  <button
                    className="mapped-url-button"
                    title={mappedUrl || "未配置映射地址"}
                    onClick={() => mappedUrl && openMappedUrl(mappedUrl)}
                    disabled={!mappedUrl}
                  >
                    {mappedUrl || "未配置映射地址"}
                  </button>
                </div>
              );
            })}
            {dashboard.config_summary.proxies.length === 0 && <p className="empty">暂无代理配置</p>}
          </div>
        </div>

        <div className="panel">
          <div className="panel-heading compact">
            <div>
              <h2>路径</h2>
              <p>工作区与当前二进制</p>
            </div>
          </div>
          <PathRow label="运行工作区" value={dashboard.paths.workspace_dir} onOpen={openLocalPath} />
          <PathRow label="应用数据" value={dashboard.paths.app_data_dir} onOpen={openLocalPath} />
          <PathRow label="当前二进制" value={dashboard.paths.current_binary ?? "未选择"} onOpen={openLocalPath} />
        </div>
      </section>

      <section className="panel versions-panel">
        <div className="panel-heading">
          <div>
            <h2>
              已安装版本 <span>{dashboard.runtimes.length}</span>
            </h2>
            <p>{release ? `GitHub 最新版本 ${release.version}` : "可检测 GitHub Release 并导入 Windows/macOS 包"}</p>
          </div>
          <div className="button-row">
            {release && (
              <button className="ghost" onClick={() => openLocalPath(release.html_url)}>
                <ExternalLink size={18} />
                源码地址
              </button>
            )}
            <button className="ghost" onClick={checkLatest} disabled={Boolean(busy)}>
              <RefreshCw size={18} />
              检测更新
            </button>
            <button className="primary muted" onClick={downloadRuntime} disabled={Boolean(busy) || !selectedAsset}>
              <Download size={18} />
              下载导入
            </button>
          </div>
        </div>

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
              <span className="archive-name">{runtime.archive_name}</span>
              <span>
                {runtime.is_current ? (
                  <span className="pill green">
                    <CheckCircle2 size={16} />
                    当前
                  </span>
                ) : (
                  <button className="soft" onClick={() => setCurrentRuntime(runtime.id)} disabled={Boolean(busy)}>
                    设为当前
                  </button>
                )}
              </span>
              <span>
                <button
                  className="delete"
                  onClick={() => deleteRuntime(runtime.id)}
                  disabled={Boolean(busy) || !runtime.can_delete}
                >
                  <Trash2 size={16} />
                  删除
                </button>
              </span>
            </div>
          ))}
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

function StatusBar({ notice, error, busy }: { notice: string; error: string; busy: string | null }) {
  return (
    <div className={`status-bar ${error ? "has-error" : ""}`}>
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

function MetricCard({
  label,
  value,
  detail,
  tone = "idle"
}: {
  label: string;
  value: string;
  detail: string;
  tone?: "good" | "idle";
}) {
  return (
    <div className={`metric-card ${tone}`}>
      <span>{label}</span>
      <strong>{value}</strong>
      <p>{detail}</p>
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
