import { FormEvent, useState } from "react";
import { alex } from "@alex/sdk";

type Message = { role: "user" | "assistant"; content: string };

export function App() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [running, setRunning] = useState(false);
  const [status, setStatus] = useState("Ready · Ollama / qwen3 · filesystem MCP");

  async function submit(event: FormEvent) {
    event.preventDefault();
    const prompt = input.trim();
    if (!prompt || running) return;
    setInput("");
    setRunning(true);
    setMessages((items) => [...items, { role: "user", content: prompt }, { role: "assistant", content: "" }]);
    try {
      const run = await alex.agent.create({
        model: "remote/ollama/qwen3",
        systemPrompt: "Inspect before editing. Work only inside the granted workspace.",
        tools: [
          { binding: "filesystem", name: "read_text_file", idempotent: true },
          { binding: "filesystem", name: "list_directory", idempotent: true },
          { binding: "filesystem", name: "write_text_file", requireApproval: true },
        ],
      }, [{ role: "user", content: prompt }]);
      setStatus(`Running ${run.id}`);
      for await (const item of alex.agent.start(run.id)) {
        if (item.type === "modelDelta") {
          setMessages((items) => items.map((message, index) => index === items.length - 1
            ? { ...message, content: message.content + item.text }
            : message));
        } else if (item.type === "toolIntent") setStatus(`Tool: ${item.call.name}`);
        else if (item.type === "error") throw new Error(item.message);
      }
      setStatus("Completed");
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      setStatus(`Error: ${detail}`);
      setMessages((items) => items.map((message, index) => index === items.length - 1
        ? { ...message, content: message.content || `运行失败：${detail}` }
        : message));
    } finally { setRunning(false); }
  }

  return <main>
    <header><div><span className="eyebrow">ALEX RUNTIME</span><h1>Coding Agent</h1></div><span className="status">{status}</span></header>
    <section className="chat">
      {messages.length === 0 && <div className="empty"><h2>让 Agent 从理解代码开始</h2><p>例如：读取 README，总结项目结构并提出一个小改进。</p></div>}
      {messages.map((message, index) => <article className={message.role} key={index}><b>{message.role === "user" ? "You" : "Alex"}</b><p>{message.content}</p></article>)}
    </section>
    <form onSubmit={submit}><textarea value={input} onChange={(event) => setInput(event.target.value)} placeholder="描述你想完成的开发任务…" /><button disabled={running}>{running ? "运行中" : "发送"}</button></form>
  </main>;
}
