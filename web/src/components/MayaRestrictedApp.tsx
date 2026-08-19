import { lazy, Suspense, useCallback, useState } from "react";
import { LogOut, MessageSquarePlus, Pencil } from "lucide-react";
import { useMatch, useNavigate } from "react-router-dom";

import { useSessions } from "../hooks/useSessions";
import { AcpPrefsProvider } from "../lib/acpPrefs";
import { createSession, renameSession, type ServerAbout } from "../lib/api";
import type { MayaRestrictedCreateSessionRequest } from "../lib/types";
import { MainPaneSkeleton } from "./AppShellSkeleton";

const StructuredView = lazy(() =>
  import("./acp/StructuredView").then((module) => ({ default: module.StructuredView })),
);

export const MAYA_PROJECT_PATH = "/home/aaiyer/maya/maya-main";

/** The only create projection emitted by the restricted browser surface. */
export function mayaRestrictedCreateRequest(title: string): MayaRestrictedCreateSessionRequest {
  const trimmed = title.trim();
  return trimmed ? { title: trimmed } : {};
}

export function MayaRestrictedApp({
  about,
  loginRequired,
  onLogout,
}: {
  about: ServerAbout;
  loginRequired: boolean;
  onLogout: () => void;
}) {
  const navigate = useNavigate();
  const sessionMatch = useMatch("/session/:sessionId");
  const activeSessionId = sessionMatch?.params.sessionId ?? null;
  const { sessions, loaded, error, injectSession, refresh } = useSessions();
  const [newTitle, setNewTitle] = useState("");
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  const createChat = useCallback(async () => {
    if (creating) return;
    setCreating(true);
    setCreateError(null);
    const result = await createSession(mayaRestrictedCreateRequest(newTitle));
    setCreating(false);
    if (!result.ok || !result.session) {
      setCreateError(result.error ?? "Could not create chat");
      return;
    }
    setNewTitle("");
    injectSession(result.session);
    navigate(`/session/${encodeURIComponent(result.session.id)}`);
  }, [creating, injectSession, navigate, newTitle]);

  const rename = useCallback(
    async (id: string, currentTitle: string) => {
      const next = window.prompt("Rename chat", currentTitle)?.trim();
      if (!next || next === currentTitle) return;
      const result = await renameSession(id, next);
      if (!result.ok) {
        setCreateError(result.message ?? "Could not rename chat");
        return;
      }
      await refresh();
    },
    [refresh],
  );

  const active = sessions.find((session) => session.id === activeSessionId) ?? null;

  return (
    <AcpPrefsProvider
      value={{ showToolDurations: about.acp_show_tool_durations, replayEvents: about.acp_replay_events }}
    >
      <div className="flex h-dvh min-h-0 bg-surface-950 text-text-primary">
        <aside className="flex w-72 shrink-0 flex-col border-r border-surface-800 bg-surface-900">
          <header className="border-b border-surface-800 px-4 py-4">
            <div className="text-sm font-semibold">Maya Codex</div>
            <div className="mt-1 truncate text-xs text-text-dim" title={MAYA_PROJECT_PATH}>
              {MAYA_PROJECT_PATH}
            </div>
          </header>

          <div className="border-b border-surface-800 p-3">
            <label className="block text-xs text-text-secondary" htmlFor="maya-new-title">
              New chat title (optional)
            </label>
            <div className="mt-2 flex gap-2">
              <input
                id="maya-new-title"
                value={newTitle}
                onChange={(event) => setNewTitle(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") void createChat();
                }}
                className="min-w-0 flex-1 rounded-md border border-surface-700 bg-surface-850 px-2 py-1.5 text-sm outline-none focus:border-brand-600"
                placeholder="Auto-name from first turn"
              />
              <button
                type="button"
                onClick={() => void createChat()}
                disabled={creating}
                className="inline-flex items-center gap-1 rounded-md bg-brand-600 px-2.5 py-1.5 text-sm font-medium text-white disabled:opacity-50"
              >
                <MessageSquarePlus className="h-4 w-4" />
                {creating ? "Creating…" : "New"}
              </button>
            </div>
            {createError && <p className="mt-2 text-xs text-red-400">{createError}</p>}
          </div>

          <nav aria-label="Chats" className="min-h-0 flex-1 overflow-y-auto p-2">
            {error && <p className="px-2 py-3 text-xs text-red-400">Could not load chats.</p>}
            {!loaded && <p className="px-2 py-3 text-xs text-text-dim">Loading chats…</p>}
            {loaded && sessions.length === 0 && <p className="px-2 py-3 text-xs text-text-dim">No chats yet.</p>}
            {sessions.map((session) => (
              <div
                key={session.id}
                className={`group flex items-center rounded-md ${
                  session.id === activeSessionId ? "bg-surface-800" : "hover:bg-surface-850"
                }`}
              >
                <button
                  type="button"
                  onClick={() => navigate(`/session/${encodeURIComponent(session.id)}`)}
                  className="min-w-0 flex-1 truncate px-2 py-2 text-left text-sm"
                >
                  {session.title}
                </button>
                <button
                  type="button"
                  aria-label={`Rename ${session.title}`}
                  title="Rename chat"
                  onClick={() => void rename(session.id, session.title)}
                  className="mr-1 rounded p-1 text-text-dim opacity-60 hover:bg-surface-700 hover:text-text-primary group-hover:opacity-100"
                >
                  <Pencil className="h-3.5 w-3.5" />
                </button>
              </div>
            ))}
          </nav>

          {loginRequired && (
            <button
              type="button"
              onClick={onLogout}
              className="m-3 inline-flex items-center justify-center gap-2 rounded-md border border-surface-700 px-3 py-2 text-sm text-text-secondary hover:bg-surface-800"
            >
              <LogOut className="h-4 w-4" />
              Log out
            </button>
          )}
        </aside>

        <main className="min-w-0 flex-1">
          {!activeSessionId ? (
            <div className="flex h-full items-center justify-center text-sm text-text-dim">
              Create or select a Codex chat.
            </div>
          ) : !loaded ? (
            <MainPaneSkeleton />
          ) : !active ? (
            <div className="flex h-full items-center justify-center text-sm text-text-dim">Chat not found.</div>
          ) : (
            <Suspense fallback={<MainPaneSkeleton />}>
              <StructuredView
                sessionId={active.id}
                acpWorkerState={active.acp_worker_state ?? "absent"}
                tool="codex"
                acpAgent={active.acp_agent ?? "codex"}
                archivedAt={null}
                snoozedUntil={null}
                trashedAt={null}
                restricted
              />
            </Suspense>
          )}
        </main>
      </div>
    </AcpPrefsProvider>
  );
}
