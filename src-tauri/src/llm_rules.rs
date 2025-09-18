use crate::http_shared::{Header, HttpRequestEvent, HttpResponseEvent};
use base64::Engine as _;
use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct RawHeaderRule {
    #[serde(default)]
    name_regex: Option<String>,
    #[serde(default)]
    value_regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawRuleSide {
    #[serde(default)]
    methods: Option<Vec<String>>, // e.g., ["POST"]
    #[serde(default)]
    path_regex: Option<String>,
    #[serde(default)]
    headers: Option<Vec<RawHeaderRule>>, // all must be satisfied (any header can satisfy each rule)
    #[serde(default)]
    body_contains_any: Option<Vec<String>>, // simple substring contains
}

#[derive(Debug, Clone, Deserialize)]
struct RawLlmRule {
    provider: String,
    #[serde(default)]
    provider_by_port: Option<std::collections::HashMap<u16, String>>, // per-rule override by server port
    #[serde(default)]
    request: Option<RawRuleSide>,
    #[serde(default)]
    response: Option<RawRuleSide>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawLlmRules {
    rules: Vec<RawLlmRule>,
}

#[derive(Debug, Clone)]
struct HeaderRuleCompiled {
    name: Option<Regex>,
    value: Option<Regex>,
}

#[derive(Debug, Clone)]
struct RuleSideCompiled {
    methods: Option<Vec<String>>, // uppercased
    path: Option<Regex>,
    headers: Vec<HeaderRuleCompiled>,
    body_contains_any: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LlmRuleCompiled {
    provider: String,
    provider_by_port: std::collections::HashMap<u16, String>,
    request: Option<RuleSideCompiled>,
    response: Option<RuleSideCompiled>,
}

#[derive(Debug, Clone)]
pub struct LlmRules {
    rules: Vec<LlmRuleCompiled>,
}

const DEFAULT_LLM_RULES_JSON: &str = r#"{
  "rules": [
    {
      "provider": "openai_compatible",
      "provider_by_port": { "1234": "lmstudio", "11434": "ollama" },
      "request": {
        "methods": ["POST"],
        "path_regex": "^/v1/(chat/completions|completions)",
        "body_contains_any": ["\"model\"", "\"messages\"", "\"prompt\""]
      },
      "response": {
        "body_contains_any": ["\"choices\""]
      }
    },
    {
      "provider": "ollama",
      "request": {
        "methods": ["POST"],
        "path_regex": "^/api/(generate|chat)"
      },
      "response": {
        "body_contains_any": ["\"response\"", "\"message\"", "\"model\"", "\"choices\""]
      }
    }
  ]
}"#;

fn compile_header_rule(r: &RawHeaderRule) -> Option<HeaderRuleCompiled> {
    let name = match &r.name_regex {
        Some(s) if !s.is_empty() => match Regex::new(s) {
            Ok(rx) => Some(rx),
            Err(_) => None,
        },
        _ => None,
    };
    let value = match &r.value_regex {
        Some(s) if !s.is_empty() => match Regex::new(s) {
            Ok(rx) => Some(rx),
            Err(_) => None,
        },
        _ => None,
    };
    Some(HeaderRuleCompiled { name, value })
}

fn compile_side(r: &RawRuleSide) -> Option<RuleSideCompiled> {
    let methods = r
        .methods
        .as_ref()
        .map(|v| v.iter().map(|s| s.to_ascii_uppercase()).collect::<Vec<_>>());
    let path = match &r.path_regex {
        Some(s) if !s.is_empty() => match Regex::new(s) {
            Ok(rx) => Some(rx),
            Err(_) => None,
        },
        _ => None,
    };
    let headers_raw = r.headers.clone().unwrap_or_default();
    let mut headers = Vec::new();
    for hr in headers_raw.iter() {
        if let Some(comp) = compile_header_rule(hr) {
            headers.push(comp);
        }
    }
    let body_contains_any = r.body_contains_any.clone().unwrap_or_default();
    Some(RuleSideCompiled {
        methods,
        path,
        headers,
        body_contains_any,
    })
}

fn headers_match(compiled: &RuleSideCompiled, headers: &Vec<Header>) -> bool {
    if compiled.headers.is_empty() {
        return true;
    }
    'rules: for hr in compiled.headers.iter() {
        for h in headers.iter() {
            let name_ok = match &hr.name {
                Some(rx) => rx.is_match(&h.name),
                None => true,
            };
            let val_ok = match &hr.value {
                Some(rx) => rx.is_match(&h.value),
                None => true,
            };
            if name_ok && val_ok {
                continue 'rules;
            }
        }
        return false;
    }
    true
}

fn body_contains_any(compiled: &RuleSideCompiled, body_b64: &Option<String>) -> bool {
    if compiled.body_contains_any.is_empty() {
        return true;
    }
    let mut body = String::new();
    if let Some(b64) = body_b64 {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
            body = String::from_utf8_lossy(&bytes).to_string();
        }
    }
    compiled
        .body_contains_any
        .iter()
        .any(|needle| body.contains(needle))
}

fn compile_rules(raw: RawLlmRules) -> LlmRules {
    let mut rules = Vec::new();
    for rr in raw.rules.into_iter() {
        let request = rr.request.as_ref().and_then(compile_side);
        let response = rr.response.as_ref().and_then(compile_side);
        let provider_by_port = rr.provider_by_port.unwrap_or_default();
        rules.push(LlmRuleCompiled {
            provider: rr.provider,
            provider_by_port,
            request,
            response,
        });
    }
    LlmRules { rules }
}

pub fn load_llm_rules_from_json_str(s: &str) -> Option<LlmRules> {
    let raw: RawLlmRules = serde_json::from_str(s).ok()?;
    Some(compile_rules(raw))
}

pub fn load_llm_rules() -> LlmRules {
    if let Ok(s) = std::fs::read_to_string("llm_rules.json") {
        if let Some(r) = load_llm_rules_from_json_str(&s) {
            return r;
        }
    }
    load_llm_rules_from_json_str(DEFAULT_LLM_RULES_JSON).unwrap_or(LlmRules { rules: Vec::new() })
}

impl LlmRules {
    pub fn match_request(&self, evt: &HttpRequestEvent) -> Option<String> {
        for r in &self.rules {
            if let Some(side) = &r.request {
                if let Some(ms) = &side.methods {
                    if !ms.iter().any(|m| m == &evt.method.to_ascii_uppercase()) {
                        continue;
                    }
                }
                if let Some(rx) = &side.path {
                    if !rx.is_match(&evt.path) {
                        continue;
                    }
                }
                if !headers_match(side, &evt.headers) {
                    continue;
                }
                if !body_contains_any(side, &evt.body_base64) {
                    continue;
                }
                if let Some(p) = r.provider_by_port.get(&evt.dst_port) {
                    return Some(p.clone());
                }
                return Some(r.provider.clone());
            }
        }
        None
    }
    pub fn match_response(&self, evt: &HttpResponseEvent) -> Option<String> {
        for r in &self.rules {
            if let Some(side) = &r.response {
                if !headers_match(side, &evt.headers) {
                    continue;
                }
                if !body_contains_any(side, &evt.body_base64) {
                    continue;
                }
                if let Some(p) = r.provider_by_port.get(&evt.src_port) {
                    return Some(p.clone());
                }
                return Some(r.provider.clone());
            }
        }
        None
    }
    pub fn match_text_only(&self, text: &str) -> Option<String> {
        for r in &self.rules {
            if let Some(side) = &r.response {
                if side
                    .body_contains_any
                    .iter()
                    .any(|needle| text.contains(needle))
                {
                    return Some(r.provider.clone());
                }
            }
        }
        None
    }
}
