import { useEffect, useState } from "react";
import { alex } from "@alex/sdk";
import type { WorkspaceListing } from "@alex/coding-agent-shared";
import { AppHeader } from "./components/AppHeader.js";
import { ChatList } from "./components/ChatList.js";
import { Composer } from "./components/Composer.js";
import { useAppStatus } from "./hooks/useAppStatus.js";
import { useChat } from "./hooks/useChat.js";
import { appClient } from "./lib/app-client.js";

export function App(): React.ReactElement {
  const chat = useChat();
  const { status, loading, error } = useAppStatus();
  const [workspace, setWorkspace] = useState<WorkspaceListing | null>(null);
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const listing = await appClient.listWorkspace();
        if (!cancelled) setWorkspace(listing);
      } catch (err) {
        if (!cancelled) setWorkspaceError(err instanceof Error ? err.message : String(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Keep the runtime identity visible — useful when the user needs to know
  // which host answered the SDK calls.
  const runtimeIdentity = (() => {
    const transport = (alex as unknown as { transport?: { id?: string } }).transport;
    return transport?.id ?? "host";
  })();

  const runtimeStatus = chat.status + (workspace ? ` · ${workspace.entries.length} entries` : "");

  return (
    <main>
      <AppHeader status={status} loading={loading} error={error} runtimeStatus={runtimeStatus} />
      {workspaceError && <p className="warning">workspace: {workspaceError}</p>}
      <ChatList messages={chat.messages} />
      <Composer
        value={chat.input}
        running={chat.running}
        onChange={chat.setInput}
        onSubmit={chat.submit}
      />
      <footer>
        <span>runtime: {runtimeIdentity}</span>
        {chat.error && <span className="warning"> · last error: {chat.error}</span>}
      </footer>
    </main>
  );
}
