// Boundary events (issue #609): the runner's report that a control refused something — an exec
// policy deny, a refused exec-policy ask tried again, an egress refusal, a read-only mount. The
// mothership's watchdog reads them for its control-defeat signature (docs/boundaries.md,
// "Watchdog signatures"). Reporting only: the control already decided before the event exists,
// so the event carries no permission and grants nothing, and removing it changes no return code.
//
// This module is pure, so tests drive it without an SDK, and it is one file in two places — the
// ACP module keeps a byte-identical copy, and a test there fails when the two drift.

/** The closed vocabulary of boundary kinds (docs/agent-events.schema.json `boundary.kind`). */
export const BOUNDARY_KINDS = [
  'exec_policy_deny',
  'exec_policy_ask_bypass_attempt',
  'path_policy_denied',
  'path_policy_unbound',
  'egress_denied',
  'publish_rewrite_refused',
  'sandbox_denied',
];

const CONTROL_CHARS = 80;
const DETAIL_CHARS = 300;
const TARGET_CHARS = 200;

// Credential shapes masked in a detail before it leaves the runner. The mothership redacts every
// line again before it is persisted; this keeps the obvious ones off agentd's own log too.
const SECRET_SHAPES = [
  [/\b(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b/g, '[redacted]'],
  [/\b(sk-[A-Za-z0-9_-]{16,})\b/g, '[redacted]'],
  [/\b(xox[abpr]-[A-Za-z0-9-]{10,})\b/g, '[redacted]'],
  [/\b(AKIA[0-9A-Z]{16})\b/g, '[redacted]'],
  [/(\bbearer\s+)[A-Za-z0-9._~+/=-]{8,}/gi, '$1[redacted]'],
  [/(\b[A-Za-z_]*(?:TOKEN|SECRET|PASSWORD|PASSWD|API_?KEY|PRIVATE_KEY)[A-Za-z_]*=)\S+/gi, '$1[redacted]'],
  [/(\b[a-z][a-z0-9+.-]*:\/\/)[^/\s:@'"]+:[^@\s'"]+@/gi, '$1[redacted]@'],
];

/** One line, credential shapes masked, capped. */
export function redactDetail(text, max = DETAIL_CHARS) {
  let flat = String(text ?? '').replace(/[\u0000-\u001f\u007f]+/g, ' ').replace(/\s+/g, ' ').trim();
  for (const [re, mask] of SECRET_SHAPES) flat = flat.replace(re, mask);
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}

/**
 * One `boundary` event. `target` — a host or a path the control refused — is what the watchdog
 * matches a later successful call against; it is left out when the runner cannot name one.
 * @param {string} kind     one of BOUNDARY_KINDS
 * @param {string} control  the control or rule that decided (`exec_policy:secret-paths`, `egress`)
 * @param {string} detail   what was refused, redacted and capped here
 * @param {{ target?: string|null, now?: () => Date }} [opts]
 */
export function boundaryEvent(kind, control, detail, { target = null, now = () => new Date() } = {}) {
  const event = {
    type: 'boundary',
    kind,
    control: redactDetail(control, CONTROL_CHARS) || 'unknown',
    detail: redactDetail(detail),
    at: now().toISOString(),
  };
  const named = target ? redactDetail(target, TARGET_CHARS) : '';
  if (named) event.target = named;
  return event;
}

const WRITE_WORDS = new Set(['cp', 'mv', 'tee', 'install', 'touch', 'ln', 'rsync', 'dd']);
const NAVIGATION = new Set(['cd', 'pushd', 'popd']);

/** Strips one layer of quotes off a shell word. */
const unquote = (word) => word.replace(/^(['"])(.*)\1$/, '$2');

/**
 * The path a refused command was after, when the command names one plainly: a redirect's target,
 * the destination of a write command (`cp a b` → `b`, `dd of=b` → `b`), else the first argument
 * that reads as a path (absolute, `~`, `./` or a dotfile), in any segment of a pipeline or list —
 * a write anywhere wins over a plain path argument. Undefined when nothing does — the
 * event then carries no target, and the watchdog matches only on repetition.
 * @param {string} command
 */
export function commandTarget(command) {
  const text = String(command ?? '');
  const redirect = text.match(/(?:^|[^<>&0-9])>{1,2}\s*([^\s;&|<>]+)/);
  if (redirect && !/^&\d$/.test(redirect[1])) return unquote(redirect[1]);
  let fallback;
  for (const segment of text.split(/&&|\|\||[;|\n]/)) {
    const words = segment.trim().split(/\s+/).filter(Boolean).map(unquote);
    // `cd /workspace && cat .env` was after `.env`: where a command moves to is not its target.
    if (!words.length || NAVIGATION.has(words[0])) continue;
    const args = words.slice(1).filter((w) => !w.startsWith('-'));
    if (WRITE_WORDS.has(words[0])) {
      const of = args.find((w) => w.startsWith('of='));
      if (of) return of.slice(3);
      if (args.length) return args[args.length - 1];
    }
    fallback ??= args.find((w) => /^(\/|~|\.\/|\.[A-Za-z0-9_])/.test(w));
  }
  return fallback;
}

/** The `boundary` event an exec-policy deny becomes. Its target is the path the rule matched when
 * the rule names one (a `touches` rule), else the path the command plainly names. */
export function execPolicyBoundary(hit, command, opts = {}) {
  return boundaryEvent('exec_policy_deny', `exec_policy:${hit.rule}`, `${hit.decision} (${hit.layer}): ${command}`, {
    target: hit.target ?? commandTarget(command),
    ...opts,
  });
}

/**
 * The colony's refused exec-policy asks (issue #609): an ask a person (or the judge) refused, kept
 * by rule, so the same rule asking again in this run — the agent retrying what it was told no to,
 * however it rephrased the command — is reported as an `exec_policy_ask_bypass_attempt`.
 */
export function createAskRefusals() {
  const refused = new Map(); // `layer\0rule` -> the refused command
  const key = (hit) => `${hit.layer}\u0000${hit.rule}`;
  return {
    refuse(hit, command) {
      refused.set(key(hit), command);
    },
    /** The bypass-attempt event when this rule was refused earlier, else null. */
    attempt(hit, command, opts = {}) {
      const earlier = refused.get(key(hit));
      if (earlier === undefined) return null;
      return boundaryEvent(
        'exec_policy_ask_bypass_attempt',
        `exec_policy:${hit.rule}`,
        `asked again after a refusal (${hit.layer}): ${command} — refused earlier: ${earlier}`,
        { target: commandTarget(command), ...opts },
      );
    },
  };
}

const HOST = '([A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:\\.[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)+)';
const HOST_IN_OUTPUT = [
  new RegExp(`unable to access '[a-z][a-z0-9+.-]*://(?:[^/'@]*@)?${HOST}`, 'i'),
  new RegExp(`could not resolve host:?\\s*${HOST}`, 'i'),
  new RegExp(`(?:enotfound|eai_again|getaddrinfo \\w+)\\s+${HOST}`, 'i'),
  new RegExp(`failed to connect to ${HOST}`, 'i'),
  new RegExp(`[a-z][a-z0-9+.-]*://(?:[^/\\s'"@]*@)?${HOST}`, 'i'),
];
const PATH_IN_OUTPUT = /(?:cannot (?:touch|create|open|remove|write)[^'"`]*|open)\s*['"`]([^'"`]+)['"`]|((?:\/|~)[^\s:'"`]+):?\s*read-only file system/i;

/** The host an egress refusal names, from the tool's output first, then the call's input. */
export function egressTarget(output, input) {
  for (const text of [output, input ? JSON.stringify(input) : '']) {
    for (const re of HOST_IN_OUTPUT) {
      const m = String(text ?? '').match(re);
      if (m) return m[1].toLowerCase();
    }
  }
  return undefined;
}

/**
 * The `boundary` event a classified refusal on a tool result becomes (denials.mjs), or null:
 * `egress` → `egress_denied`, `read_only` → `sandbox_denied` on the read-only mount. An exec-policy
 * refusal (`policy`) is reported where the policy decided, and a disabled tool (`tool_disabled`) is
 * the delegation gate's guidance-shaped refusal, so neither becomes a second event here.
 */
export function denialBoundary(denial, output, input, opts = {}) {
  if (!denial) return null;
  if (denial.class === 'egress') {
    return boundaryEvent('egress_denied', 'egress', output, { target: egressTarget(output, input), ...opts });
  }
  if (denial.class === 'read_only') {
    const m = String(output ?? '').match(PATH_IN_OUTPUT);
    return boundaryEvent('sandbox_denied', 'read_only_mount', output, { target: m ? (m[1] ?? m[2]) : undefined, ...opts });
  }
  return null;
}
