#!/bin/sh
# Downloads the pinned third-party binaries from vendor/vendor.lock, verifies their sha256 and
# installs them into dist/vendor. Downloads are cached in vendor/cache.
#
# VENDOR_KINDS="plugin" limits it to those kinds (space-separated), for checking plugin staging
# without the platform binaries.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
platform="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
cache="$root/vendor/cache"
out="$root/dist/vendor"
mkdir -p "$cache" "$out/tailscale"

# GNU calls it sha256sum, macOS ships shasum. Both print "<hash>  <file>".
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}
verify() { [ -f "$1" ] && [ "$(sha256_of "$1")" = "$2" ]; }

found=0
while read -r name version plat kind sha url; do
  case "$name" in ''|'#'*) continue ;; esac
  # `any` entries are platform-independent (source, not binaries).
  [ "$plat" = "$platform" ] || [ "$plat" = "any" ] || continue
  if [ -n "${VENDOR_KINDS:-}" ]; then
    case " $VENDOR_KINDS " in *" $kind "*) ;; *) continue ;; esac
  fi
  found=$((found + 1))
  # Prefixed with the name: two tag archives can share a basename (v2.2.1), and would evict each other.
  file="$cache/$name-$(basename "$url")"
  if ! verify "$file" "$sha"; then
    echo "fetching $name $version"
    curl -fsSL --retry 3 -o "$file.part" "$url"
    mv "$file.part" "$file"
  fi
  verify "$file" "$sha" || { echo "checksum mismatch for $name" >&2; rm -f "$file"; exit 1; }
  case "$name" in
    headscale) install -m 755 "$file" "$out/headscale" ;;
    tailscale)
      # The Linux rows are upstream's own tgz. The darwin row is source, verified above and left in
      # the cache like rtk's: macOS has no prebuilt tailscaled, so scripts/build-tailscaled.sh builds
      # tailscale and tailscaled from it.
      case "$kind" in
        source) ;;
        *)
          tmp=$(mktemp -d)
          tar -xzf "$file" -C "$tmp"
          install -m 755 "$tmp"/tailscale_*/tailscale "$out/tailscale/tailscale"
          install -m 755 "$tmp"/tailscale_*/tailscaled "$out/tailscale/tailscaled"
          rm -rf "$tmp"
          ;;
      esac
      ;;
    tailscale-guest)
      # Upstream's Linux build for the guest's architecture: what a colony mounts when the host's own
      # tailscaled is not a Linux one.
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      mkdir -p "$out/tailscale-guest"
      install -m 755 "$tmp"/tailscale_*/tailscale "$out/tailscale-guest/tailscale"
      install -m 755 "$tmp"/tailscale_*/tailscaled "$out/tailscale-guest/tailscaled"
      rm -rf "$tmp"
      ;;
    ecc)
      # Staged as a Claude Code plugin directory, not a binary, at dist/plugins/<name>:
      # the mothership resolves vendored plugins at <app>/plugins/<name>
      # (crates/colonizer/src/plugins.rs), not under vendor/. Staging it under
      # dist/vendor/ left every `plugins = ecc` colony failing to boot.
      #
      # The whole hooks/ directory is dropped. ECC's plugin manifest sets
      # userConfig.hooks_enabled default true and Claude Code discovers
      # hooks/hooks.json by convention, so "skills and agents only" cannot be
      # expressed as a flag: every ECC hook is a `node -e` bootstrap that spawns
      # first and checks ECC_HOOKS_ENABLED second. Removing the files is the
      # only version of this that is true by construction.
      #
      # Also dropped: docs/ and the per-harness copies under .kiro, .cursor,
      # .opencode and .agents, which duplicate the same skills for other tools.
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      src=$(echo "$tmp"/ECC-*)
      dest="$root/dist/plugins/ecc"
      rm -rf "$dest"
      mkdir -p "$dest"
      for keep in .claude-plugin skills agents commands scripts LICENSE; do
        [ -e "$src/$keep" ] || { echo "ecc $version has no $keep" >&2; exit 1; }
        cp -R "$src/$keep" "$dest/$keep"
      done
      # Fail loudly rather than shipping hooks by accident.
      if [ -e "$dest/hooks" ]; then echo "ecc staging leaked hooks/" >&2; exit 1; fi
      rm -rf "$tmp"
      ;;
    superpowers)
      # Staged as a Claude Code plugin directory at dist/plugins/superpowers.
      #
      # hooks/ is dropped, as for ECC. Its one hook is a SessionStart bootstrap
      # that injects skills/using-superpowers/SKILL.md; the claude-code runner puts
      # that text in the system prompt instead, so no hook process spawns.
      #
      # using-git-worktrees and finishing-a-development-branch are dropped too: a
      # colony already runs in its own worktree on the branch Colonizer publishes,
      # and the host commits, pushes and opens the pull request. Other skills name
      # them, so the runner tells the agent they are missing on purpose.
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      src=$(echo "$tmp"/superpowers-*)
      dest="$root/dist/plugins/superpowers"
      rm -rf "$dest"
      mkdir -p "$dest"
      for keep in .claude-plugin skills LICENSE; do
        [ -e "$src/$keep" ] || { echo "superpowers $version has no $keep" >&2; exit 1; }
        cp -R "$src/$keep" "$dest/$keep"
      done
      rm -rf "$dest/skills/using-git-worktrees" "$dest/skills/finishing-a-development-branch"
      [ -f "$dest/skills/using-superpowers/SKILL.md" ] || { echo "superpowers $version has no using-superpowers skill to bootstrap from" >&2; exit 1; }
      for leak in hooks skills/using-git-worktrees skills/finishing-a-development-branch; do
        if [ -e "$dest/$leak" ]; then echo "superpowers staging leaked $leak" >&2; exit 1; fi
      done
      # Canonical layout (docs/skill-packs.md): a root plugin.json beside the
      # .claude-plugin/ manifest the SDK reads. Upstream lists no skills, so the
      # list is generated from the staged skill names. No mcp.json: no servers.
      node -e 'const fs=require("fs"),d=process.argv[1],u=JSON.parse(fs.readFileSync(d+"/.claude-plugin/plugin.json","utf8")),s=fs.readdirSync(d+"/skills").filter((n)=>fs.existsSync(d+"/skills/"+n+"/SKILL.md")).sort();fs.writeFileSync(d+"/plugin.json",JSON.stringify({name:u.name,version:u.version,description:u.description,skills:s},null,2)+"\n");console.log("superpowers root plugin.json: "+s.length+" skills")' "$dest" || exit 1
      rm -rf "$tmp"
      ;;
    rtk)
      # Source, verified and left in the cache: scripts/build-rtk.sh builds the static binary from it.
      ;;
    caveman)
      # Staged at dist/vendor/caveman: only the MIT skill text and LICENSE. The claude-code runner puts
      # the ruleset in the system prompt of colonies that switch on terse replies, instead of the
      # plugin's SessionStart and UserPromptSubmit hooks. The rest of the repository — its compression
      # engine, proxy and MCP server — is BSL-1.1, and none of it is staged.
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      src=$(echo "$tmp"/caveman-*)
      dest="$out/caveman"
      rm -rf "$dest"
      mkdir -p "$dest"
      for keep in skills/caveman/SKILL.md LICENSE; do
        [ -f "$src/$keep" ] || { echo "caveman $version has no $keep" >&2; exit 1; }
      done
      cp "$src/skills/caveman/SKILL.md" "$dest/SKILL.md"
      cp "$src/LICENSE" "$dest/LICENSE"
      grep -q '^name: caveman$' "$dest/SKILL.md" || { echo "caveman $version: SKILL.md is not the caveman skill" >&2; exit 1; }
      rm -rf "$tmp"
      ;;
    fast-jev-compaction)
      # Staged at dist/vendor/fast-jev-compaction: only what the function hook runs — the plugin
      # manifest, hooks/ with its src/ imports, plus LICENSE and package.json. Left out: tests,
      # examples, types (compile-time only), docs and the demo. Kind `hook`, never `plugin`: a
      # dist/plugins/ copy would list it as a pickable skillset and bypass the compaction switch.
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      src=$(echo "$tmp"/fast-jev-compaction-*)
      dest="$out/fast-jev-compaction"
      rm -rf "$dest"
      mkdir -p "$dest"
      for keep in .claude-plugin hooks src LICENSE package.json; do
        [ -e "$src/$keep" ] || { echo "fast-jev-compaction $version has no $keep" >&2; exit 1; }
        cp -R "$src/$keep" "$dest/$keep"
      done
      grep -q '"name": "fast-jev-compaction"' "$dest/.claude-plugin/plugin.json" || { echo "fast-jev-compaction $version: plugin.json is not the fast-jev-compaction plugin" >&2; exit 1; }
      rm -rf "$tmp"
      ;;
    google-skills)
      # Staged for on-demand loading at dist/plugins/google-skills. Preloading all
      # of google/skills would put ~17k tokens of skill descriptions into every
      # colony; instead Claude Code discovers one skill, the finder, which reads
      # a catalog of the rest from the same read-only mount:
      #
      #   skills/finding-google-skills/  Colonizer's copy (vendor/google-skills/):
      #                                  reads the local catalog, never the network
      #   catalog/<category>/<name>/     upstream skills/, minus the upstream finder;
      #                                  same shape, so relative links keep working
      #   index.json                     upstream catalog, entrypoints rewritten from
      #                                  raw.githubusercontent.com URLs to catalog/ paths
      #
      # Not staged: plugins/ (MCP servers, and git submodules a codeload archive
      # doesn't contain) and the marketplace manifest.
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      src=$(echo "$tmp"/skills-*)
      dest="$root/dist/plugins/google-skills"
      rm -rf "$dest"
      mkdir -p "$dest/.claude-plugin" "$dest/skills"
      for need in skills index.json LICENSE; do
        [ -e "$src/$need" ] || { echo "google-skills $version has no $need" >&2; exit 1; }
      done
      cp -R "$src/skills" "$dest/catalog"
      rm -rf "$dest/catalog/developers/finding-google-skills"
      cp "$src/LICENSE" "$dest/LICENSE"
      cp -R "$root/vendor/google-skills/finding-google-skills" "$dest/skills/finding-google-skills"
      printf '{\n  "name": "google-skills",\n  "version": "%s",\n  "description": "Agent Skills for Google products and technologies, from github.com/google/skills, loaded on demand",\n  "license": "Apache-2.0"\n}\n' "$version" > "$dest/.claude-plugin/plugin.json"
      # Rewrites the catalog and fails on anything it can't map to a staged file.
      node - "$src/index.json" "$dest" <<'NODE' || exit 1
const fs = require('fs'), path = require('path');
const [indexPath, dest] = process.argv.slice(2);
const prefix = 'https://raw.githubusercontent.com/google/skills/main/skills/';
const index = JSON.parse(fs.readFileSync(indexPath, 'utf8'));
if (!Array.isArray(index.skills)) throw new Error('google-skills index.json has no skills array');
const skills = [];
for (const skill of index.skills) {
  if (skill.name === 'finding-google-skills') continue;
  if (typeof skill.entrypoint !== 'string' || !skill.entrypoint.startsWith(prefix)) {
    throw new Error(`google-skills: ${skill.name} has an entrypoint outside skills/: ${skill.entrypoint}`);
  }
  const entrypoint = 'catalog/' + skill.entrypoint.slice(prefix.length);
  if (!fs.existsSync(path.join(dest, entrypoint))) throw new Error(`google-skills: ${skill.name} points at a missing ${entrypoint}`);
  skills.push({ name: skill.name, description: skill.description, entrypoint });
}
fs.writeFileSync(path.join(dest, 'index.json'), JSON.stringify({ skills }, null, 2) + '\n');
console.log(`google-skills catalog: ${skills.length} skills`);
NODE
      # Fail loudly rather than ship what this staging exists to keep out.
      if [ -n "$(find "$dest" \( -name hooks -o -name hooks.json -o -name .mcp.json -o -name mcp.json -o -name mcp_config.json \) -print -quit)" ]; then
        echo "google-skills staging leaked a hook or an MCP server configuration" >&2; exit 1
      fi
      if grep -q 'https://raw.githubusercontent.com' "$dest/index.json" "$dest/skills/finding-google-skills/SKILL.md"; then
        echo "google-skills: the catalog or the finder still points at GitHub" >&2; exit 1
      fi
      if [ "$(find "$dest/skills" -name SKILL.md | wc -l)" -ne 1 ]; then
        echo "google-skills: only the finder may be discoverable under skills/" >&2; exit 1
      fi
      rm -rf "$tmp"
      ;;
    microsandbox)
      tmp=$(mktemp -d)
      tar -xzf "$file" -C "$tmp"
      mkdir -p "$out/microsandbox/bin" "$out/microsandbox/lib"
      install -m 755 "$tmp/msb" "$out/microsandbox/bin/msb"
      # msb loads libkrunfw by soname from the lib directory beside it, so keep upstream's links.
      lib=$(cd "$tmp" && ls libkrunfw.so.*.*.* libkrunfw.*.dylib 2>/dev/null | head -1)
      [ -n "$lib" ] || { echo "the microsandbox bundle has no libkrunfw" >&2; exit 1; }
      install -m 755 "$tmp/$lib" "$out/microsandbox/lib/$lib"
      case "$lib" in
        *.dylib)
          ln -sf "$lib" "$out/microsandbox/lib/libkrunfw.dylib"
          ;;
        *)
          abi=${lib#libkrunfw.so.}
          abi=${abi%%.*}
          ln -sf "$lib" "$out/microsandbox/lib/libkrunfw.so.$abi"
          ln -sf "libkrunfw.so.$abi" "$out/microsandbox/lib/libkrunfw.so"
          ;;
      esac
      rm -rf "$tmp"
      ;;
  esac
  echo "installed $name $version"
done < "$root/vendor/vendor.lock"

[ "$found" -gt 0 ] || { echo "no vendored binaries pinned for $platform" >&2; exit 1; }

# The DERP relay map is committed (scripts/update-derpmap.sh), so the mesh never fetches it at runtime.
install -m 644 "$root/vendor/derpmap.yaml" "$out/derpmap.yaml"
echo "installed DERP map"
