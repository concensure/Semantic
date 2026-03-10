import { useMemo, useState } from "react";
import type { SettingsPayload } from "./types";

const DEFAULT_BASE_URL = (import.meta as any).env?.VITE_SEMANTIC_BASE_URL || "<SEMANTIC_API_BASE_URL>";

export function App() {
  const [baseUrl, setBaseUrl] = useState(DEFAULT_BASE_URL);
  const [llmConfig, setLlmConfig] = useState("");
  const [llmRouting, setLlmRouting] = useState("");
  const [modelMetrics, setModelMetrics] = useState("{}");
  const [envFile, setEnvFile] = useState("");
  const [enableOllama, setEnableOllama] = useState(true);
  const [status, setStatus] = useState("");

  const ready = useMemo(() => llmConfig.length > 0 || llmRouting.length > 0, [llmConfig, llmRouting]);

  async function load() {
    try {
      const res = await fetch(`${baseUrl}/mcp_settings_ui`);
      const html = await res.text();
      setStatus("Loaded current settings from server UI. Paste into fields if needed.");
      if (html.includes("llm_config.toml")) {
        setStatus("Server-side UI is available. Use it or continue here.");
      }
    } catch (err) {
      setStatus(`Load failed: ${String(err)}`);
    }
  }

  async function save() {
    const payload: SettingsPayload = {
      llm_config: llmConfig,
      llm_routing: llmRouting,
      model_metrics: modelMetrics,
      env_file: envFile,
      enable_ollama: enableOllama,
    };

    try {
      const form = new URLSearchParams();
      form.set("llm_config", payload.llm_config);
      form.set("llm_routing", payload.llm_routing);
      form.set("model_metrics", payload.model_metrics);
      form.set("env_file", payload.env_file);
      if (payload.enable_ollama) {
        form.set("enable_ollama", "true");
      }

      const res = await fetch(`${baseUrl}/mcp_settings_update`, {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: form.toString(),
      });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      setStatus("Saved settings to server.");
    } catch (err) {
      setStatus(`Save failed: ${String(err)}`);
    }
  }

  return (
    <div className="page">
      <header>
        <h1>Semantic MCP Settings</h1>
        <p>Manage LLM providers, routing, and metrics with a local UI.</p>
      </header>

      <section className="row">
        <label>Semantic API Base URL</label>
        <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} />
        <div className="actions">
          <button onClick={load}>Check Server UI</button>
        </div>
      </section>

      <section>
        <div className="label">llm_config.toml</div>
        <textarea value={llmConfig} onChange={(e) => setLlmConfig(e.target.value)} rows={14} />
      </section>

      <section>
        <div className="label">llm_routing.toml</div>
        <textarea value={llmRouting} onChange={(e) => setLlmRouting(e.target.value)} rows={10} />
      </section>

      <section>
        <div className="label">model_metrics.json</div>
        <textarea value={modelMetrics} onChange={(e) => setModelMetrics(e.target.value)} rows={8} />
      </section>

      <section>
        <div className="label">.env (API keys)</div>
        <textarea value={envFile} onChange={(e) => setEnvFile(e.target.value)} rows={8} />
      </section>

      <section className="row">
        <label className="toggle">
          <input
            type="checkbox"
            checked={enableOllama}
            onChange={(e) => setEnableOllama(e.target.checked)}
          />
          Enable Ollama
        </label>
      </section>

      <section className="actions">
        <button disabled={!ready} onClick={save}>Save Settings</button>
        <span className="status">{status}</span>
      </section>
    </div>
  );
}
