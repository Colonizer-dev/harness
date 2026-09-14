// The `ask_user` choice card: every agent question is answered by picking options (plus "Other…").
import type { ToolCallMessagePartProps } from "@assistant-ui/react";
import { createContext, useContext, useId, useMemo, useState } from "react";
import type { AskUserArgs, AskUserResult } from "../sessionStream";
import type { Answers, Question, QuestionOption } from "../types";
import { IconCheck, IconChevronDown, IconQuestion } from "./icons";
import { Button, Spinner, cx } from "./ui";

export interface QuestionActions {
  answer(questionId: string, answers: Answers, response: string | null): boolean;
  submitting: Record<string, true>;
  canAnswer: boolean;
}

export const QuestionActionsContext = createContext<QuestionActions>({
  answer: () => false,
  submitting: {},
  canAnswer: false,
});

interface Draft {
  selected: string[];
  otherOn: boolean;
  otherText: string;
}

const emptyDraft: Draft = { selected: [], otherOn: false, otherText: "" };

function draftComplete(draft: Draft): boolean {
  return draft.selected.length > 0 || (draft.otherOn && draft.otherText.trim().length > 0);
}

export function AskUserCard({ toolCallId, args, result }: ToolCallMessagePartProps<AskUserArgs, AskUserResult>) {
  const questions = args?.questions ?? [];
  if (result) return <AnsweredCard questions={questions} result={result} />;
  return <OpenCard questionId={toolCallId} questions={questions} />;
}

function OpenCard({ questionId, questions }: { questionId: string; questions: Question[] }) {
  const { answer, submitting, canAnswer } = useContext(QuestionActionsContext);
  const [drafts, setDrafts] = useState<Draft[]>(() => questions.map(() => emptyDraft));
  const isSubmitting = Boolean(submitting[questionId]);
  const complete = questions.length > 0 && questions.every((_, i) => draftComplete(drafts[i] ?? emptyDraft));
  const remaining = questions.filter((_, i) => !draftComplete(drafts[i] ?? emptyDraft)).length;

  const setDraft = (index: number, fn: (d: Draft) => Draft) =>
    setDrafts((list) => list.map((d, i) => (i === index ? fn(d) : d)));

  const submit = () => {
    if (!complete || isSubmitting) return;
    const answers: Answers = {};
    questions.forEach((q, i) => {
      const draft = drafts[i] ?? emptyDraft;
      const other = draft.otherOn ? draft.otherText.trim() : "";
      if (q.multi_select) answers[q.question] = other ? [...draft.selected, other] : draft.selected;
      else answers[q.question] = other || draft.selected[0];
    });
    answer(questionId, answers, null);
  };

  return (
    <form
      className="my-1 overflow-hidden rounded-2xl border border-border bg-panel shadow-[var(--shadow)]"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <div className="flex items-center gap-2 border-b border-border bg-accent-soft/60 px-4 py-2.5">
        <span className="grid size-6 place-items-center rounded-full bg-accent text-on-accent">
          <IconQuestion size={14} />
        </span>
        <span className="text-[13px] font-semibold">The agent needs your decision</span>
        <span className="ml-auto text-[12px] text-muted">
          {questions.length === 1 ? "1 question" : `${questions.length} questions`}
        </span>
      </div>

      <div className="divide-y divide-border">
        {questions.map((q, i) => (
          <QuestionSection
            key={`${questionId}-${i}`}
            name={`${questionId}-${i}`}
            question={q}
            draft={drafts[i] ?? emptyDraft}
            disabled={isSubmitting}
            onChange={(fn) => setDraft(i, fn)}
          />
        ))}
      </div>

      <div className="flex flex-wrap items-center gap-3 border-t border-border bg-panel-2/60 px-4 py-3">
        <p className="min-w-0 flex-1 text-[12.5px] text-muted">
          {!canAnswer
            ? "Reconnecting to the colony…"
            : isSubmitting
              ? "Sending your answer to the agent…"
              : complete
                ? "Ready to send."
                : `Answer ${remaining === 1 ? "1 more question" : `${remaining} more questions`} to continue.`}
        </p>
        <Button type="submit" variant="primary" disabled={!complete || isSubmitting || !canAnswer}>
          {isSubmitting ? <Spinner /> : <IconCheck size={15} />}
          Submit answer{questions.length > 1 ? "s" : ""}
        </Button>
      </div>
    </form>
  );
}

function QuestionSection({
  name,
  question,
  draft,
  disabled,
  onChange,
}: {
  name: string;
  question: Question;
  draft: Draft;
  disabled: boolean;
  onChange: (fn: (d: Draft) => Draft) => void;
}) {
  const multi = question.multi_select;
  const headingId = useId();
  const otherInputId = useId();

  const toggle = (label: string) =>
    onChange((d) => {
      if (!multi) return { ...d, selected: [label], otherOn: false };
      return d.selected.includes(label)
        ? { ...d, selected: d.selected.filter((l) => l !== label) }
        : { ...d, selected: [...d.selected, label] };
    });

  const toggleOther = () =>
    onChange((d) => (multi ? { ...d, otherOn: !d.otherOn } : { ...d, selected: [], otherOn: true }));

  return (
    <fieldset className="min-w-0 px-4 py-4" aria-labelledby={headingId} disabled={disabled}>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        {question.header && (
          <span className="rounded-md bg-panel-3 px-2 py-0.5 text-[11px] font-semibold uppercase tracking-wide text-muted">
            {question.header}
          </span>
        )}
        <span className="text-[11.5px] text-faint">{multi ? "Choose any that apply" : "Choose one"}</span>
      </div>
      <h3 id={headingId} className="mb-3 text-[15px] font-semibold leading-snug">
        {question.question}
      </h3>

      <div className="grid gap-2" role={multi ? "group" : "radiogroup"} aria-labelledby={headingId}>
        {/* The card always offers its own free-text "Other…", so drop a model-supplied "Other". */}
        {question.options.filter((option) => option.label.trim().toLowerCase() !== "other").map((option) => (
          <OptionCard
            key={option.label}
            name={name}
            multi={multi}
            option={option}
            checked={draft.selected.includes(option.label)}
            onToggle={() => toggle(option.label)}
          />
        ))}

        <label
          className={cx(
            "group flex cursor-pointer gap-3 relative rounded-xl border p-3 transition-colors has-[input:focus-visible]:ring-2 has-[input:focus-visible]:ring-[var(--accent-ring)]",
            draft.otherOn ? "border-accent bg-accent-soft/50" : "border-dashed border-border-strong hover:bg-panel-2",
          )}
        >
          <input
            type={multi ? "checkbox" : "radio"}
            name={name}
            className="sr-only"
            checked={draft.otherOn}
            onChange={toggleOther}
          />
          <Indicator multi={multi} checked={draft.otherOn} />
          <span className="min-w-0 flex-1">
            <span className="block font-medium">Other…</span>
            {!draft.otherOn && <span className="block text-[13px] text-muted">Type your own answer</span>}
            {draft.otherOn && (
              <input
                id={otherInputId}
                autoFocus
                value={draft.otherText}
                onChange={(e) => onChange((d) => ({ ...d, otherText: e.target.value }))}
                onClick={(e) => e.preventDefault()}
                placeholder="Your answer"
                aria-label={`Other answer for: ${question.question}`}
                className="mt-2 w-full rounded-lg border border-border bg-panel px-3 py-2 text-sm outline-none focus:border-accent focus:ring-2 focus:ring-[var(--accent-ring)]"
              />
            )}
          </span>
        </label>
      </div>
    </fieldset>
  );
}

function OptionCard({
  name,
  multi,
  option,
  checked,
  onToggle,
}: {
  name: string;
  multi: boolean;
  option: QuestionOption;
  checked: boolean;
  onToggle: () => void;
}) {
  return (
    <label
      className={cx(
        "flex min-h-14 cursor-pointer gap-3 relative rounded-xl border p-3 transition-colors has-[input:focus-visible]:ring-2 has-[input:focus-visible]:ring-[var(--accent-ring)]",
        checked ? "border-accent bg-accent-soft/50" : "border-border hover:border-border-strong hover:bg-panel-2",
      )}
    >
      <input type={multi ? "checkbox" : "radio"} name={name} className="sr-only" checked={checked} onChange={onToggle} />
      <Indicator multi={multi} checked={checked} />
      <span className="min-w-0 flex-1">
        <span className="block font-medium leading-snug">{option.label}</span>
        {option.description && <span className="mt-0.5 block text-[13px] leading-snug text-muted">{option.description}</span>}
        {option.preview && <Preview content={option.preview} />}
      </span>
    </label>
  );
}

function Indicator({ multi, checked }: { multi: boolean; checked: boolean }) {
  return (
    <span
      aria-hidden="true"
      className={cx(
        "mt-0.5 grid size-[18px] shrink-0 place-items-center border-2 transition-colors",
        multi ? "rounded-[5px]" : "rounded-full",
        checked ? "border-accent bg-accent text-on-accent" : "border-border-strong bg-panel",
      )}
    >
      {checked && (multi ? <IconCheck size={12} strokeWidth={3} /> : <span className="size-1.5 rounded-full bg-on-accent" />)}
    </span>
  );
}

function Preview({ content }: { content: string }) {
  const isHtml = /^\s*</.test(content);
  const text = useMemo(() => content.replace(/^\s*```[\w-]*\n?/, "").replace(/\n?```\s*$/, ""), [content]);
  return (
    <span className="mt-2 block" onClick={(e) => e.preventDefault()}>
      {isHtml ? (
        <iframe
          sandbox=""
          srcDoc={content}
          title="Option preview"
          className="block h-40 w-full rounded-lg border border-border bg-white"
        />
      ) : (
        <pre className="max-h-48 cursor-text overflow-auto whitespace-pre rounded-lg border border-border bg-panel-2 px-3 py-2 font-mono text-[12px] leading-relaxed text-text">
          {text}
        </pre>
      )}
    </span>
  );
}

function AnsweredCard({ questions, result }: { questions: Question[]; result: AskUserResult }) {
  const [expanded, setExpanded] = useState(false);
  const answerFor = (q: Question): string => {
    const value = result.answers?.[q.question];
    if (Array.isArray(value)) return value.length ? value.join(", ") : "—";
    return value ?? "—";
  };
  return (
    <div className="my-1 rounded-xl border border-border bg-panel-2/60">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
        className="flex w-full cursor-pointer items-start gap-2.5 rounded-xl px-3 py-2.5 text-left hover:bg-panel-2"
      >
        <span className="mt-0.5 grid size-5 shrink-0 place-items-center rounded-full bg-ok-soft text-ok">
          <IconCheck size={12} strokeWidth={3} />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-[12px] font-semibold text-muted">You answered</span>
          {result.response ? (
            <span className="block text-[13.5px]">{result.response}</span>
          ) : (
            <span className="mt-0.5 flex flex-wrap gap-x-3 gap-y-1 text-[13.5px]">
              {questions.map((q) => (
                <span key={q.question} className="min-w-0">
                  <span className="text-faint">{q.header || "Answer"}:</span>{" "}
                  <span className="font-medium">{answerFor(q)}</span>
                </span>
              ))}
            </span>
          )}
        </span>
        <IconChevronDown size={15} className={cx("mt-0.5 shrink-0 text-faint transition-transform", expanded && "rotate-180")} />
      </button>
      {expanded && (
        <div className="space-y-2 border-t border-border px-3 py-2.5 text-[13px]">
          {questions.map((q) => (
            <div key={q.question}>
              <div className="text-muted">{q.question}</div>
              <div className="font-medium">{answerFor(q)}</div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
