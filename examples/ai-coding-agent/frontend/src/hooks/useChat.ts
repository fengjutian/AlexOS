import { useCallback, useState } from "react";
import type { AgentEvent } from "@alex/sdk";
import { runCodingAgent } from "../lib/chat-agent.js";
import type { ChatMessage } from "../types/chat.js";

interface UseChatResult {
  messages: ChatMessage[];
  input: string;
  setInput: (value: string) => void;
  running: boolean;
  status: string;
  error: string | null;
  submit: (prompt: string) => Promise<void>;
  reset: () => void;
}

function newId(): string {
  return `m_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 7)}`;
}

function appendContent(messages: ChatMessage[], id: string, delta: string): ChatMessage[] {
  return messages.map((message) => (message.id === id ? { ...message, content: message.content + delta } : message));
}

export function useChat(): UseChatResult {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [running, setRunning] = useState(false);
  const [status, setStatus] = useState("Ready");
  const [error, setError] = useState<string | null>(null);

  const submit = useCallback(
    async (prompt: string) => {
      const trimmed = prompt.trim();
      if (!trimmed || running) return;
      setInput("");
      setError(null);
      setRunning(true);
      setStatus("Submitting…");
      setMessages((current) => [
        ...current,
        { id: newId(), role: "user", content: trimmed },
        { id: newId(), role: "assistant", content: "" },
      ]);
      const controller = new AbortController();
      try {
        const handleEvent = (event: AgentEvent): void => {
          if (event.type === "modelDelta") {
            setMessages((current) => {
              const last = current[current.length - 1];
              if (!last || last.role !== "assistant") return current;
              return appendContent(current, last.id, event.text);
            });
          } else if (event.type === "toolIntent") {
            setStatus(`Tool: ${event.call.name}`);
          } else if (event.type === "error") {
            throw new Error(event.message);
          }
        };
        await runCodingAgent(trimmed, controller.signal, handleEvent);
        setStatus("Completed");
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        setStatus(`Error: ${message}`);
        setMessages((current) => {
          const last = current[current.length - 1];
          if (!last || last.role !== "assistant") return current;
          const fallback = last.content || `运行失败：${message}`;
          return current.map((m) => (m.id === last.id ? { ...m, content: fallback } : m));
        });
      } finally {
        setRunning(false);
      }
    },
    [running],
  );

  const reset = useCallback(() => {
    setMessages([]);
    setStatus("Ready");
    setError(null);
  }, []);

  return { messages, input, setInput, running, status, error, submit, reset };
}
