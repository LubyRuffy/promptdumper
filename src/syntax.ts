import { Light as SyntaxHighlighter } from "react-syntax-highlighter";
import atomOneLight from "react-syntax-highlighter/dist/esm/styles/hljs/atom-one-light";
import atomOneDark from "react-syntax-highlighter/dist/esm/styles/hljs/atom-one-dark";
import httpLang from "react-syntax-highlighter/dist/esm/languages/hljs/http";
import jsonLang from "react-syntax-highlighter/dist/esm/languages/hljs/json";
import xmlLang from "react-syntax-highlighter/dist/esm/languages/hljs/xml";
import jsLang from "react-syntax-highlighter/dist/esm/languages/hljs/javascript";
import plaintextLang from "react-syntax-highlighter/dist/esm/languages/hljs/plaintext";

// Register once on module evaluation
try {
  SyntaxHighlighter.registerLanguage("http", httpLang);
  SyntaxHighlighter.registerLanguage("json", jsonLang);
  SyntaxHighlighter.registerLanguage("xml", xmlLang);
  SyntaxHighlighter.registerLanguage("plaintext", plaintextLang);
  SyntaxHighlighter.registerLanguage("javascript", jsLang);
} catch {}

export function getSyntaxStyle(isDark: boolean) {
  return isDark ? atomOneDark : atomOneLight;
}


