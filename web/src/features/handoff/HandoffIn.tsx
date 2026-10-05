// "Continue in a colony" (issue #738): hand a local agent session to a colony. Pick a txcript
// Simple JSON export and name the repository; the branch and title the file carries prefill. Shown
// on the Launch view, beside the ordinary launcher.
import { useRef, useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { Session } from "../../types";
import { Button, Spinner, cx, inputClass } from "../../components/ui";
import { IconPlus } from "../../components/icons";
import { MAX_HANDOFF_BYTES, parseHandoffFile, repoValid } from "./handoff";
import type { SimpleTranscript } from "./types";

export function HandoffIn({ onCreated }: { onCreated: (session: Session) => void }) {
  const api = useApi();
  const toast = useToast();
  const fileInput = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [fileName, setFileName] = useState<string | null>(null);
  const [transcript, setTranscript] = useState<SimpleTranscript | null>(null);
  const [repo, setRepo] = useState("");
  const [branch, setBranch] = useState("");
  const [title, setTitle] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  const reset = () => {
    setFileName(null);
    setTranscript(null);
    setRepo("");
    setBranch("");
    setTitle("");
    setError(null);
    if (fileInput.current) fileInput.current.value = "";
  };

  const pick = async (file: File | undefined) => {
    setError(null);
    if (!file) return;
    // Refuse an oversize file before reading it, so a huge download never has to land in memory.
    if (file.size > MAX_HANDOFF_BYTES) {
      setTranscript(null);
      setError(`That file is larger than ${Math.round(MAX_HANDOFF_BYTES / (1024 * 1024))} MiB — the limit for an uploaded session.`);
      return;
    }
    const parsed = parseHandoffFile(await file.text());
    if ("error" in parsed) {
      setTranscript(null);
      setError(parsed.error);
      return;
    }
    setFileName(file.name);
    setTranscript(parsed.transcript);
    setBranch(parsed.branch ?? "");
    setTitle(parsed.title ?? "");
  };

  const launch = async () => {
    if (!transcript) return;
    setStarting(true);
    setError(null);
    try {
      const session = await api.handoffSession({
        repo: repo.trim(),
        branch: branch.trim() || undefined,
        title: title.trim() || undefined,
        transcript,
      });
      toast(session.status === "queued" ? `Queued on ${repo.trim()} — it starts when a colony finishes` : `Colony launched on ${repo.trim()}`);
      reset();
      setOpen(false);
      onCreated(session);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setStarting(false);
    }
  };

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="flex w-full cursor-pointer items-center gap-2.5 rounded-xl border border-dashed border-border-strong px-3 py-2.5 text-left text-muted hover:bg-panel-2 hover:text-text"
      >
        <IconPlus size={15} className="shrink-0" />
        <span className="min-w-0 flex-1">
          <span className="block text-body font-medium text-text">Continue in a colony</span>
          <span className="block text-small">Upload a txcript Simple JSON export from Claude Code, Codex or OpenCode</span>
        </span>
      </button>
    );
  }

  const canLaunch = transcript !== null && repoValid(repo) && !starting;

  return (
    <div className="space-y-2.5 rounded-xl border border-border bg-panel p-3 shadow-[var(--shadow)]">
      <div className="flex items-center justify-between gap-2">
        <span className="text-body font-medium">Continue a local session in a colony</span>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => {
            reset();
            setOpen(false);
          }}
        >
          Cancel
        </Button>
      </div>
      <input
        ref={fileInput}
        type="file"
        accept="application/json,.json"
        aria-label="Session export"
        onChange={(e) => void pick(e.target.files?.[0])}
        className="block w-full cursor-pointer text-small-lg text-muted file:mr-2 file:cursor-pointer file:rounded-md file:border file:border-border file:bg-panel-2 file:px-2.5 file:py-1 file:text-small-lg file:text-text"
      />
      {fileName && <p className="m-0 truncate font-mono text-meta-lg text-faint" title={fileName}>{fileName}</p>}
      <input
        value={repo}
        onChange={(e) => setRepo(e.target.value)}
        placeholder="owner/repository"
        aria-label="Repository"
        className={cx(inputClass, "font-mono text-body-sm")}
      />
      <div className="flex gap-2">
        <input
          value={branch}
          onChange={(e) => setBranch(e.target.value)}
          placeholder="Branch (optional)"
          aria-label="Branch"
          className={cx(inputClass, "min-w-0 flex-1 font-mono text-body-sm")}
        />
        <input
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="Title (optional)"
          aria-label="Title"
          className={cx(inputClass, "min-w-0 flex-1 text-body-sm")}
        />
      </div>
      {error && (
        <p role="alert" className="m-0 text-small-lg text-err [overflow-wrap:anywhere]">
          {error}
        </p>
      )}
      <Button variant="primary" className="w-full" disabled={!canLaunch} onClick={launch}>
        {starting ? <Spinner /> : <IconPlus size={15} />} Launch colony
      </Button>
    </div>
  );
}
