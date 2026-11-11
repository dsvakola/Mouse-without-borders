// These mirror the Rust structs in src-tauri/src/lib.rs (camelCase).

export type Role = "master" | "secondary";

export interface Machine {
  id: string;
  name: string;
  isLocal: boolean;
  connected: boolean;
  col: number;
  row: number;
}

export interface AppState {
  role: Role;
  machineName: string;
  sharedPin: string;
  twoRow: boolean;
  shareClipboard: boolean;
  transferFiles: boolean;
  masterIp: string;
  machines: Machine[];
  connectedTo: string | null;
  engineError: string | null;
  log: string[];
}

export interface Cell {
  id: string;
  col: number;
  row: number;
}

export interface ApplyRequest {
  role: Role;
  shareClipboard: boolean;
  transferFiles: boolean;
  masterIp: string;
  twoRow: boolean;
  cells: Cell[];
}

export interface AppInfo {
  name: string;
  version: string;
  developers: string;
  organisation: string;
  website: string;
  email: string;
  license: string;
}
