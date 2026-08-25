export type Role = "user" | "assistant";

export interface ChatMessage {
  id: string;
  role: Role;
  content: string;
}

export interface AppStatus {
  service: string;
  workspace: string;
  version: string;
  node: string;
  uptimeMs: number;
}
