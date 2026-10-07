#!/bin/sh
# Refreshes vendor/derpmap.yaml, the DERP relay map bundled with Colonizer's private mesh so
# headscale never has to fetch it at runtime. Maintainer tool; the result is committed.
#
#   scripts/update-derpmap.sh             read the map from the local tailscale client
#   scripts/update-derpmap.sh <file.json> convert a saved https://controlplane.tailscale.com/derpmap/default
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
src=$(mktemp)
trap 'rm -f "$src"' EXIT
if [ $# -gt 0 ]; then cp "$1" "$src"; else tailscale debug derp-map > "$src"; fi

python3 - "$src" "$root/vendor/derpmap.yaml" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
regions = data.get("Regions") or data.get("regions")
if not regions:
    sys.exit("no Regions in the DERP map")

def scalar(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if v is None:
        return "null"
    if isinstance(v, (int, float)):
        return str(v)
    if isinstance(v, dict):
        return "{}"
    if isinstance(v, list):
        return "[]"
    return json.dumps(v)

def emit(value, depth):
    pad = "  " * depth
    lines = []
    if isinstance(value, dict):
        for key, item in value.items():
            name = key if key.isdigit() else key.lower()
            if isinstance(item, (dict, list)) and item:
                lines.append(f"{pad}{name}:")
                lines.extend(emit(item, depth + 1))
            else:
                lines.append(f"{pad}{name}: {scalar(item)}")
    else:
        for item in value:
            if isinstance(item, dict) and item:
                sub = emit(item, depth + 1)
                lines.append(f"{pad}- {sub[0].lstrip()}")
                lines.extend(sub[1:])
            else:
                lines.append(f"{pad}- {scalar(item)}")
    return lines

out = ["# Tailscale's public DERP relay map, bundled so the mesh control server works offline.",
       "# Regenerate with scripts/update-derpmap.sh.", "regions:"]
out.extend(emit(regions, 1))
open(sys.argv[2], "w").write("\n".join(out) + "\n")
nodes = sum(len(r.get("Nodes", [])) for r in regions.values())
print(f"wrote {sys.argv[2]}: {len(regions)} regions, {nodes} relay nodes")
PY
