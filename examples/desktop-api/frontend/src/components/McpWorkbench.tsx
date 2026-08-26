import { useState } from "react";
import type React from "react";
import { desktop } from "../lib/desktop.js";

interface McpWorkbenchProps {
  pending: boolean;
  onRun: (label: string, fn: () => Promise<unknown>) => void;
}

function parseObject(value: string, label: string): Record<string, unknown> {
  const parsed: unknown = value.trim() ? JSON.parse(value) : {};
  if (!parsed || Array.isArray(parsed) || typeof parsed !== "object") {
    throw new Error(`${label} 必须是 JSON 对象`);
  }
  return parsed as Record<string, unknown>;
}

export function McpWorkbench({ pending, onRun }: McpWorkbenchProps): React.ReactElement {
  const [binding, setBinding] = useState("filesystem");
  const [tool, setTool] = useState("list_directory");
  const [argumentsJson, setArgumentsJson] = useState('{"path":"."}');
  const [resourceUri, setResourceUri] = useState("demo://workspace/readme");
  const [promptName, setPromptName] = useState("summarize");
  const [promptArguments, setPromptArguments] = useState("{}");
  const [completionValue, setCompletionValue] = useState("");
  const [clientId, setClientId] = useState("alex-desktop-api-demo");
  const [scopes, setScopes] = useState("openid,profile");
  const [redirectUri, setRedirectUri] = useState("http://127.0.0.1:8787/callback");
  const [oauthState, setOauthState] = useState("");
  const [oauthCode, setOauthCode] = useState("");
  const [oauthIssuer, setOauthIssuer] = useState("");
  const [inputId, setInputId] = useState("");
  const invoke = (label: string, fn: () => Promise<unknown>) => onRun(`MCP · ${label}`, fn);
  const parsedArguments = () => parseObject(argumentsJson, "工具参数");
  const parsedPromptArguments = () => parseObject(promptArguments, "Prompt 参数");

  const actions: Array<[string, () => Promise<unknown>]> = [
    ["连接列表", () => desktop.mcp.connections()],
    ["健康检查", () => desktop.mcp.health()],
    ["能力发现", () => desktop.mcp.discover(binding)],
    ["Ping", () => desktop.mcp.ping(binding)],
    ["工具列表", () => desktop.mcp.listTools(binding)],
    ["调用工具", () => desktop.mcp.callTool(binding, tool, parsedArguments())],
    ["交互式调用", async () => {
      const events = [];
      for await (const event of desktop.mcp.callToolInteractive(binding, tool, parsedArguments(), { creditBytes: 64 * 1024 })) {
        events.push(event);
        if (event.type === "inputRequired") {
          setInputId(event.inputId);
          break;
        }
      }
      return { events, note: "inputRequired 的 ID 已自动填入交互输入字段" };
    }],
    ["原生交互调用", async () => {
      const events = [];
      for await (const event of desktop.mcp.callToolNative(binding, tool, parsedArguments(), { creditBytes: 64 * 1024 })) {
        events.push(event);
      }
      return { events };
    }],
    ["Resources", () => desktop.mcp.listResources(binding)],
    ["读取 Resource", () => desktop.mcp.readResource(binding, resourceUri)],
    ["Prompts", () => desktop.mcp.listPrompts(binding)],
    ["获取 Prompt", () => desktop.mcp.getPrompt(binding, promptName, parsedPromptArguments())],
    ["Completion", () => desktop.mcp.complete(
      binding,
      { type: "ref/prompt", name: promptName },
      { name: "value", value: completionValue },
    )],
    ["审计报告", () => desktop.mcp.auditReport(100)],
    ["监听一次事件", async () => {
      const controller = new AbortController();
      const timer = window.setTimeout(() => controller.abort(), 15_000);
      try {
        for await (const event of desktop.mcp.listen(binding, {
          toolsListChanged: true,
          promptsListChanged: true,
          resourcesListChanged: true,
        }, { signal: controller.signal, creditBytes: 64 * 1024 })) {
          controller.abort();
          return event;
        }
        return { ended: true };
      } finally {
        window.clearTimeout(timer);
      }
    }],
    ["OAuth 自动授权", async () => {
      const result = await desktop.mcp.oauthAuthorize(binding, clientId, splitScopes(scopes));
      setOauthState(result.state);
      return result;
    }],
    ["OAuth 手动开始", async () => {
      const result = await desktop.mcp.oauthBegin(binding, clientId, redirectUri, splitScopes(scopes));
      setOauthState(result.state);
      return result;
    }],
    ["OAuth 完成", () => desktop.mcp.oauthComplete(oauthState, oauthCode, oauthIssuer)],
    ["展示交互输入", () => desktop.mcp.presentInput(inputId, "是否允许 MCP Server 继续执行？", "MCP 授权确认")],
    ["接受交互输入", () => desktop.mcp.respondInput(inputId, { accepted: true })],
  ];

  return (
    <section className="action-group mcp-workbench">
      <div className="section-title">
        <div>
          <h2>MCP 工作台</h2>
          <p>连接、工具、Resources、Prompts、OAuth、交互输入和订阅事件。</p>
        </div>
        <span className="badge">{actions.length}</span>
      </div>
      <div className="mcp-fields">
        <Field label="Binding" value={binding} onChange={setBinding} />
        <Field label="Tool" value={tool} onChange={setTool} />
        <Field label="工具参数 JSON" value={argumentsJson} onChange={setArgumentsJson} wide />
        <Field label="Resource URI" value={resourceUri} onChange={setResourceUri} wide />
        <Field label="Prompt" value={promptName} onChange={setPromptName} />
        <Field label="Prompt 参数 JSON" value={promptArguments} onChange={setPromptArguments} />
        <Field label="Completion value" value={completionValue} onChange={setCompletionValue} />
        <Field label="OAuth Client ID" value={clientId} onChange={setClientId} />
        <Field label="OAuth scopes（逗号分隔）" value={scopes} onChange={setScopes} wide />
        <Field label="OAuth redirect URI" value={redirectUri} onChange={setRedirectUri} wide />
        <Field label="OAuth state" value={oauthState} onChange={setOauthState} />
        <Field label="OAuth code" value={oauthCode} onChange={setOauthCode} />
        <Field label="OAuth issuer" value={oauthIssuer} onChange={setOauthIssuer} />
        <Field label="交互 Input ID" value={inputId} onChange={setInputId} />
      </div>
      <ul className="actions mcp-actions">
        {actions.map(([label, run]) => (
          <li key={label}>
            <button type="button" disabled={pending} onClick={() => invoke(label, run)}>{label}</button>
          </li>
        ))}
      </ul>
      <small className="mcp-note">需要应用已经配置或持久化对应 binding；监听操作最多等待 15 秒。</small>
    </section>
  );
}

function Field({ label, value, onChange, wide = false }: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  wide?: boolean;
}): React.ReactElement {
  return (
    <label className={wide ? "wide" : undefined}>
      <span>{label}</span>
      <input value={value} onChange={(event) => onChange(event.target.value)} />
    </label>
  );
}

function splitScopes(value: string): string[] {
  return value.split(",").map((scope) => scope.trim()).filter(Boolean);
}
