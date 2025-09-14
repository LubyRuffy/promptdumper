import React, { Suspense, useMemo } from "react";

// 惰性加载 Monaco，避免主包体积与初次渲染成本
const Monaco = React.lazy(() => import("@monaco-editor/react"));

export type BigCodeViewerProps = {
  value: string;
  language?: string;
  height?: number | string;
  theme?: "vs-dark" | "light";
};

export default function BigCodeViewer({ value, language = "json", height = 360, theme = "vs-dark" }: BigCodeViewerProps) {
  // 超大文本禁用语法校验/联想，仅做只读渲染
  const options = useMemo(
    () => ({
      readOnly: true,
      wordWrap: "on" as const,
      minimap: { enabled: false },
      scrollbar: { vertical: "auto", horizontal: "auto" } as const,
      quickSuggestions: false,
      suggestOnTriggerCharacters: false,
      parameterHints: { enabled: false },
      folding: true,
      renderWhitespace: "selection" as const,
      fontSize: 12,
      lineNumbersMinChars: 4,
      smoothScrolling: true,
      tabSize: 2,
    }),
    []
  );

  return (
    <div style={{ height }}>
      <Suspense fallback={<pre className="text-xs w-full whitespace-pre-wrap break-all">{value}</pre>}>
        <Monaco
          height={height}
          theme={theme}
          defaultLanguage={language}
          defaultValue={value}
          options={options}
        />
      </Suspense>
    </div>
  );
}


