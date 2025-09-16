export type HeaderKV = { name: string; value: string };

export type HttpReq = {
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

export type HttpResp = {
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

export type Row = {
  id: string;
  req?: HttpReq;
  resp?: HttpResp;
};


