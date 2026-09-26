- **graft as a downloadable skillset.** Settings → Skillsets offers graft (a code
  map of the repository: `graft ask`, `callers`, `skeleton`, `grep`) with a Download
  button. The bundle is not shipped with the app: the mothership downloads the
  per-architecture bundle pinned by sha256 in `crates/colonizer/graft.lock` into
  `<data>/plugins/graft`, where it is an ordinary skillset to switch on. Each colony
  builds its own map of its own worktree on first use, outside the worktree and
  offline — no model key reaches graft and its telemetry stays closed. The bundle
  carries its own Node 22, because graft's native tree-sitter core does not build
  on the Node 24 colonies run. Bundles are built by `.github/workflows/graft-bundle.yml`
  on `graft-*` tags; until one is published and pinned, the row says it is not
  available yet.
