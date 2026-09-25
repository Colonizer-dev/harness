import assert from 'node:assert/strict';
import { test } from 'node:test';

import { annotateDenial, classifyDenial, DENIAL_CLASSES, DENIAL_HINTS } from '../denials.mjs';
import { buildOptions, toolResultText } from '../runner.mjs';

// [class, refusal signature]: one row per family the colony actually produces.
const REFUSALS = [
  ['egress', 'curl: (7) Failed to connect to 127.0.0.1 port 8123: Connection refused'],
  ['egress', 'Error: connect ECONNREFUSED 10.0.0.5:443'],
  ['egress', "fatal: unable to access 'https://example.com/repo.git/': Could not resolve host: example.com"],
  ['egress', 'Error: getaddrinfo ENOTFOUND example.com'],
  ['egress', 'getaddrinfo EAI_AGAIN registry.npmjs.org'],
  ['egress', 'connect ENETUNREACH 10.0.0.5:443'],
  ['egress', 'Error: Network is unreachable'],
  ['egress', 'CONNECT tunnel failed with response code 403'],
  ['egress', 'curl: (56) Received HTTP code 403 from proxy after CONNECT'],
  ['read_only', "EROFS: Read-only file system, open '/harness/repo/pkg.lock'"],
  ['read_only', "touch: cannot touch '/x': Read-only file system"],
  ['read_only', "fatal: Unable to create '/harness/repo/.git/index.lock': File exists."],
  ['read_only', 'error: insufficient permission for adding an object to repository database .git/objects'],
  ['tool_disabled', 'Permission to use Bash with command git push origin main has been denied.'],
  ['tool_disabled', 'Permission for this action has been denied. Reason: Write belongs to your subagents in this colony. Start one with the Task tool and have it do this; you plan, decide and review. /harness/out/pr.md is yours to write.'],
  ['tool_disabled', 'Only the orchestrator paces or stops the loop. Say in your report what should happen next.'],
  ['tool_disabled', 'Only the orchestrator files findings. Put this finding in your report, with how you confirmed it.'],
  ['tool_disabled', 'memory_read_only: only the orchestrator proposes shared memory. Put this learning in your report.'],
  ['tool_disabled', 'tool NotebookEdit is not allowed for this session'],
];

const NOT_REFUSALS = [
  'npm ERR! Test failed.  See above for more details.',
  'bash: ./run.sh: Permission denied', // an OS fact, not a colony refusal
  'fatal: could not create work tree dir ./x: Permission denied',
  'fatal: not a git repository (or any of the parent directories): .git',
];

test('refusal signatures classify to their fixed hint; ordinary failures do not', () => {
  for (const [cls, text] of REFUSALS) assert.deepEqual(classifyDenial(text), { class: cls, hint: DENIAL_HINTS[cls] }, text);
  assert.deepEqual(classifyDenial('FATAL: CONNECTION REFUSED'), { class: 'egress', hint: DENIAL_HINTS.egress }, 'case-blind');
  for (const text of NOT_REFUSALS) assert.equal(classifyDenial(text), null, text);
  assert.deepEqual(DENIAL_CLASSES, ['egress', 'read_only', 'tool_disabled']);
  for (const empty of ['', undefined, 42]) assert.equal(classifyDenial(empty), null);
});

test('stripping the layer leaves a built tool_result event exactly as it was; hints never touch is_error', () => {
  const content = [{ type: 'text', text: "fatal: Unable to create '/harness/repo/.git/index.lock': File exists." }];
  const built = { type: 'tool_result', tool_call_id: 'toolu_1', output: toolResultText(content), is_error: true };
  const annotated = annotateDenial(built, built.output);
  assert.deepEqual(annotateDenial(built, built.output, () => null), built, 'stripped, the event is untouched');

  // The only difference is the denial field itself; is_error and the output are the SDK's own.
  const { denial, ...rest } = annotated;
  assert.deepEqual(rest, built);
  assert.deepEqual(denial, { class: 'read_only', hint: DENIAL_HINTS.read_only });
  assert.equal(annotated.is_error, true);
  assert.equal(annotated.output, built.output);

  // Non-error results never classify, even when their text matches a signature.
  const ok = { type: 'tool_result', tool_call_id: 'toolu_2', output: 'Connection refused', is_error: false };
  assert.equal(annotateDenial(ok, ok.output), ok);
});

/** The runner-registered PostToolUseFailure hook (buildOptions is one call per session). */
const failureHook = (env = {}) => {
  const { options } = buildOptions({ COLONIZER_DELEGATE: 'off', ...env });
  return options.hooks.PostToolUseFailure[0].hooks[0];
};
const failure = (error) => ({ hook_event_name: 'PostToolUseFailure', tool_name: 'Bash', tool_input: {}, tool_use_id: 't1', error });

test('a failed tool call carries its hint mid-turn, once per class per session', async () => {
  const hook = failureHook();
  const egress = await hook(failure(REFUSALS[0][1]));
  assert.equal(egress.continue, true, 'never a decision, never a permission field');
  assert.equal(egress.permissionDecision, undefined);
  assert.equal(egress.hookSpecificOutput.hookEventName, 'PostToolUseFailure');
  assert.ok(egress.hookSpecificOutput.additionalContext.includes(DENIAL_HINTS.egress));

  // The same class again gets no second hint; a different class still gets its first.
  assert.equal((await hook(failure(REFUSALS[1][1]))).hookSpecificOutput, undefined);
  const readOnly = await hook(failure(REFUSALS[9][1]));
  assert.ok(readOnly.hookSpecificOutput.additionalContext.includes(DENIAL_HINTS.read_only));

  // Unrelated errors carry no context at all.
  for (const text of NOT_REFUSALS) assert.equal((await hook(failure(text))).hookSpecificOutput, undefined, text);
});

test('the hook is registered alongside the delegation gate, whatever the settings', () => {
  for (const env of [{}, { COLONIZER_DELEGATE: 'enforce' }, { COLONIZER_DELEGATE: 'off' }]) {
    const { options } = buildOptions({ COLONIZER_DELEGATE: 'off', ...env });
    assert.ok(options.hooks.PostToolUseFailure.length >= 1, JSON.stringify(env));
  }
  assert.ok(buildOptions().options.hooks.PreToolUse.length >= 1, 'delegation enforcement keeps its own hook');
});
