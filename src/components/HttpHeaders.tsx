import { Light as SyntaxHighlighter } from "react-syntax-highlighter";
import { HeaderKV } from "../types/http";
import "../syntax";

type Props = {
  startLine: string;
  headers: HeaderKV[];
  style: any;
};

export default function HttpHeaders({ startLine, headers, style }: Props) {
  const text = [startLine, ...headers.map((h) => `${h.name}: ${h.value}`)].join("\n");
  return (
    <SyntaxHighlighter language="http" style={style} wrapLongLines customStyle={{ margin: 0, background: "transparent", fontSize: 12, width: "100%" }}>
      {text}
    </SyntaxHighlighter>
  );
}


