import { useEffect, useMemo, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { clsx } from "clsx";
import { ResizablePanelGroup, ResizablePanel, ResizableHandle } from "./components/ui/resizable";
import { ScrollArea } from "./components/ui/scroll-area";
import { Button } from "./components/ui/button";
import { Checkbox } from "./components/ui/checkbox";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./components/ui/select";
import { Languages, Sun, Moon, Monitor } from "lucide-react";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "./components/ui/tabs";
import { ProviderIcon } from "./components/ProviderIcon";
import { Row, HttpReq, HttpResp } from "./types/http";
import HttpHeaders from "./components/HttpHeaders";
import BodyPreview from "./components/BodyPreview";
import MarkdownView from "./components/MarkdownView";
import { getSyntaxStyle } from "./syntax";
import { buildCurlFromRow, formatSize } from "./utils/http";

// Types moved to ./types/http

function App() {
  const [ifaces, setIfaces] = useState<{ name: string; desc?: string | null; ip?: string | null }[]>([]);
  const [iface, setIface] = useState<string>("lo0");
  const [rows, setRows] = useState<Row[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = useMemo(() => rows.find((r) => r.id === selectedId), [rows, selectedId]);
  const [running, setRunning] = useState(false);
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

  // moved to utils/http as formatSize

  function respSizeForRow(r: Row): number {
    const agg = respAgg[r.id];
    if (agg && agg.text) {
      try { return new TextEncoder().encode(agg.text).length; } catch { return agg.size || 0; }
    }
    return r.resp?.body_len ?? 0;
  }

  // buildCurlFromRow moved to utils/http

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

  const syntaxStyle = getSyntaxStyle(isDark);
  // parseLlmMarkdown moved to utils/llm

  // local MarkdownView removed; using components/MarkdownView instead

  // local renderHeadersAsHttp removed; using components/HttpHeaders instead

  // local bodyPreview removed; using components/BodyPreview instead

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
                <td className="px-2 py-1.5 align-middle text-[12px]">{r.resp ? formatSize(respSizeForRow(r)) : ""}</td>
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
                    <HttpHeaders startLine={`${selected.req.method} ${selected.req.path} HTTP/${selected.req.version}`} headers={selected.req.headers} style={syntaxStyle} />
                    {((selected.req?.body_len || selected.req?.body_base64) ? (
                      <Tabs defaultValue="format">
                        <TabsList>
                          <TabsTrigger value="raw">{t("raw")}</TabsTrigger>
                          <TabsTrigger value="format">{t("format")}</TabsTrigger>
                        </TabsList>
                        <TabsContent value="raw">
                          <BodyPreview
                            headers={selected.req.headers}
                            base64={selected.req.body_base64}
                            mode="raw"
                            jsonIfLooksLike={((selected.req?.is_llm || selected.resp?.is_llm) && !!(selected.resp?.headers.find((h) => h.name.toLowerCase() === "content-type")?.value || "").toLowerCase().includes("ndjson"))}
                            isDark={isDark}
                            style={syntaxStyle}
                          />
                        </TabsContent>
                        <TabsContent value="format">
                          <BodyPreview
                            headers={selected.req.headers}
                            base64={selected.req.body_base64}
                            mode="pretty"
                            jsonIfLooksLike={((selected.req?.is_llm || selected.resp?.is_llm) && !!(selected.resp?.headers.find((h) => h.name.toLowerCase() === "content-type")?.value || "").toLowerCase().includes("ndjson"))}
                            isDark={isDark}
                            style={syntaxStyle}
                          />
                        </TabsContent>
                      </Tabs>
                    ) : null)}
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
                    <HttpHeaders startLine={`HTTP/${selected.resp.version} ${selected.resp.status_code}${selected.resp.reason ? ` ${selected.resp.reason}` : ""}`} headers={selected.resp.headers} style={syntaxStyle} />
                    {(selected.resp.is_llm) ? (
                      <Tabs defaultValue="format">
                        <TabsList>
                          <TabsTrigger value="raw">{t("raw")}</TabsTrigger>
                          <TabsTrigger value="format">{t("format")}</TabsTrigger>
                          <TabsTrigger value="markdown">{t("markdown")}</TabsTrigger>
                        </TabsList>
                        <TabsContent value="raw">
                          <BodyPreview
                            headers={selected.resp.headers}
                            base64={selected.resp.body_base64}
                            mode="raw"
                            aggText={respAgg[selected.id || ""]?.text}
                            isDark={isDark}
                            style={syntaxStyle}
                          />
                        </TabsContent>
                        <TabsContent value="format">
                          <BodyPreview
                            headers={selected.resp.headers}
                            base64={selected.resp.body_base64}
                            mode="pretty"
                            aggText={respAgg[selected.id || ""]?.text}
                            isDark={isDark}
                            style={syntaxStyle}
                          />
                        </TabsContent>
                        <TabsContent value="markdown">
                          <MarkdownView
                            headers={selected.resp.headers}
                            base64={selected.resp.body_base64}
                            aggText={respAgg[selected.id || ""]?.text}
                            style={syntaxStyle}
                            thinkingLabel={t("thinking")}
                            toolCallsLabel={t("tool_calls")}
                          />
                        </TabsContent>
                      </Tabs>
                    ) : (
                      <BodyPreview
                        headers={selected.resp.headers}
                        base64={selected.resp.body_base64}
                        mode={respBodyMode}
                        aggText={respAgg[selected.id || ""]?.text}
                        onToggle={() => setRespBodyMode(m => m === "pretty" ? "raw" : "pretty")}
                        isDark={isDark}
                        style={syntaxStyle}
                      />
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
