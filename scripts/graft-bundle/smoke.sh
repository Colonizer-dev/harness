#!/bin/sh
# Smoke-tests an unpacked graft bundle the way a colony runs it: bin/graft on a small repository, offline
# defaults on. Proves every native grammar loads (graft imports all of them at startup), the graph is built
# outside the worktree, and ask / callers / skeleton / grep answer.
#
#   scripts/graft-bundle/smoke.sh <unpacked bundle directory, the one holding bin/graft>
set -eu
bundle=$1
repo=$(mktemp -d "${TMPDIR:-/var/tmp}/graft-smoke.XXXXXX")
trap 'rm -rf "$repo"' EXIT
export COLONIZER_GRAFT_STATE="$repo.state"
mkdir -p "$repo/src"
cat > "$repo/src/billing.ts" <<'TS'
export function invoiceTotal(lines: { qty: number; price: number }[]): number {
  return lines.reduce((sum, line) => sum + lineTotal(line), 0);
}
export function lineTotal(line: { qty: number; price: number }): number {
  return line.qty * line.price;
}
TS
cat > "$repo/src/report.py" <<'PY'
def summarize(invoices):
    return sum(total(i) for i in invoices)

def total(invoice):
    return invoice["amount"]
PY
(cd "$repo" && git init -q && git add -A && git -c user.email=s@x -c user.name=s commit -qm init)
cd "$repo"
g="$bundle/bin/graft"
out=$("$g" ask "how is an invoice total computed" --source)
echo "$out" | grep -q "invoiceTotal" || { echo "ask did not find invoiceTotal:"; echo "$out"; exit 1; }
"$g" callers lineTotal | grep -q "invoiceTotal" || { echo "callers missed invoiceTotal" >&2; exit 1; }
"$g" skeleton src/billing.ts | grep -q "lineTotal" || { echo "skeleton missed lineTotal" >&2; exit 1; }
"$g" grep "amount" | grep -q "report.py" || { echo "grep missed report.py" >&2; exit 1; }
[ -f "$COLONIZER_GRAFT_STATE/graph/INDEX.md" ] || { echo "no graph outside the worktree" >&2; exit 1; }
[ ! -e "$repo/graft" ] || { echo "graft wrote into the worktree" >&2; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "the worktree is dirty after graft ran:"; git status --porcelain; exit 1; }
rm -rf "$COLONIZER_GRAFT_STATE"
echo "graft smoke test passed"
