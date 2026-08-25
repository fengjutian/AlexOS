import type React from "react";
import type { ChatMessage } from "../types/chat.js";

interface ChatListProps {
  messages: ChatMessage[];
}

export function ChatList({ messages }: ChatListProps): React.ReactElement {
  if (messages.length === 0) {
    return (
      <div className="empty">
        <h2>让 Agent 从理解代码开始</h2>
        <p>例如：读取 README，总结项目结构并提出一个小改进。</p>
      </div>
    );
  }
  return (
    <section className="chat">
      {messages.map((message) => (
        <article className={message.role} key={message.id}>
          <b>{message.role === "user" ? "You" : "Alex"}</b>
          <p>{message.content}</p>
        </article>
      ))}
    </section>
  );
}
