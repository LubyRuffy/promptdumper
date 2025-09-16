import { Light as SyntaxHighlighter } from "react-syntax-highlighter";
import BigCodeViewer from "./ui/BigCodeViewer";
import { HeaderKV } from "../types/http";
import { decodeBody } from "../utils/http";
import "../syntax";

type Props = {
  headers: HeaderKV[];
  base64?: string;
  mode?: "pretty" | "raw";
  aggText?: string;
  onToggle?: () => void;
  jsonIfLooksLike?: boolean;
  isDark: boolean;
  style: any;
};

export default function BodyPreview({ headers, base64, mode = "pretty", aggText, onToggle, jsonIfLooksLike, isDark, style }: Props) {
  const HIGHLIGHT_HEAVY_THRESHOLD = 40000;
  const bytes = decodeBody(base64);
  const ct = headers.find((h) => h.name.toLowerCase() === "content-type")?.value || "";
  const text = aggText ?? (bytes ? new TextDecoder().decode(bytes) : "");
  if (!text) return null;

  const toggleNode = onToggle ? (
    <div className="absolute right-2 top-2 z-10">
      <button className="inline-flex items-center justify-center h-8 w-8" onClick={onToggle}>
        {mode === "pretty" ? "{}" : "<>"}
      </button>
    </div>
  ) : null;

  const wrap = (child: React.ReactNode) => (
    <div className="relative">
      {toggleNode}
      {child}
    </div>
  );

  if (ct.includes("text/event-stream")) {
    if (mode === "raw") {
      return wrap(
        <SyntaxHighlighter language="plaintext" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
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
      } else if (payload.startsWith("{") || payload.startsWith("[")) {
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
      if (line.startsWith("data:")) {
        buf.push(line.slice(5).trimStart());
        continue;
      }
      if (line.trim() === "") {
        flush();
        continue;
      }
    }
    flush();
    const shouldRenderPlain = events.length > 80 || text.length > HIGHLIGHT_HEAVY_THRESHOLD;
    if (shouldRenderPlain) {
      const merged = events.map((e) => e.pretty).join("\n\n");
      const jsonRatio = events.length ? events.filter((e) => e.kind === "json").length / events.length : 0;
      const lang = jsonRatio > 0.5 ? "json" : "plaintext";
      return wrap(<BigCodeViewer value={merged} language={lang as any} theme={isDark ? "vs-dark" : "light"} height={480} />);
    }
    return wrap(
      <div className="space-y-2">
        {events.map((ev, i) => (
          <SyntaxHighlighter
            key={i}
            language={ev.kind === "json" ? "json" : "plaintext"}
            style={style}
            wrapLongLines
            customStyle={{ margin: 0, fontSize: 12, width: "100%" }}
          >
            {ev.pretty}
          </SyntaxHighlighter>
        ))}
      </div>
    );
  }

  if (ct.includes("application/json")) {
    if (mode === "raw") {
      return wrap(<pre className="text-xs w-full whitespace-pre-wrap break-all">{text}</pre>);
    }
    try {
      const pretty = JSON.stringify(JSON.parse(text), null, 2);
      if (pretty.length > HIGHLIGHT_HEAVY_THRESHOLD) {
        return wrap(<BigCodeViewer value={pretty} language="json" theme={isDark ? "vs-dark" : "light"} height={480} />);
      }
      return wrap(
        <SyntaxHighlighter language="json" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
          {pretty}
        </SyntaxHighlighter>
      );
    } catch {}
  }

  if (jsonIfLooksLike && mode !== "raw") {
    const trimmed = text.trimStart();
    if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
      try {
        const parsed = JSON.parse(text);
        const pretty = JSON.stringify(parsed, null, 2);
        if (pretty.length > HIGHLIGHT_HEAVY_THRESHOLD) {
          return wrap(<BigCodeViewer value={pretty} language="json" theme={isDark ? "vs-dark" : "light"} height={480} />);
        }
        return wrap(
          <SyntaxHighlighter language="json" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
            {pretty}
          </SyntaxHighlighter>
        );
      } catch {}
    }
  }

  if (ct.includes("application/javascript") || ct.includes("text/javascript") || ct.includes("ecmascript")) {
    if (mode === "raw") {
      return wrap(<pre className="text-xs w-full whitespace-pre-wrap break-all">{text}</pre>);
    }
    return wrap(
      <SyntaxHighlighter language="javascript" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
        {text}
      </SyntaxHighlighter>
    );
  }

  if (ct.includes("application/x-www-form-urlencoded")) {
    if (mode === "raw") {
      return wrap(
        <SyntaxHighlighter language="plaintext" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
          {text}
        </SyntaxHighlighter>
      );
    }
    const formatted = text
      .split("&")
      .filter(Boolean)
      .map((seg) => {
        const [k, v = ""] = seg.split("=");
        const dk = (() => {
          try {
            return decodeURIComponent((k || "").replace(/\+/g, " "));
          } catch {
            return k || "";
          }
        })();
        const dv = (() => {
          try {
            return decodeURIComponent((v || "").replace(/\+/g, " "));
          } catch {
            return v || "";
          }
        })();
        return `${dk}: ${dv}`;
      })
      .join("\n");
    return wrap(
      <SyntaxHighlighter language="plaintext" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
        {formatted || text}
      </SyntaxHighlighter>
    );
  }

  if (ct.includes("text/html") || ct.includes("application/xml") || ct.includes("text/xml")) {
    if (mode === "raw") {
      return wrap(<pre className="text-xs w-full whitespace-pre-wrap break-all">{text}</pre>);
    }
    return wrap(
      <SyntaxHighlighter language="xml" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
        {text}
      </SyntaxHighlighter>
    );
  }

  if (ct.startsWith("text/")) {
    if (mode === "raw") {
      return wrap(<pre className="text-xs w-full whitespace-pre-wrap break-all">{text}</pre>);
    }
    return wrap(
      <SyntaxHighlighter language="plaintext" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
        {text}
      </SyntaxHighlighter>
    );
  }

  if (ct.includes("application/x-ndjson") || ct.includes("application/ndjson")) {
    if (mode === "raw") {
      return wrap(
        <SyntaxHighlighter language="plaintext" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
          {text}
        </SyntaxHighlighter>
      );
    }
    const lines = text.replace(/\r/g, "").split("\n").filter(Boolean);
    const shouldRenderPlain = lines.length > 200 || text.length > HIGHLIGHT_HEAVY_THRESHOLD;
    if (shouldRenderPlain) {
      const mergedLines = lines.map((ln) => ln.trim()).filter((line) => line && !/^[0-9a-fA-F]+$/.test(line) && line !== "0");
      const merged = mergedLines.join("\n");
      const scanCount = Math.min(mergedLines.length, 200);
      const jsonLike = mergedLines.slice(0, scanCount).filter((l) => l.startsWith("{") || l.startsWith("[")).length / (scanCount || 1);
      const lang = jsonLike > 0.5 ? "json" : "plaintext";
      return wrap(<BigCodeViewer value={merged} language={lang as any} theme={isDark ? "vs-dark" : "light"} height={480} />);
    }
    return wrap(
      <div className="space-y-2">
        {lines.map((ln, i) => {
          let line = ln.trim();
          if (!line) return null;
          if (/^[0-9a-fA-F]+$/.test(line) || line === "0") return null;
          const hexPrefix = /^([0-9a-fA-F]+)\s+(\{.*)$/.exec(line);
          if (hexPrefix) line = hexPrefix[2];
          try {
            const obj = JSON.parse(line);
            return (
              <SyntaxHighlighter key={i} language="json" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
                {JSON.stringify(obj, null, 2)}
              </SyntaxHighlighter>
            );
          } catch {
            return (
              <SyntaxHighlighter key={i} language="plaintext" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
                {line}
              </SyntaxHighlighter>
            );
          }
        })}
      </div>
    );
  }

  if (mode === "pretty") {
    const trimmed = text.trimStart();
    if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
      try {
        const parsed = JSON.parse(text);
        const pretty = JSON.stringify(parsed, null, 2);
        if (pretty.length > HIGHLIGHT_HEAVY_THRESHOLD) {
          return wrap(<BigCodeViewer value={pretty} language="json" theme={isDark ? "vs-dark" : "light"} height={480} />);
        }
        return wrap(
          <SyntaxHighlighter language="json" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
            {pretty}
          </SyntaxHighlighter>
        );
      } catch {}
    }
  }

  if (aggText && aggText.length > 0) {
    if (text.length > HIGHLIGHT_HEAVY_THRESHOLD) {
      const firstNonEmpty = text.match(/[\S\s]/) ? text.trimStart() : "";
      const looksJson = firstNonEmpty.startsWith("{") || firstNonEmpty.startsWith("[");
      return wrap(<BigCodeViewer value={text} language={(looksJson ? "json" : "plaintext") as any} theme={isDark ? "vs-dark" : "light"} height={480} />);
    }
    return wrap(
      <SyntaxHighlighter language="plaintext" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
        {text}
      </SyntaxHighlighter>
    );
  }

  const hexLines: string[] = [];
  const arr = bytes || new Uint8Array();
  for (let i = 0; i < arr.length; i += 16) {
    const slice = arr.slice(i, i + 16);
    const hex = Array.from(slice)
      .map((b) => b.toString(16).padStart(2, "0"))
      .join(" ");
    const ascii = Array.from(slice)
      .map((b) => (b >= 32 && b <= 126 ? String.fromCharCode(b) : "."))
      .join("");
    hexLines.push(i.toString(16).padStart(8, "0") + "  " + hex.padEnd(16 * 3 - 1, " ") + "  |" + ascii + "|");
  }
  return wrap(<pre className="text-xs font-mono w-full whitespace-pre-wrap break-all">{hexLines.join("\n")}</pre>);
}


