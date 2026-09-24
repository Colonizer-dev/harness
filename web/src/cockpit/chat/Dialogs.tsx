// Hand-offs from a conversation: launch a colony, schedule a loop, or file a GitHub issue. Each is
// a modal the operator reviews and confirms; nothing leaves the mothership before that.
import { useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode } from "react";
import { errorMessage, useToast } from "../../context";
import { Avatar } from "../../components/Avatar";
import { Button, Spinner } from "../../components/ui";
import type { LoopCadence, Repo } from "../../types";
import { Select, type ListItem } from "./Popover";

function Shell({ label, title, children, onClose, footer }: { label: string; title: ReactNode; children: ReactNode; onClose: () => void; footer: (close: () => void) => ReactNode }): ReactElement {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (ref.current && !ref.current.open) ref.current.showModal();
  }, []);
  const close = () => ref.current?.close();
  return (
    <dialog ref={ref} onClose={onClose} aria-label={label} className="m-auto w-[min(720px,calc(100vw-24px))] rounded-2xl border border-border bg-panel p-0 text-text backdrop:bg-black/50">
      <div className="flex flex-col gap-3.5 p-5">
        <h2 className="m-0 text-[16px] font-semibold">{title}</h2>
        {children}
        <div className="flex justify-end gap-2">{footer(close)}</div>
      </div>
    </dialog>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }): ReactElement {
  return (
    <div className="flex flex-col gap-1 text-[12.5px] text-muted">
      <span>{label}</span>
      {children}
    </div>
  );
}

function useRepoItems(repos: readonly Repo[], org: string | null, avatarFor: (org: string) => string | null): ListItem[] {
  return useMemo(
    () =>
      repos
        .filter((r) => !r.archived && (!org || r.full_name.toLowerCase().startsWith(`${org.toLowerCase()}/`)))
        .map((r) => {
          const owner = r.full_name.split("/")[0];
          return { id: r.full_name, label: r.full_name, hint: r.description ?? undefined, leading: <Avatar name={owner} src={avatarFor(owner)} size={18} rounded="md" /> };
        }),
    [repos, org, avatarFor],
  );
}

const textareaClass = "scroll-thin rounded-lg border border-border bg-transparent px-2.5 py-1.5 font-mono text-[12.5px] text-text outline-none focus:border-accent";
const inputClass = "rounded-lg border border-border bg-transparent px-2.5 py-1.5 text-[13px] text-text outline-none focus:border-accent";

/** The conversation as a colony: pick the repository, edit the prefilled instructions, launch. */
export function HandoffDialog({
  repos,
  org,
  avatarFor,
  initial,
  onClose,
  onLaunch,
}: {
  repos: readonly Repo[];
  org: string | null;
  avatarFor: (org: string) => string | null;
  initial: { instructions: string; repo: string };
  onClose: () => void;
  onLaunch: (repo: string, instructions: string) => Promise<void>;
}): ReactElement {
  const toast = useToast();
  const items = useRepoItems(repos, org, avatarFor);
  const [repo, setRepo] = useState(initial.repo);
  const [instructions, setInstructions] = useState(initial.instructions);
  const [launching, setLaunching] = useState(false);
  return (
    <Shell
      label="turn into a colony"
      title="Turn into a colony"
      onClose={onClose}
      footer={(close) => (
        <>
          <Button onClick={close}>Cancel</Button>
          <Button
            variant="primary"
            disabled={!repo || !instructions.trim() || launching}
            onClick={() => {
              setLaunching(true);
              onLaunch(repo, instructions.trim())
                .catch((e) => toast(errorMessage(e), "error"))
                .finally(() => setLaunching(false));
            }}
          >
            {launching && <Spinner />} Launch colony
          </Button>
        </>
      )}
    >
      <Field label="Repository">
        <Select value={repo || null} items={items} onChange={(i) => setRepo(i.id)} ariaLabel="repository" placeholder="Choose a repository…" width={480} />
      </Field>
      <Field label="Instructions">
        <textarea value={instructions} onChange={(e) => setInstructions(e.target.value)} rows={12} aria-label="instructions" className={textareaClass} />
      </Field>
    </Shell>
  );
}

const CADENCES: (ListItem & { cadence: LoopCadence })[] = [
  { id: "hourly", label: "Every hour", cadence: { every: "interval", minutes: 60 } },
  { id: "6h", label: "Every 6 hours", cadence: { every: "interval", minutes: 360 } },
  { id: "daily9", label: "Daily at 09:00", cadence: { every: "daily", hour: 9, minute: 0 } },
  { id: "daily18", label: "Daily at 18:00", cadence: { every: "daily", hour: 18, minute: 0 } },
  { id: "self", label: "Self-paced", hint: "The colony decides when to run again", cadence: { every: "self_paced" } },
];

/** A scheduled colony from the conversation: name, repository, how often, and the prompt. */
export function LoopDialog({
  repos,
  org,
  avatarFor,
  initial,
  onClose,
  onCreate,
}: {
  repos: readonly Repo[];
  org: string | null;
  avatarFor: (org: string) => string | null;
  initial: { name: string; prompt: string; repo: string };
  onClose: () => void;
  onCreate: (loop: { name: string; repo: string; prompt: string; cadence: LoopCadence }) => Promise<void>;
}): ReactElement {
  const toast = useToast();
  const items = useRepoItems(repos, org, avatarFor);
  const [name, setName] = useState(initial.name);
  const [repo, setRepo] = useState(initial.repo);
  const [cadence, setCadence] = useState("daily9");
  const [prompt, setPrompt] = useState(initial.prompt);
  const [saving, setSaving] = useState(false);
  return (
    <Shell
      label="create a loop"
      title="Create a loop"
      onClose={onClose}
      footer={(close) => (
        <>
          <Button onClick={close}>Cancel</Button>
          <Button
            variant="primary"
            disabled={!name.trim() || !repo || !prompt.trim() || saving}
            onClick={() => {
              setSaving(true);
              onCreate({ name: name.trim(), repo, prompt: prompt.trim(), cadence: CADENCES.find((c) => c.id === cadence)!.cadence })
                .catch((e) => toast(errorMessage(e), "error"))
                .finally(() => setSaving(false));
            }}
          >
            {saving && <Spinner />} Create loop
          </Button>
        </>
      )}
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Name">
          <input value={name} onChange={(e) => setName(e.target.value)} aria-label="loop name" className={inputClass} />
        </Field>
        <Field label="How often">
          <Select value={cadence} items={CADENCES} onChange={(i) => setCadence(i.id)} ariaLabel="cadence" searchable={false} width={280} />
        </Field>
      </div>
      <Field label="Repository">
        <Select value={repo || null} items={items} onChange={(i) => setRepo(i.id)} ariaLabel="loop repository" placeholder="Choose a repository…" width={480} />
      </Field>
      <Field label="What each run does">
        <textarea value={prompt} onChange={(e) => setPrompt(e.target.value)} rows={10} aria-label="loop prompt" className={textareaClass} />
      </Field>
    </Shell>
  );
}

/** A reply as a GitHub issue, filed by the mothership's gh after the operator confirms. */
export function IssueDialog({
  repos,
  org,
  avatarFor,
  initial,
  onClose,
  onFile,
}: {
  repos: readonly Repo[];
  org: string | null;
  avatarFor: (org: string) => string | null;
  initial: { title: string; body: string; repo: string };
  onClose: () => void;
  onFile: (issue: { repo: string; title: string; body: string }) => Promise<void>;
}): ReactElement {
  const toast = useToast();
  const items = useRepoItems(repos, org, avatarFor);
  const [repo, setRepo] = useState(initial.repo);
  const [title, setTitle] = useState(initial.title);
  const [body, setBody] = useState(initial.body);
  const [filing, setFiling] = useState(false);
  return (
    <Shell
      label="create a GitHub issue"
      title="Create a GitHub issue"
      onClose={onClose}
      footer={(close) => (
        <>
          <span className="mr-auto self-center text-[12px] text-faint">{repo ? `Filed on ${repo} with the mothership's GitHub login.` : ""}</span>
          <Button onClick={close}>Cancel</Button>
          <Button
            variant="primary"
            disabled={!repo || !title.trim() || filing}
            onClick={() => {
              setFiling(true);
              onFile({ repo, title: title.trim(), body })
                .catch((e) => toast(errorMessage(e), "error"))
                .finally(() => setFiling(false));
            }}
          >
            {filing && <Spinner />} Create issue
          </Button>
        </>
      )}
    >
      <Field label="Repository">
        <Select value={repo || null} items={items} onChange={(i) => setRepo(i.id)} ariaLabel="issue repository" placeholder="Choose a repository…" width={480} />
      </Field>
      <Field label="Title">
        <input value={title} onChange={(e) => setTitle(e.target.value.replace(/\n/g, " "))} maxLength={256} aria-label="issue title" className={inputClass} />
      </Field>
      <Field label="Body (Markdown)">
        <textarea value={body} onChange={(e) => setBody(e.target.value)} rows={12} aria-label="issue body" className={textareaClass} />
      </Field>
    </Shell>
  );
}

/** A stored image full size; Esc or a click anywhere closes it. */
export function ImageLightbox({ src, label, width, height, onClose }: { src: string; label: string; width?: number; height?: number; onClose: () => void }): ReactElement {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (ref.current && !ref.current.open) ref.current.showModal();
  }, []);
  return (
    <dialog
      ref={ref}
      onClose={onClose}
      onClick={() => ref.current?.close()}
      aria-label={`image ${label}`}
      className="m-auto max-h-[calc(100vh-32px)] max-w-[calc(100vw-32px)] cursor-zoom-out overflow-hidden rounded-xl border border-border bg-panel p-0 text-text backdrop:bg-black/75"
    >
      <img src={src} alt={label} className="block max-h-[calc(100vh-72px)] max-w-[calc(100vw-32px)] object-contain" />
      <div className="flex items-center gap-2 px-3 py-1.5 text-[12px] text-muted">
        <span className="min-w-0 flex-1 truncate">{label}</span>
        {width && height ? <span className="tabular-nums text-faint">{`${width}×${height}`}</span> : null}
        <a href={src} target="_blank" rel="noopener" onClick={(e) => e.stopPropagation()} className="text-accent hover:underline">
          Open original
        </a>
      </div>
    </dialog>
  );
}
