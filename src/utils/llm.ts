export function parseLlmMarkdown(text: string) {
  const reasoningBuf: string[] = [];
  const contentBuf: string[] = [];
  const toolCallsFinal: any[] = [];
  const toolCallsDelta: Record<string, { id?: string; index?: number; type?: string; function?: { name?: string; arguments?: string } }> = {};

  const pushText = (v: any, into: string[]) => {
    if (!v) return;
    if (typeof v === "string") {
      into.push(v);
      return;
    }
    if (Array.isArray(v)) {
      for (const item of v) {
        if (typeof item === "string") {
          into.push(item);
          continue;
        }
        if (item && typeof item === "object") {
          if (typeof (item as any).text === "string") {
            into.push((item as any).text);
            continue;
          }
          if (typeof (item as any).content === "string") {
            into.push((item as any).content);
            continue;
          }
          if (typeof (item as any).value === "string") {
            into.push((item as any).value);
            continue;
          }
        }
      }
      return;
    }
    if (typeof v === "object") {
      if (Array.isArray((v as any).content)) {
        for (const x of (v as any).content) {
          if (typeof x === "string") contentBuf.push(x);
          else if (x && typeof x === "object" && typeof (x as any).text === "string") contentBuf.push((x as any).text);
        }
      } else {
        if (typeof (v as any).text === "string") into.push((v as any).text);
        if (typeof (v as any).content === "string") into.push((v as any).content);
        if (typeof (v as any).value === "string") into.push((v as any).value);
      }
    }
  };

  const addToolCallDelta = (tc: any) => {
    if (!tc) return;
    const key = (typeof tc.index === "number" ? `idx:${tc.index}` : tc.id ? `id:${tc.id}` : "one") as string;
    if (!toolCallsDelta[key]) toolCallsDelta[key] = { id: tc.id, index: tc.index, type: tc.type || "function", function: { name: undefined, arguments: "" } };
    const cur = toolCallsDelta[key];
    if (!cur.function) cur.function = { name: "", arguments: "" } as any;
    const name = tc.function?.name || tc.name;
    const argsPart = tc.function?.arguments ?? tc.arguments ?? "";
    if (name && !cur.function!.name) cur.function!.name = name;
    if (typeof argsPart === "string" && argsPart) cur.function!.arguments = (cur.function!.arguments || "") + argsPart;
  };

  const addToolCallFull = (tc: any) => {
    if (!tc) return;
    if (tc.function || (tc.name || tc.arguments)) {
      toolCallsFinal.push(tc.function ? tc : { type: "function", function: { name: tc.name, arguments: tc.arguments } });
      return;
    }
    if (tc.tool_calls) {
      for (const t of tc.tool_calls) addToolCallFull(t);
      return;
    }
    toolCallsFinal.push(tc);
  };

  const addFromObj = (obj: any) => {
    if (!obj || typeof obj !== "object") return;
    if (obj.message && typeof obj.message === "object") {
      const m = obj.message as any;
      pushText(m.thinking, reasoningBuf);
      pushText(m.content, contentBuf);
      if (Array.isArray(m.tool_calls)) {
        for (const tc of m.tool_calls) addToolCallFull(tc);
      }
      if (m.function_call) addToolCallFull({ type: "function", function: m.function_call });
    }
    pushText(obj.reasoning, reasoningBuf);
    pushText(obj.reasoning_content, reasoningBuf);
    if (obj.choices) {
      for (const c of obj.choices) {
        if (c.delta) {
          pushText(c.delta.reasoning, reasoningBuf);
          pushText(c.delta.reasoning_content, reasoningBuf);
          pushText(c.delta.content, contentBuf);
          if (Array.isArray(c.delta.tool_calls)) {
            for (const tc of c.delta.tool_calls) addToolCallDelta(tc);
          }
          if (c.delta.function_call) addToolCallDelta({ type: "function", function: c.delta.function_call, index: 0 });
        }
        if (c.message) {
          pushText(c.message.reasoning, reasoningBuf);
          pushText(c.message.reasoning_content, reasoningBuf);
          pushText(c.message.content, contentBuf);
          if (Array.isArray(c.message.tool_calls)) {
            for (const tc of c.message.tool_calls) addToolCallFull(tc);
          }
          if (c.message.function_call) addToolCallFull({ type: "function", function: c.message.function_call });
        }
        pushText(c.reasoning, reasoningBuf);
        pushText(c.reasoning_content, reasoningBuf);
        pushText(c.text, contentBuf);
        pushText(c.content, contentBuf);
      }
    }
    if (Array.isArray((obj as any).tool_calls)) {
      for (const tc of (obj as any).tool_calls) addToolCallFull(tc);
    }
    if ((obj as any).function_call) addToolCallFull({ type: "function", function: (obj as any).function_call });
    if (Array.isArray((obj as any).parallel_tool_calls)) {
      for (const tc of (obj as any).parallel_tool_calls) addToolCallFull(tc);
    }
    pushText(obj.content, contentBuf);
    pushText(obj.text, contentBuf);
  };

  try {
    const t = text.replace(/\r/g, "");
    if (t.includes("data:")) {
      const lines = t.split("\n");
      for (const raw of lines) {
        const trimmed = raw.trim();
        if (!trimmed || /^[0-9a-fA-F]+$/.test(trimmed)) continue;
        const m = /^(?:[0-9a-fA-F]+\s+)?data:\s*(.*)$/.exec(trimmed);
        if (!m) continue;
        let payload = m[1].trim();
        const lastBrace = payload.lastIndexOf("}");
        if (lastBrace >= 0) payload = payload.slice(0, lastBrace + 1);
        if (!payload || payload === "[DONE]") continue;
        try {
          addFromObj(JSON.parse(payload));
        } catch {}
      }
    } else {
      const trimmed = t.trimStart();
      let parsedWhole = false;
      if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
        try {
          addFromObj(JSON.parse(t));
          parsedWhole = true;
        } catch {}
      }
      if (!parsedWhole) {
        if (t.includes("\n")) {
          const lines = t.split("\n");
          for (const raw of lines) {
            const line = raw.trim();
            if (!line || /^[0-9a-fA-F]+$/.test(line) || line === "0") continue;
            try {
              addFromObj(JSON.parse(line));
            } catch {}
          }
        } else {
          try {
            addFromObj(JSON.parse(t));
          } catch {}
        }
      }
    }
  } catch {}

  const reasoning = reasoningBuf.join("");
  const content = contentBuf.join("");
  Object.values(toolCallsDelta)
    .sort((a, b) => {
      const ai = typeof a.index === "number" ? a.index : 0;
      const bi = typeof b.index === "number" ? b.index : 0;
      return ai - bi;
    })
    .forEach((v) => {
      toolCallsFinal.push({ type: v.type || "function", id: v.id, index: v.index, function: { name: v.function?.name, arguments: v.function?.arguments } });
    });
  return { reasoning, content, toolCalls: toolCallsFinal };
}


