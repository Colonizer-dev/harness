// The org settings form's exec policy editor (issue #924). Rendered through react-dom/server like
// the rest of the cockpit's tests (no jsdom): the text box, and its problem shown inline beneath it.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { execPolicyProblem } from "../execPolicy";
import { ExecPolicyEditor } from "./OrgSettingsDialog";

const render = (value: string, error: string | null = execPolicyProblem(value)) =>
  renderToStaticMarkup(<ExecPolicyEditor org="acme" value={value} error={error} onChange={() => {}} />);

describe("ExecPolicyEditor", () => {
  it("edits the org's policy as JSON text, with no problem shown while it is valid", () => {
    const out = render('{"rules": [{"id": "no-publish", "decision": "deny", "command": "npm publish"}]}');
    expect(out).toContain(">Exec policy</h3>");
    expect(out).toContain('aria-label="Exec policy for acme"');
    expect(out).toContain('aria-invalid="false"');
    expect(out).toContain("no-publish");
    expect(out).not.toContain('role="alert"');
  });

  it("shows a validation error inline under the text box", () => {
    const out = render('{"rules": [{"id": "x", "decision": "deny"}]}');
    expect(out).toContain('aria-invalid="true"');
    expect(out).toContain('aria-describedby="org-exec-policy-error"');
    expect(out).toContain('role="alert"');
    expect(out).toContain("exec policy rule &quot;x&quot;: a rule needs");
    expect(out).toContain("border-err");
  });

  it("shows a refusal the server gave", () => {
    expect(render("{}", "the exec policy is larger than 64 KiB")).toContain("the exec policy is larger than 64 KiB");
  });
});
