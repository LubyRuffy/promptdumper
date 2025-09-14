import { useEffect, useMemo, useState, useDeferredValue, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { clsx } from "clsx";
import { ResizablePanelGroup, ResizablePanel, ResizableHandle } from "./components/ui/resizable";
import { ScrollArea } from "./components/ui/scroll-area";
import { Button } from "./components/ui/button";
import { Checkbox } from "./components/ui/checkbox";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./components/ui/select";
import { Light as SyntaxHighlighter } from "react-syntax-highlighter";
import atomOneLight from "react-syntax-highlighter/dist/esm/styles/hljs/atom-one-light";
import atomOneDark from "react-syntax-highlighter/dist/esm/styles/hljs/atom-one-dark";
import httpLang from "react-syntax-highlighter/dist/esm/languages/hljs/http";
import jsonLang from "react-syntax-highlighter/dist/esm/languages/hljs/json";
import xmlLang from "react-syntax-highlighter/dist/esm/languages/hljs/xml";
import jsLang from "react-syntax-highlighter/dist/esm/languages/hljs/javascript";
import plaintextLang from "react-syntax-highlighter/dist/esm/languages/hljs/plaintext";
import { Code, FileText, Languages, Sun, Moon, Monitor } from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "./components/ui/tabs";
import { ProviderIcon } from "./components/ProviderIcon";

type HeaderKV = { name: string; value: string };
type HttpReq = {
  id: string;
  timestamp: string;
  src_ip: string;
  src_port: number;
  dst_ip: string;
  dst_port: number;
  method: string;
  path: string;
  version: string;
  headers: HeaderKV[];
  body_base64?: string;
  body_len: number;
  process_name?: string;
  pid?: number;
  is_llm: boolean;
  llm_provider?: string;
};
type HttpResp = {
  id: string;
  timestamp: string;
  src_ip: string;
  src_port: number;
  dst_ip: string;
  dst_port: number;
  status_code: number;
  reason?: string;
  version: string;
  headers: HeaderKV[];
  body_base64?: string;
  body_len: number;
  process_name?: string;
  pid?: number;
  is_llm: boolean;
  llm_provider?: string;
};

type Row = {
  id: string;
  req?: HttpReq;
  resp?: HttpResp;
};

function App() {
  const [ifaces, setIfaces] = useState<{ name: string; desc?: string | null; ip?: string | null }[]>([]);
  const [iface, setIface] = useState<string>("lo0");
  const [rows, setRows] = useState<Row[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = useMemo(() => rows.find((r) => r.id === selectedId), [rows, selectedId]);
  const [running, setRunning] = useState(false);
  const [reqBodyMode, setReqBodyMode] = useState<"pretty" | "raw">("pretty");
  const [respBodyMode, setRespBodyMode] = useState<"pretty" | "raw">("pretty");
  const [respAgg, setRespAgg] = useState<Record<string, { ct: string; text: string; size: number }>>({});
  const [showAll, setShowAll] = useState<boolean>(false);
  const [theme, setTheme] = useState<"system" | "light" | "dark">(() => (localStorage.getItem("theme") as any) || "system");
  const [lang, setLang] = useState<"zh" | "en">(() => (localStorage.getItem("lang") as any) || "zh");
  const [isDark, setIsDark] = useState<boolean>(false);
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    row?: Row;
  } | null>(null);
  const [copyTip, setCopyTip] = useState<{ x: number; y: number; text: string } | null>(null);

  const translations: Record<string, Record<string, string>> = {
    zh: {
      start: "开始",
      stop: "停止",
      show_all: "显示所有",
      request: "请求",
      response: "响应",
      time: "时间",
      source: "源",
      destination: "目的",
      method: "方法",
      status: "状态",
      path: "路径",
      length: "长度",
      process: "进程",
      no_request: "未选择或无请求",
      no_response: "未选择或无响应",
      thinking: "思考",
      contentTitle: "正文",
      tool_calls: "工具调用",
      theme_system: "系统",
      theme_light: "浅色",
      theme_dark: "深色",
      raw: "原始",
      format: "格式化",
      markdown: "Markdown",
      theme_label: "主题",
      language_label: "语言",
      copy_as_curl: "复制为 curl",
      copied: "已复制到剪贴板",
    },
    en: {
      start: "Start",
      stop: "Stop",
      show_all: "Show All",
      request: "Request",
      response: "Response",
      time: "Time",
      source: "Source",
      destination: "Destination",
      method: "Method",
      status: "Status",
      path: "Path",
      length: "Length",
      process: "Process",
      no_request: "No request selected",
      no_response: "No response selected",
      thinking: "Thinking",
      contentTitle: "Content",
      tool_calls: "Tool Calls",
      theme_system: "System",
      theme_light: "Light",
      theme_dark: "Dark",
      raw: "Raw",
      format: "Format",
      markdown: "Markdown",
      theme_label: "Theme",
      language_label: "Language",
      copy_as_curl: "Copy as curl",
      copied: "Copied to clipboard",
    },
  };
  const t = useCallback((k: keyof typeof translations["zh"]) => translations[lang]?.[k] || (k as string), [lang]);

  useEffect(() => {
    (async () => {
      const list = (await invoke("list_network_interfaces")) as { name: string; desc?: string | null; ip?: string | null }[];
      setIfaces(list);
      if (list.some((d) => d.name === "lo")) setIface("lo");
      if (list.some((d) => d.name === "lo0")) setIface("lo0");
    })();
    const unlistenReqP = listen<HttpReq>("onHttpRequest", (e) => {
      const data = e.payload;
      setRows((old) => {
        const nx = [...old];
        const idx = nx.findIndex((r) => r.id === data.id);
        if (idx >= 0) nx[idx].req = data;
        else nx.unshift({ id: data.id, req: data });
        return nx.slice(0, 500);
      });
    });
    const unlistenRespP = listen<HttpResp>("onHttpResponse", (e) => {
      const data = e.payload;
      // accumulate streaming chunks by id (avoid stale closure by using functional setter)
      setRespAgg((old) => {
        let ct = data.headers.find((h) => h.name.toLowerCase() === "content-type")?.value || old[data.id]?.ct || "";
        let text = old[data.id]?.text || "";
        let size = old[data.id]?.size || 0;
        try {
          if (data.body_base64) {
            const bytes = Uint8Array.from(atob(data.body_base64), (c) => c.charCodeAt(0));
            const chunkText = new TextDecoder().decode(bytes);
            text += chunkText;
            size += bytes.length;
          }
        } catch {}
        return { ...old, [data.id]: { ct, text, size } };
      });
      setRows((old) => {
        const nx = [...old];
        const idx = nx.findIndex((r) => r.id === data.id);
        if (idx >= 0) nx[idx].resp = data; // 只更新已有请求，不创建只有响应的行
        return nx.slice(0, 500);
      });
    });
    return () => {
      unlistenReqP.then((f) => f());
      unlistenRespP.then((f) => f());
    };
  }, []);

  useEffect(() => {
    setReqBodyMode("pretty");
    setRespBodyMode("pretty");
  }, [selectedId]);

  useEffect(() => {
    const root = document.documentElement;
    const apply = () => {
      const wantDark = theme === "dark" || (theme === "system" && window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches);
      root.classList.toggle("dark", !!wantDark);
      setIsDark(!!wantDark);
    };
    apply();
    localStorage.setItem("theme", theme);
    let mq: MediaQueryList | null = null;
    if (theme === "system" && window.matchMedia) {
      mq = window.matchMedia("(prefers-color-scheme: dark)");
      const handler = () => apply();
      try { mq.addEventListener("change", handler); } catch { mq?.addListener(handler); }
      return () => { try { mq?.removeEventListener("change", handler); } catch { mq?.removeListener(handler); } };
    }
  }, [theme]);
  useEffect(() => { localStorage.setItem("lang", lang); }, [lang]);

  useEffect(() => {
    const hide = () => setContextMenu(null);
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") setContextMenu(null); };
    document.addEventListener("click", hide);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("click", hide);
      document.removeEventListener("keydown", onKey);
    };
  }, []);

  async function start() {
    await invoke("start_capture", { args: { iface } });
    setRunning(true);
  }
  async function stop() {
    await invoke("stop_capture");
    setRunning(false);
  }

  function humanSize(n: number) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 / 1024).toFixed(1)} MB`;
  }

  function respSizeForRow(r: Row): number {
    const agg = respAgg[r.id];
    if (agg && agg.text) {
      try { return new TextEncoder().encode(agg.text).length; } catch { return agg.size || 0; }
    }
    return r.resp?.body_len ?? 0;
  }

  function decodeBody(base64?: string): Uint8Array | null {
    if (!base64) return null;
    try { return Uint8Array.from(atob(base64), (c) => c.charCodeAt(0)); } catch { return null; }
  }

  function escapeShellArg(s: string): string {
    return "'" + s.replace(/'/g, "'\\''") + "'";
  }

  function buildCurlFromRow(r: Row): string {
    if (!r.req) return "";
    const req = r.req;
    // URL: prefer Host header; fallback to dst_ip:dst_port
    const hostHeader = req.headers.find(h => h.name.toLowerCase() === "host")?.value || "";
    const host = hostHeader.trim() || `${req.dst_ip}:${req.dst_port}`;
    const path = req.path.startsWith("/") ? req.path : ("/" + req.path);
    const forwardedProto = req.headers.find(h => h.name.toLowerCase() === "x-forwarded-proto")?.value?.toLowerCase() || "";
    const scheme = (forwardedProto === "https" || req.dst_port === 443) ? "https" : "http";
    const url = `${scheme}://${host}${path}`;
    // Headers: include all except content-length (curl will set)
    const headerArgs = req.headers
      .filter(h => h.name.toLowerCase() !== "content-length")
      .map(h => `-H ${escapeShellArg(`${h.name}: ${h.value}`)}`)
      .join(" ");
    // Method
    const methodArg = `-X ${req.method}`;
    // Body
    let bodyArg = "";
    if (req.body_base64) {
      const bytes = decodeBody(req.body_base64);
      if (bytes && bytes.length > 0) {
        let bodyText: string;
        try {
          bodyText = new TextDecoder().decode(bytes);
        } catch {
          bodyText = "";
        }
        if (bodyText) bodyArg = `--data-binary ${escapeShellArg(bodyText)}`;
      }
    }
    const parts = ["curl", "-sS", methodArg, headerArgs, bodyArg, escapeShellArg(url)].filter(Boolean);
    return parts.join(" ").replace(/\s+/g, " ").trim();
  }

  async function copyAsCurl(r: Row, pos?: { x: number; y: number }) {
    const cmd = buildCurlFromRow(r);
    if (!cmd) return;
    try {
      await navigator.clipboard.writeText(cmd);
    } catch {
      try {
        const ta = document.createElement("textarea");
        ta.value = cmd;
        ta.style.position = "fixed";
        ta.style.top = "-1000px";
        document.body.appendChild(ta);
        ta.focus();
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
      } catch {}
    }
    setContextMenu(null);
    const tipAt = pos || (contextMenu ? { x: contextMenu.x, y: contextMenu.y } : { x: 20, y: 20 });
    setCopyTip({ x: tipAt.x, y: tipAt.y, text: t("copied") });
    window.setTimeout(() => setCopyTip(null), 1500);
  }

  // register once
  SyntaxHighlighter.registerLanguage("http", httpLang);
  SyntaxHighlighter.registerLanguage("json", jsonLang);
  SyntaxHighlighter.registerLanguage("xml", xmlLang);
  SyntaxHighlighter.registerLanguage("plaintext", plaintextLang);
  SyntaxHighlighter.registerLanguage("javascript", jsLang);
  const syntaxStyle = isDark ? atomOneDark : atomOneLight;
  function parseLlmMarkdown(text: string) {
    // 尝试从 JSON（或 SSE JSON 行）中提取 reasoning / reasoning_content 与正文（常见字段）
    // 兼容 OpenAI choices[].delta / message / content，以及通用 "reasoning" | "reasoning_content"
    const reasoningBuf: string[] = [];
    const contentBuf: string[] = [];
    // 收集工具调用（支持流式增量和最终消息）
    const toolCallsFinal: any[] = [];
    const toolCallsDelta: Record<string, { id?: string; index?: number; type?: string; function?: { name?: string; arguments?: string } }> = {};

    const pushText = (v: any, into: string[]) => {
      if (!v) return;
      if (typeof v === 'string') { into.push(v); return; }
      if (Array.isArray(v)) {
        for (const item of v) {
          if (typeof item === 'string') { into.push(item); continue; }
          if (item && typeof item === 'object') {
            // OpenAI / others: { type: 'text', text: '...' }
            if (typeof (item as any).text === 'string') { into.push((item as any).text); continue; }
            if (typeof (item as any).content === 'string') { into.push((item as any).content); continue; }
            if (typeof (item as any).value === 'string') { into.push((item as any).value); continue; }
          }
        }
        return;
      }
      if (typeof v === 'object') {
        if (Array.isArray((v as any).content)) {
          for (const x of (v as any).content) {
            if (typeof x === 'string') contentBuf.push(x);
            else if (x && typeof x === 'object' && typeof (x as any).text === 'string') contentBuf.push((x as any).text);
          }
        } else {
          if (typeof (v as any).text === 'string') into.push((v as any).text);
          if (typeof (v as any).content === 'string') into.push((v as any).content);
          if (typeof (v as any).value === 'string') into.push((v as any).value);
        }
      }
    };

    const addToolCallDelta = (tc: any) => {
      if (!tc) return;
      // 使用 index 作为主键进行聚合（OpenAI 流式场景中 index 始终稳定），
      // 若 index 缺失再回退到 id，最后回退到单一聚合键。
      const key = (typeof tc.index === 'number' ? `idx:${tc.index}` : (tc.id ? `id:${tc.id}` : 'one')) as string;
      if (!toolCallsDelta[key]) toolCallsDelta[key] = { id: tc.id, index: tc.index, type: tc.type || 'function', function: { name: undefined, arguments: '' } };
      const cur = toolCallsDelta[key];
      if (!cur.function) cur.function = {};
      const name = tc.function?.name || tc.name;
      const argsPart = tc.function?.arguments ?? tc.arguments ?? '';
      if (name && !cur.function.name) cur.function.name = name;
      if (typeof argsPart === 'string' && argsPart) cur.function.arguments = (cur.function.arguments || '') + argsPart;
    };

    const addToolCallFull = (tc: any) => {
      if (!tc) return;
      // 统一成 { type: 'function', function: { name, arguments } } 形态
      if (tc.function || (tc.name || tc.arguments)) {
        toolCallsFinal.push(tc.function ? tc : { type: 'function', function: { name: tc.name, arguments: tc.arguments } });
        return;
      }
      if (tc.tool_calls) {
        for (const t of tc.tool_calls) addToolCallFull(t);
        return;
      }
      // 兜底直接放入
      toolCallsFinal.push(tc);
    };

    const addFromObj = (obj: any) => {
      if (!obj || typeof obj !== 'object') return;
      // Ollama chat format: { message: { content: string, thinking: string } }
      if (obj.message && typeof obj.message === 'object') {
        const m = obj.message as any;
        pushText(m.thinking, reasoningBuf);
        pushText(m.content, contentBuf);
        // 非流式最终工具调用
        if (Array.isArray(m.tool_calls)) {
          for (const tc of m.tool_calls) addToolCallFull(tc);
        }
        if (m.function_call) addToolCallFull({ type: 'function', function: m.function_call });
      }
      pushText(obj.reasoning, reasoningBuf);
      pushText(obj.reasoning_content, reasoningBuf);
      // OpenAI style
      if (obj.choices) {
        for (const c of obj.choices) {
          if (c.delta) {
            pushText(c.delta.reasoning, reasoningBuf);
            pushText(c.delta.reasoning_content, reasoningBuf);
            pushText(c.delta.content, contentBuf);
            // 流式工具调用（新接口）
            if (Array.isArray(c.delta.tool_calls)) {
              for (const tc of c.delta.tool_calls) addToolCallDelta(tc);
            }
            // 旧接口 function_call 增量
            if (c.delta.function_call) addToolCallDelta({ type: 'function', function: c.delta.function_call, index: 0 });
          }
          if (c.message) {
            pushText(c.message.reasoning, reasoningBuf);
            pushText(c.message.reasoning_content, reasoningBuf);
            pushText(c.message.content, contentBuf);
            // 最终工具调用
            if (Array.isArray(c.message.tool_calls)) {
              for (const tc of c.message.tool_calls) addToolCallFull(tc);
            }
            if (c.message.function_call) addToolCallFull({ type: 'function', function: c.message.function_call });
          }
          pushText(c.reasoning, reasoningBuf);
          pushText(c.reasoning_content, reasoningBuf);
          pushText(c.text, contentBuf);
          pushText(c.content, contentBuf);
        }
      }
      // 顶层同名字段
      if (Array.isArray((obj as any).tool_calls)) {
        for (const tc of (obj as any).tool_calls) addToolCallFull(tc);
      }
      if ((obj as any).function_call) addToolCallFull({ type: 'function', function: (obj as any).function_call });
      if (Array.isArray((obj as any).parallel_tool_calls)) {
        for (const tc of (obj as any).parallel_tool_calls) addToolCallFull(tc);
      }
      pushText(obj.content, contentBuf);
      pushText(obj.text, contentBuf);
    };

    try {
      const t = text.replace(/\r/g, "");
      // SSE frames (chunked with data: prefix)
      if (t.includes("data:")) {
        const lines = t.split("\n");
        for (const raw of lines) {
          const trimmed = raw.trim();
          if (!trimmed || /^[0-9a-fA-F]+$/.test(trimmed)) continue;
          const m = /^(?:[0-9a-fA-F]+\s+)?data:\s*(.*)$/.exec(trimmed);
          if (!m) continue;
          let payload = m[1].trim();
          const lastBrace = payload.lastIndexOf('}');
          if (lastBrace >= 0) payload = payload.slice(0, lastBrace + 1);
          if (!payload || payload === "[DONE]") continue;
          try { addFromObj(JSON.parse(payload)); } catch {}
        }
      } else {
        // First, try to parse the whole text as a single JSON document
        // (covers pretty-printed JSON that contains newlines).
        const trimmed = t.trimStart();
        let parsedWhole = false;
        if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
          try { addFromObj(JSON.parse(t)); parsedWhole = true; } catch {}
        }
        // If not a single JSON doc, fall back to NDJSON line-by-line parsing
        if (!parsedWhole) {
          if (t.includes("\n")) {
            const lines = t.split("\n");
            for (const raw of lines) {
              const line = raw.trim();
              if (!line || /^[0-9a-fA-F]+$/.test(line) || line === "0") continue;
              try { addFromObj(JSON.parse(line)); } catch {}
            }
          } else {
            try { addFromObj(JSON.parse(t)); } catch {}
          }
        }
      }
    } catch {}

    const reasoning = reasoningBuf.join("");
    const content = contentBuf.join("");
    // 将增量工具调用合并为最终形态
    Object.values(toolCallsDelta)
      .sort((a, b) => {
        const ai = typeof a.index === 'number' ? a.index : 0;
        const bi = typeof b.index === 'number' ? b.index : 0;
        return ai - bi;
      })
      .forEach((v) => {
        toolCallsFinal.push({ type: v.type || 'function', id: v.id, index: v.index, function: { name: v.function?.name, arguments: v.function?.arguments } });
      });
    return { reasoning, content, toolCalls: toolCallsFinal };
  }

  function MarkdownView({ headers: _headers, base64, aggText }: { headers: HeaderKV[]; base64?: string; aggText?: string }) {
    const bytes = decodeBody(base64);
    const raw = aggText ?? (bytes ? new TextDecoder().decode(bytes) : "");
    const deferredRaw = useDeferredValue(raw);
    const { reasoning, content, toolCalls } = useMemo(() => parseLlmMarkdown(deferredRaw), [deferredRaw]);
    const [reasoningOpen, setReasoningOpen] = useState<boolean>(() => !!reasoning && !content);
    const [reasoningUserToggled, setReasoningUserToggled] = useState<boolean>(false);
    useEffect(() => {
      if (!reasoningUserToggled && reasoning && !content) {
        setReasoningOpen(true);
      }
    }, [reasoning, content, reasoningUserToggled]);
    return (
      <div className="rounded-md border bg-gray-50 dark:bg-gray-900/30 p-3 text-[12px] leading-6">
        {reasoning ? (
          <details className="mb-2" open={reasoningOpen} onToggle={(e) => { setReasoningOpen((e.target as HTMLDetailsElement).open); setReasoningUserToggled(true); }}>
            <summary className="cursor-pointer select-none text-[12px] font-semibold">{t("thinking")}</summary>
            <div className="mt-1 text-[12px] leading-6">
              <ReactMarkdown
                remarkPlugins={[remarkGfm]}
                components={{
                  h1: (props: any) => <h3 className="text-[12px] font-semibold my-1" {...props} />,
                  h2: (props: any) => <h4 className="text-[12px] font-semibold my-1" {...props} />,
                  h3: (props: any) => <h5 className="text-[12px] font-semibold my-1" {...props} />,
                  p:  (props: any) => <p className="my-1" {...props} />,
                  ul: (props: any) => <ul className="list-disc ml-4 my-1" {...props} />,
                  ol: (props: any) => <ol className="list-decimal ml-4 my-1" {...props} />,
                  code: (props: any) => {
                    const { inline, children } = props as any;
                    return inline
                      ? <code className="px-1 py-0.5 bg-gray-100 dark:bg-gray-800 rounded text-[11px]">{children}</code>
                      : <pre className="p-2 bg-gray-100 dark:bg-gray-800 rounded overflow-auto text-[11px]"><code>{children}</code></pre>;
                  },
                }}
              >
                {reasoning}
              </ReactMarkdown>
            </div>
          </details>
        ) : null}
        {toolCalls && toolCalls.length > 0 ? (
          <div className="mb-2">
            <div className="text-[12px] font-semibold my-1">{t("tool_calls")}</div>
            <div className="space-y-2">
              {toolCalls.map((tc: any, i: number) => (
                <SyntaxHighlighter key={i} language="json" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
                  {JSON.stringify(tc, null, 2)}
                </SyntaxHighlighter>
              ))}
            </div>
          </div>
        ) : null}
        {content ? (
        <div className="text-[12px] leading-6">
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            components={{
              table: (props: any) => <table className="table-fixed border-collapse my-2" {...props} />,
              thead: (props: any) => <thead className="bg-gray-100 dark:bg-gray-800" {...props} />,
              th: (props: any) => <th className="border px-2 py-1 text-left text-[12px]" {...props} />,
              td: (props: any) => <td className="border px-2 py-1 text-left text-[12px]" {...props} />,
              h1: (props: any) => <h3 className="text-[12px] font-semibold my-1" {...props} />,
              h2: (props: any) => <h4 className="text-[12px] font-semibold my-1" {...props} />,
              h3: (props: any) => <h5 className="text-[12px] font-semibold my-1" {...props} />,
              p:  (props: any) => <p className="my-1" {...props} />,
              ul: (props: any) => <ul className="list-disc ml-4 my-1" {...props} />,
              ol: (props: any) => <ol className="list-decimal ml-4 my-1" {...props} />,
              code: (props: any) => {
                const { inline, children } = props as any;
                return inline
                  ? <code className="px-1 py-0.5 bg-gray-100 dark:bg-gray-800 rounded text-[11px]">{children}</code>
                  : <pre className="p-2 bg-gray-100 dark:bg-gray-800 rounded overflow-auto text-[11px]"><code>{children}</code></pre>;
              },
            }}
          >
            {content}
          </ReactMarkdown>
        </div>
        ) : null}
      </div>
    );
  }

  function renderHeadersAsHttp(startLine: string, headers: HeaderKV[]) {
    const text = [startLine, ...headers.map(h => `${h.name}: ${h.value}`)].join("\n");
    return (
      <SyntaxHighlighter language="http" style={syntaxStyle} customStyle={{ margin: 0, background: "transparent", fontSize: 12 }}>
        {text}
      </SyntaxHighlighter>
    );
  }

  function bodyPreview(
    headers: HeaderKV[],
    base64?: string,
    mode: "pretty" | "raw" = "pretty",
    aggText?: string,
    onToggle?: () => void,
    jsonIfLooksLike?: boolean,
  ) {
    const bytes = decodeBody(base64);
    const ct = headers.find((h) => h.name.toLowerCase() === "content-type")?.value || "";
    const text = aggText ?? (bytes ? new TextDecoder().decode(bytes) : "");
    if (!text) return null;
    const toggleNode = onToggle ? (
      <div className="absolute right-2 top-2 z-10">
        <Button variant="ghost" size="icon" onClick={onToggle}>
          {mode === "pretty" ? <FileText className="h-4 w-4" /> : <Code className="h-4 w-4" />}
        </Button>
      </div>
    ) : null;
    const wrap = (child: React.ReactNode) => (
      <div className="relative">
        {toggleNode}
        {child}
      </div>
    );
    // SSE pretty formatting: hide chunk-size lines, keep only data: payload, pretty print JSON per event
    if (ct.includes("text/event-stream")) {
      if (mode === "raw") {
        return wrap(
          <SyntaxHighlighter language="plaintext" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
            {text}
          </SyntaxHighlighter>
        );
      }
      const lines = text.replace(/\r/g, "").split("\n");
      type Ev = { kind: "json" | "text" | "done"; pretty: string };
      const events: Ev[] = [];
      let buf: string[] = [];
      const isChunkSize = (s: string) => /^[0-9a-fA-F]+$/.test(s.trim());
      const flush = () => {
        if (buf.length === 0) return;
        const payload = buf.join("\n").trim();
        if (payload === "[DONE]") {
          events.push({ kind: "done", pretty: "[DONE]" });
        } else if ((payload.startsWith("{") || payload.startsWith("["))) {
          try {
            events.push({ kind: "json", pretty: JSON.stringify(JSON.parse(payload), null, 2) });
          } catch {
            events.push({ kind: "text", pretty: payload });
          }
        } else {
          events.push({ kind: "text", pretty: payload });
        }
        buf = [];
      };
      for (const line of lines) {
        if (isChunkSize(line)) continue;
        if (line.startsWith("data:")) { buf.push(line.slice(5).trimStart()); continue; }
        if (line.trim() === "") { flush(); continue; }
        // ignore other SSE fields (event:, id:, retry:) in pretty mode
      }
      flush();
      return wrap(
        <div className="space-y-2">
          {events.map((ev, i) => (
            <SyntaxHighlighter
              key={i}
              language={ev.kind === "json" ? "json" : "plaintext"}
              style={atomOneLight}
              customStyle={{ margin: 0, fontSize: 12 }}
            >
              {ev.pretty}
            </SyntaxHighlighter>
          ))}
        </div>
      );
    }
    if (ct.includes("application/json")) {
      if (mode === "raw") {
        return wrap(<pre className="text-xs whitespace-pre-wrap">{text}</pre>);
      }
      try {
        return wrap(
          <SyntaxHighlighter language="json" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
            {JSON.stringify(JSON.parse(text), null, 2)}
          </SyntaxHighlighter>
        );
      } catch {}
    }
    // Compatibility: some curl requests use application/x-www-form-urlencoded but actually send JSON text
    if (jsonIfLooksLike && mode !== "raw") {
      const trimmed = text.trimStart();
      if ((trimmed.startsWith("{") || trimmed.startsWith("["))) {
        try {
          const parsed = JSON.parse(text);
          return wrap(
            <SyntaxHighlighter language="json" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
              {JSON.stringify(parsed, null, 2)}
            </SyntaxHighlighter>
          );
        } catch {}
      }
    }
    if (ct.includes("application/javascript") || ct.includes("text/javascript") || ct.includes("ecmascript")) {
      if (mode === "raw") {
        return wrap(<pre className="text-xs whitespace-pre-wrap">{text}</pre>);
      }
      return wrap(
        <SyntaxHighlighter language="javascript" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
          {text}
        </SyntaxHighlighter>
      );
    }
    if (ct.includes("application/x-www-form-urlencoded")) {
      if (mode === "raw") {
        return wrap(
          <SyntaxHighlighter language="plaintext" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
            {text}
          </SyntaxHighlighter>
        );
      }
      // Pretty print k=v&k2=v2 with URL decoding
      const formatted = text
        .split("&")
        .filter(Boolean)
        .map((seg) => {
          const [k, v = ""] = seg.split("=");
          const dk = (() => { try { return decodeURIComponent((k || "").replace(/\+/g, " ")); } catch { return k || ""; } })();
          const dv = (() => { try { return decodeURIComponent((v || "").replace(/\+/g, " ")); } catch { return v || ""; } })();
          return `${dk}: ${dv}`;
        })
        .join("\n");
      return wrap(
        <SyntaxHighlighter language="plaintext" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
          {formatted || text}
        </SyntaxHighlighter>
      );
    }
    if (ct.includes("text/html") || ct.includes("application/xml") || ct.includes("text/xml")) {
      if (mode === "raw") {
        return wrap(<pre className="text-xs whitespace-pre-wrap">{text}</pre>);
      }
      return wrap(
        <SyntaxHighlighter language="xml" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
          {text}
        </SyntaxHighlighter>
      );
    }
    if (ct.startsWith("text/")) {
      if (mode === "raw") {
        return wrap(<pre className="text-xs whitespace-pre-wrap">{text}</pre>);
      }
      return wrap(
        <SyntaxHighlighter language="plaintext" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
          {text}
        </SyntaxHighlighter>
      );
    }
    // NDJSON: treat as text and pretty-print each JSON line
    if (ct.includes("application/x-ndjson") || ct.includes("application/ndjson")) {
      if (mode === "raw") {
        return wrap(
          <SyntaxHighlighter language="plaintext" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
            {text}
          </SyntaxHighlighter>
        );
      }
      const lines = text.replace(/\r/g, "").split("\n").filter(Boolean);
      return wrap(
        <div className="space-y-2">
          {lines.map((ln, i) => {
            let line = ln.trim();
            if (!line) return null;
            // skip pure chunk-size (hex) or terminator 0
            if (/^[0-9a-fA-F]+$/.test(line) || line === "0") return null;
            // handle "<hex> {json}" on same line
            const hexPrefix = /^([0-9a-fA-F]+)\s+(\{.*)$/.exec(line);
            if (hexPrefix) line = hexPrefix[2];
            try {
              const obj = JSON.parse(line);
              return (
                <SyntaxHighlighter key={i} language="json" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
                  {JSON.stringify(obj, null, 2)}
                </SyntaxHighlighter>
              );
            } catch {
              return (
                <SyntaxHighlighter key={i} language="plaintext" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
                  {line}
                </SyntaxHighlighter>
              );
            }
          })}
        </div>
      );
    }

    // 通用 JSON 猜测（某些客户端未设置 JSON Content-Type），仅在 Pretty 模式下尝试
    if (mode === "pretty") {
      const trimmed = text.trimStart();
      if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
        try {
          const parsed = JSON.parse(text);
          return wrap(
            <SyntaxHighlighter language="json" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
              {JSON.stringify(parsed, null, 2)}
            </SyntaxHighlighter>
          );
        } catch {}
      }
    }

    // Fallback: 如果有聚合文本（如流式积累），就按纯文本展示
    if (aggText && aggText.length > 0) {
      return wrap(
        <SyntaxHighlighter language="plaintext" style={syntaxStyle} customStyle={{ margin: 0, fontSize: 12 }}>
          {text}
        </SyntaxHighlighter>
      );
    }

    // fallback: hex dump
    const hexLines: string[] = [];
    const arr = bytes || new Uint8Array();
    for (let i = 0; i < arr.length; i += 16) {
      const slice = arr.slice(i, i + 16);
      const hex = Array.from(slice).map((b) => b.toString(16).padStart(2, "0")).join(" ");
      const ascii = Array.from(slice).map((b) => (b >= 32 && b <= 126 ? String.fromCharCode(b) : ".")).join("");
      hexLines.push(i.toString(16).padStart(8, "0") + "  " + hex.padEnd(16 * 3 - 1, " ") + "  |" + ascii + "|");
    }
    return wrap(<pre className="text-xs font-mono">{hexLines.join("\n")}</pre>);
  }

  return (
    <div className="h-screen flex flex-col">
      <div className="p-1.5 flex items-center gap-1.5 text-xs z-[100] relative">
        <Select value={iface} onValueChange={setIface}>
          <SelectTrigger className="w-[240px] h-8 text-xs">
            <SelectValue placeholder={"iface"} />
          </SelectTrigger>
          <SelectContent>
            {ifaces.map((i) => (
              <SelectItem key={i.name} value={i.name}>
                {i.name}{i.ip ? ` (${i.ip})` : ""}{i.desc ? ` - ${i.desc}` : ""}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {!running ? (
          <Button size="sm" onClick={start}>{t("start")}</Button>
        ) : (
          <Button size="sm" variant="secondary" onClick={stop}>{t("stop")}</Button>
        )}
        <label className="flex items-center gap-1 select-none">
          <Checkbox checked={showAll} onCheckedChange={(v: boolean | "indeterminate") => setShowAll(v === true)} />
          <span>{t("show_all")}</span>
        </label>
        <div className="ml-auto flex items-center gap-1.5">
          <Select value={theme} onValueChange={(v) => setTheme(v as any)}>
            <SelectTrigger variant="icon" aria-label={t("theme_label")} hideChevron>
              {theme === "system" ? (
                <Monitor className="h-4 w-4" />
              ) : theme === "light" ? (
                <Sun className="h-4 w-4" />
              ) : (
                <Moon className="h-4 w-4" />
              )}
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="light">
                <span className="inline-flex items-center gap-2">
                  <Sun className="h-4 w-4" />
                  {t("theme_light")}
                </span>
              </SelectItem>
              <SelectItem value="dark">
                <span className="inline-flex items-center gap-2">
                  <Moon className="h-4 w-4" />
                  {t("theme_dark")}
                </span>
              </SelectItem>
              <SelectItem value="system">
                <span className="inline-flex items-center gap-2">
                  <Monitor className="h-4 w-4" />
                  {t("theme_system")}
                </span>
              </SelectItem>
            </SelectContent>
          </Select>
          <Select value={lang} onValueChange={(v) => setLang(v as any)}>
            <SelectTrigger variant="icon" aria-label={t("language_label")} hideChevron>
              <Languages className="h-4 w-4" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="zh">🇨🇳 中文</SelectItem>
              <SelectItem value="en">🇺🇸 English</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      <ResizablePanelGroup direction="vertical" className="flex-1 min-h-0">
        <ResizablePanel defaultSize={55} minSize={20}>
          <ScrollArea className="min-h-0 h-full bg-background">
          <table className="w-full caption-bottom text-sm">
            <thead className="sticky top-0 z-10 bg-background">
              <tr>
                <th className="h-8 px-2 text-left align-middle text-[14px] font-medium">{t("time")}</th>
                <th className="h-8 px-2 text-left align-middle text-[14px] font-medium">{t("source")}</th>
                <th className="h-8 px-2 text-left align-middle text-[14px] font-medium">{t("destination")}</th>
                <th className="h-8 px-2 text-left align-middle text-[14px] font-medium">{t("method")}</th>
                <th className="h-8 px-2 text-left align-middle text-[14px] font-medium">{t("status")}</th>
                <th className="h-8 px-2 text-left align-middle text-[14px] font-medium">{t("path")}</th>
                <th className="h-8 px-2 text-left align-middle text-[14px] font-medium">{t("length")}</th>
                <th className="h-8 px-2 text-left align-middle text-[14px] font-medium">{t("process")}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {rows
                // 显示：
                // - 当“显示所有”开启时，任何有请求或响应的行
                // - 默认只显示识别为 LLM 的请求/响应（允许只有请求的占位行）
                .filter(r => (showAll ? (r.req || r.resp) : (r.req?.is_llm || r.resp?.is_llm)))
                .map((r) => (
              <tr
                key={r.id}
                className={clsx("hover:bg-muted/50 cursor-pointer", selectedId === r.id && "bg-muted")}
                onClick={() => setSelectedId(r.id)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setSelectedId(r.id);
                  setContextMenu({ x: e.clientX, y: e.clientY, row: r });
                }}
              >
                <td className="px-2 py-1.5 align-middle text-[12px]">
                  {(r.req?.is_llm || r.resp?.is_llm) ? (
                    <span className="inline-flex items-center gap-1">
                      <ProviderIcon provider={r.req?.llm_provider || r.resp?.llm_provider} />
                      <span>{r.req?.timestamp || r.resp?.timestamp}</span>
                    </span>
                  ) : (r.req?.timestamp || r.resp?.timestamp)}
                </td>
                <td className="px-2 py-1.5 align-middle text-[12px]">{r.req ? `${r.req.src_ip}:${r.req.src_port}` : r.resp ? `${r.resp.src_ip}:${r.resp.src_port}` : ""}</td>
                <td className="px-2 py-1.5 align-middle text-[12px]">{r.req ? `${r.req.dst_ip}:${r.req.dst_port}` : r.resp ? `${r.resp.dst_ip}:${r.resp.dst_port}` : ""}</td>
                <td className="px-2 py-1.5 align-middle text-[12px]">{r.req?.method || ""}</td>
                <td className="px-2 py-1.5 align-middle text-[12px]">{r.resp?.status_code ?? ""}</td>
                <td className="px-2 py-1.5 align-middle text-[12px] truncate max-w-[16rem]">{r.req?.path || ""}</td>
                <td className="px-2 py-1.5 align-middle text-[12px]">{r.resp ? humanSize(respSizeForRow(r)) : ""}</td>
                <td className="px-2 py-1.5 align-middle text-[12px]">{r.req?.process_name || r.resp?.process_name}</td>
              </tr>
            ))}
            </tbody>
          </table>
          </ScrollArea>
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel defaultSize={45} minSize={20}>
          <ResizablePanelGroup direction="horizontal" className="h-full min-h-0">
            <ResizablePanel defaultSize={50} minSize={20}>
              <ScrollArea className="p-3 h-full relative">
                <h3 className="font-semibold mb-1">{t("request")}</h3>
                {selected?.req ? (
                  <div className="space-y-2">
                    {renderHeadersAsHttp(`${selected.req.method} ${selected.req.path} HTTP/${selected.req.version}`, selected.req.headers)}
                    {bodyPreview(
                      selected.req.headers,
                      selected.req.body_base64,
                      reqBodyMode,
                      undefined,
                      () => setReqBodyMode(m => m === "pretty" ? "raw" : "pretty"),
                      // If this is an LLM call with NDJSON response, and curl didn't set JSON content-type for request,
                      // try to pretty print request body as JSON when it looks like JSON.
                      ((selected.req?.is_llm || selected.resp?.is_llm) &&
                        !!(selected.resp?.headers.find((h) => h.name.toLowerCase() === "content-type")?.value || "")
                          .toLowerCase()
                          .includes("ndjson"))
                    )}
                  </div>
                ) : <div className="text-sm text-muted-foreground">{t("no_request")}</div>}
              </ScrollArea>
            </ResizablePanel>
            <ResizableHandle />
            <ResizablePanel defaultSize={50} minSize={20}>
              <ScrollArea className="p-3 h-full relative">
                <h3 className="font-semibold mb-1">{t("response")}</h3>
                {selected?.resp ? (
                  <div className="space-y-2">
                    {renderHeadersAsHttp(`HTTP/${selected.resp.version} ${selected.resp.status_code}${selected.resp.reason ? ` ${selected.resp.reason}` : ""}`, selected.resp.headers)}
                    {(selected.resp.is_llm) ? (
                      <Tabs defaultValue="format">
                        <TabsList>
                          <TabsTrigger value="raw">{t("raw")}</TabsTrigger>
                          <TabsTrigger value="format">{t("format")}</TabsTrigger>
                          <TabsTrigger value="markdown">{t("markdown")}</TabsTrigger>
                        </TabsList>
                        <TabsContent value="raw">
                          {bodyPreview(selected.resp.headers, selected.resp.body_base64, "raw", respAgg[selected.id || ""]?.text)}
                        </TabsContent>
                        <TabsContent value="format">
                          {bodyPreview(selected.resp.headers, selected.resp.body_base64, "pretty", respAgg[selected.id || ""]?.text)}
                        </TabsContent>
                        <TabsContent value="markdown">
                          <MarkdownView headers={selected.resp.headers} base64={selected.resp.body_base64} aggText={respAgg[selected.id || ""]?.text} />
                        </TabsContent>
                      </Tabs>
                    ) : (
                      bodyPreview(
                        selected.resp.headers,
                        selected.resp.body_base64,
                        respBodyMode,
                        respAgg[selected.id || ""]?.text,
                        () => setRespBodyMode(m => m === "pretty" ? "raw" : "pretty"),
                      )
                    )}
                  </div>
                ) : <div className="text-sm text-muted-foreground">{t("no_response")}</div>}
              </ScrollArea>
            </ResizablePanel>
          </ResizablePanelGroup>
        </ResizablePanel>
      </ResizablePanelGroup>
      {contextMenu ? (
        <div
          className="fixed z-[200] min-w-[160px] rounded-md border bg-popover text-popover-foreground shadow-md"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          <button
            className="w-full text-left px-3 py-2 text-[12px] hover:bg-muted"
            onClick={() => contextMenu.row && copyAsCurl(contextMenu.row, { x: contextMenu.x, y: contextMenu.y })}
          >
            {t("copy_as_curl")}
          </button>
        </div>
      ) : null}
      {copyTip ? (
        <div
          className="fixed z-[201] px-2 py-1 rounded bg-black/80 text-white text-[12px]"
          style={{ left: copyTip.x + 8, top: copyTip.y + 8 }}
        >
          {copyTip.text}
        </div>
      ) : null}
    </div>
  );
}

export default App;
