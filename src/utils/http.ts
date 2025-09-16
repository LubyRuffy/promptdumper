import { Row } from "../types/http";

export function decodeBody(base64?: string): Uint8Array | null {
  if (!base64) return null;
  try {
    return Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

export function escapeShellArg(s: string): string {
  return "'" + s.replace(/'/g, "'\\''") + "'";
}

export function buildCurlFromRow(r: Row): string {
  if (!r.req) return "";
  const req = r.req;
  const hostHeader = req.headers.find((h) => h.name.toLowerCase() === "host")?.value || "";
  const host = hostHeader.trim() || `${req.dst_ip}:${req.dst_port}`;
  const path = req.path.startsWith("/") ? req.path : "/" + req.path;
  const forwardedProto = req.headers.find((h) => h.name.toLowerCase() === "x-forwarded-proto")?.value?.toLowerCase() || "";
  const scheme = forwardedProto === "https" || req.dst_port === 443 ? "https" : "http";
  const url = `${scheme}://${host}${path}`;

  const headerArgs = req.headers
    .filter((h) => h.name.toLowerCase() !== "content-length")
    .map((h) => `-H ${escapeShellArg(`${h.name}: ${h.value}`)}`)
    .join(" ");

  const methodArg = `-X ${req.method}`;

  let bodyArg = "";
  if (req.body_base64) {
    const bytes = decodeBody(req.body_base64);
    if (bytes && bytes.length > 0) {
      let bodyText: string = "";
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

export function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}


