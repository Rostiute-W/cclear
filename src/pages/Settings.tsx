import { useEffect, useState, type FormEvent } from "react";
import { aiSettingsGet, aiSettingsSave } from "../api";
import { AlertBanner, StatusBadge } from "../components/Ui";
import type { AiSettings } from "../types";

export function SettingsPage() {
  const [settings, setSettings] = useState<AiSettings | null>(null);
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    aiSettingsGet()
      .then((loaded) => {
        if (!active) {
          return;
        }
        setSettings(loaded);
        setBaseUrl(loaded.baseUrl ?? "");
        setModel(loaded.model ?? "");
        setApiKey("");
        setClearKey(false);
      })
      .catch((reason) => {
        if (active) {
          setError(String(reason));
        }
      })
      .finally(() => {
        if (active) {
          setLoading(false);
        }
      });
    return () => {
      active = false;
    };
  }, []);

  const hasKey = settings?.hasKey === true;

  const onClearKey = () => {
    setApiKey("");
    setClearKey(true);
    setMessage(null);
    setError(null);
  };

  const onSubmit = (event: FormEvent) => {
    event.preventDefault();
    setSaving(true);
    setError(null);
    setMessage(null);

    const keyPayload = apiKey !== "" ? apiKey : clearKey ? "" : null;

    aiSettingsSave(baseUrl.trim(), model.trim(), keyPayload)
      .then((saved) => {
        setSettings(saved);
        setBaseUrl(saved.baseUrl ?? "");
        setModel(saved.model ?? "");
        setApiKey("");
        setClearKey(false);
        setMessage("已保存 AI 咨询设置");
      })
      .catch((reason) => setError(String(reason)))
      .finally(() => setSaving(false));
  };

  return (
    <section className="page-section">
      <div className="page-header">
        <div>
          <h2 className="page-title">设置</h2>
          <p className="page-subtitle">配置可选的 AI 咨询接口与本地偏好</p>
        </div>
      </div>

      {error && <AlertBanner tone="danger">{error}</AlertBanner>}
      {message && <AlertBanner tone="success">{message}</AlertBanner>}

      <div className="card">
        <div className="card-header">
          <div className="card-title-group">
            <h3 className="card-title">AI 咨询</h3>
            {hasKey && !clearKey ? (
              <StatusBadge tone="safe">已保存 API Key</StatusBadge>
            ) : (
              <StatusBadge tone="neutral">未保存 API Key</StatusBadge>
            )}
          </div>
        </div>

        <p className="card-lead">
          {
            "只在你点击‘问 AI’时才联网，请求只发往上面填写的地址。发送前会把用户目录替换为 %USERPROFILE%、把用户名替换为 <用户名>。API Key 用 Windows 加密保存，只有当前 Windows 账号能解开。AI 的回答仅供参考，不会自动执行任何清理。"
          }
        </p>

        <form className="settings-form" onSubmit={onSubmit}>
          <div className="settings-row">
            <label className="field-label" htmlFor="ai-base-url">
              服务地址
            </label>
            <input
              id="ai-base-url"
              type="text"
              className="text-input"
              value={baseUrl}
              disabled={loading || saving}
              placeholder="https://api.deepseek.com/v1"
              onChange={(event) => setBaseUrl(event.target.value)}
            />
          </div>

          <div className="settings-row">
            <label className="field-label" htmlFor="ai-model">
              模型名
            </label>
            <input
              id="ai-model"
              type="text"
              className="text-input"
              value={model}
              disabled={loading || saving}
              placeholder="deepseek-chat"
              onChange={(event) => setModel(event.target.value)}
            />
          </div>

          <div className="settings-row">
            <label className="field-label" htmlFor="ai-api-key">
              API Key
            </label>
            <div>
              <div className="settings-key-wrap">
                <input
                  id="ai-api-key"
                  type="password"
                  className="text-input"
                  value={apiKey}
                  disabled={loading || saving}
                  placeholder={
                    hasKey && !clearKey
                      ? "已保存（留空表示不修改）"
                      : "选填，本地模型可留空"
                  }
                  onChange={(event) => {
                    const next = event.target.value;
                    setApiKey(next);
                    if (next !== "") {
                      setClearKey(false);
                    }
                  }}
                />
                <button
                  type="button"
                  className="btn btn-subtle"
                  disabled={loading || saving || (!hasKey && apiKey === "") || clearKey}
                  onClick={onClearKey}
                >
                  清除 Key
                </button>
              </div>
              {clearKey && (
                <p className="muted" style={{ margin: "6px 0 0" }}>
                  已标记清除：点击“保存”后将删除本地保存的 API Key。
                </p>
              )}
            </div>
          </div>

          <div className="settings-actions">
            <button
              type="submit"
              className="btn btn-primary"
              disabled={loading || saving}
            >
              {saving ? "保存中…" : "保存"}
            </button>
          </div>
        </form>

        <div className="summary-box settings-providers">
          <p className="muted" style={{ margin: 0 }}>
            支持所有 OpenAI 兼容接口（需使用 HTTPS，本机地址可使用 HTTP）：
          </p>
          <p className="muted" style={{ margin: "6px 0 0" }}>
            DeepSeek、通义千问、Kimi、硅基流动、OpenRouter、本地 Ollama（http://localhost:11434/v1）
          </p>
        </div>
      </div>
    </section>
  );
}
