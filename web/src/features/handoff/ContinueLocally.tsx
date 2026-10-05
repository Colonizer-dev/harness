// "Continue locally" (issue #738): download a colony's conversation as a txcript Simple JSON
// (colony.json) and show the two commands that bring it back to a developer's machine. Rendered in
// the colony header; the panel is presentational so it can be tested without a DOM.
import { useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { Session } from "../../types";
import { Button, Spinner } from "../../components/ui";
import { IconDownload } from "../../components/icons";
import { HANDOFF_FILE, locallyCommandsFor } from "./handoff";

/** Saves a string as a file download, the way the server's `Content-Disposition: attachment` would. */
function saveText(name: string, text: string): void {
  if (typeof document === "undefined") return;
  const url = URL.createObjectURL(new Blob([text], { type: "application/json" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/** The instructions the download reveals: the file, the repository, and the two commands. */
export function ContinueLocallyPanel({ lines, repo, file = HANDOFF_FILE }: { lines: string[]; repo: string; file?: string }) {
  return (
    <div className="w-full rounded-xl border border-border bg-panel-2 px-3 py-2.5 text-[12.5px]">
      <p className="m-0 text-muted">
        Downloaded <code className="font-mono text-text">{file}</code>. In a checkout of <span className="font-mono text-text">{repo}</span>, run:
      </p>
      <ol className="m-0 mt-1.5 list-none space-y-1 p-0">
        {lines.map((line, i) => (
          <li key={i} className="flex items-start gap-2">
            <span className="shrink-0 text-faint">{i + 1}.</span>
            <code className="min-w-0 select-all font-mono text-[12px] text-text [overflow-wrap:anywhere]">{line}</code>
          </li>
        ))}
      </ol>
      <p className="m-0 mt-1.5 text-[11.5px] text-faint">The colony keeps its own copy; this is a snapshot to continue from.</p>
    </div>
  );
}

export function ContinueLocally({ session }: { session: Session }) {
  const api = useApi();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const [lines, setLines] = useState<string[]>(() => locallyCommandsFor(session));

  const download = async () => {
    setBusy(true);
    try {
      const doc = await api.sessionHandoff(session.id);
      saveText(HANDOFF_FILE, JSON.stringify(doc, null, 2));
      // The exported transcript may reveal the agent that wrote it, which beats the colony's setting.
      setLines(locallyCommandsFor(session, doc));
      setOpen(true);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Button disabled={busy} onClick={() => void download()} title={`Download this colony's conversation as ${HANDOFF_FILE} and continue it locally`}>
        {busy ? <Spinner /> : <IconDownload size={15} />} Continue locally
      </Button>
      {open && <ContinueLocallyPanel lines={lines} repo={session.repo} />}
    </>
  );
}
