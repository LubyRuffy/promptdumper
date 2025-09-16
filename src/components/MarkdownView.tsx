import { useDeferredValue, useEffect, useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Light as SyntaxHighlighter } from "react-syntax-highlighter";
import { HeaderKV } from "../types/http";
import { decodeBody } from "../utils/http";
import { parseLlmMarkdown } from "../utils/llm";
import "../syntax";

type Props = {
  headers: HeaderKV[];
  base64?: string;
  aggText?: string;
  style: any;
  thinkingLabel: string;
  toolCallsLabel: string;
};

export default function MarkdownView({ base64, aggText, style, thinkingLabel, toolCallsLabel }: Props) {
  const bytes = decodeBody(base64);
  const raw = aggText ?? (bytes ? new TextDecoder().decode(bytes) : "");
  const deferredRaw = useDeferredValue(raw);
  const { reasoning, content, toolCalls } = useMemo(() => parseLlmMarkdown(deferredRaw), [deferredRaw]);
  const [reasoningOpen, setReasoningOpen] = useState<boolean>(() => !!reasoning && !content);
  const [reasoningUserToggled, setReasoningUserToggled] = useState<boolean>(false);

  function prettyJsonOrNull(text?: string | null): string | null {
    if (!text) return null;
    const t = text.trim();
    if (!(t.startsWith("{") || t.startsWith("["))) return null;
    try {
      return JSON.stringify(JSON.parse(t), null, 2);
    } catch {
      return null;
    }
  }

  const prettyReasoningTopLevel = useMemo(() => prettyJsonOrNull(reasoning), [reasoning]);
  const prettyContentTopLevel = useMemo(() => prettyJsonOrNull(content), [content]);

  function MdCode(props: any) {
    const { inline, className, children } = props as { inline?: boolean; className?: string; children?: any };
    if (inline) {
      return <code className="px-1 py-0.5 bg-gray-100 dark:bg-gray-800 rounded text-[11px]">{children}</code>;
    }
    const rawText = String(children || "").replace(/\n$/, "");
    const langMatch = /language-([\w-]+)/.exec(className || "");
    const specified = (langMatch?.[1] || "").toLowerCase();
    const looksJson = specified === "json" || /^(\s*[\[{])/.test(rawText);
    if (looksJson) {
      let toShow = rawText;
      try {
        toShow = JSON.stringify(JSON.parse(rawText), null, 2);
      } catch {}
      return (
        <SyntaxHighlighter language="json" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
          {toShow}
        </SyntaxHighlighter>
      );
    }
    return <pre className="p-2 bg-gray-100 dark:bg-gray-800 rounded text-[11px] w-full whitespace-pre-wrap break-all"><code>{rawText}</code></pre>;
  }

  useEffect(() => {
    if (!reasoningUserToggled && reasoning && !content) {
      setReasoningOpen(true);
    }
  }, [reasoning, content, reasoningUserToggled]);

  return (
    <div className="rounded-md border bg-gray-50 dark:bg-gray-900/30 p-3 text-[12px] leading-6">
      {reasoning ? (
        <details
          className="mb-2"
          open={reasoningOpen}
          onToggle={(e) => {
            setReasoningOpen((e.target as HTMLDetailsElement).open);
            setReasoningUserToggled(true);
          }}
        >
          <summary className="cursor-pointer select-none text-[12px] font-semibold">{thinkingLabel}</summary>
          <div className="mt-1 text-[12px] leading-6">
            {prettyReasoningTopLevel ? (
              <SyntaxHighlighter language="json" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
                {prettyReasoningTopLevel}
              </SyntaxHighlighter>
            ) : (
              <ReactMarkdown
                remarkPlugins={[remarkGfm]}
                components={{
                  h1: (p: any) => <h3 className="text-[12px] font-semibold my-1" {...p} />,
                  h2: (p: any) => <h4 className="text-[12px] font-semibold my-1" {...p} />,
                  h3: (p: any) => <h5 className="text-[12px] font-semibold my-1" {...p} />,
                  p: (p: any) => <p className="my-1" {...p} />,
                  ul: (p: any) => <ul className="list-disc ml-4 my-1" {...p} />,
                  ol: (p: any) => <ol className="list-decimal ml-4 my-1" {...p} />,
                  code: MdCode,
                }}
              >
                {reasoning}
              </ReactMarkdown>
            )}
          </div>
        </details>
      ) : null}

      {toolCalls && toolCalls.length > 0 ? (
        <div className="mb-2">
          <div className="text-[12px] font-semibold my-1">{toolCallsLabel}</div>
          <div className="space-y-2">
            {toolCalls.map((tc: any, i: number) => (
              <SyntaxHighlighter key={i} language="json" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
                {JSON.stringify(tc, null, 2)}
              </SyntaxHighlighter>
            ))}
          </div>
        </div>
      ) : null}

      {content ? (
        <div className="text-[12px] leading-6">
          {prettyContentTopLevel ? (
            <SyntaxHighlighter language="json" style={style} wrapLongLines customStyle={{ margin: 0, fontSize: 12, width: "100%" }}>
              {prettyContentTopLevel}
            </SyntaxHighlighter>
          ) : (
            <ReactMarkdown
              remarkPlugins={[remarkGfm]}
              components={{
                table: (p: any) => <table className="table-fixed border-collapse my-2" {...p} />,
                thead: (p: any) => <thead className="bg-gray-100 dark:bg-gray-800" {...p} />,
                th: (p: any) => <th className="border px-2 py-1 text-left text-[12px]" {...p} />,
                td: (p: any) => <td className="border px-2 py-1 text-left text-[12px]" {...p} />,
                h1: (p: any) => <h3 className="text-[12px] font-semibold my-1" {...p} />,
                h2: (p: any) => <h4 className="text-[12px] font-semibold my-1" {...p} />,
                h3: (p: any) => <h5 className="text-[12px] font-semibold my-1" {...p} />,
                p: (p: any) => <p className="my-1" {...p} />,
                ul: (p: any) => <ul className="list-disc ml-4 my-1" {...p} />,
                ol: (p: any) => <ol className="list-decimal ml-4 my-1" {...p} />,
                code: MdCode,
              }}
            >
              {content}
            </ReactMarkdown>
          )}
        </div>
      ) : null}
    </div>
  );
}


