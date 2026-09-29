# Media

- `demo.gif` — a ~30 second recording of the cockpit running on its in-browser mock
  (`?mock=1`): the nest of colonies, one colony's chat while its settlers work, the choice
  card the agent asks with, and the pull request it opens. The README embeds it.
- `social-preview.png` — the 1280×640 card GitHub shows when the repository is linked,
  rendered from `social-preview.html` next to it. A maintainer uploads it once, by hand, under
  the repository's Settings → Social preview; GitHub stores it, so it is never rebuilt in CI.

## Regenerating

```sh
node docs/media/record-demo.mjs                # both files
node docs/media/record-demo.mjs --only gif     # demo.gif only
node docs/media/record-demo.mjs --only social  # social-preview.png only
```

Needs Node, ffmpeg and gifsicle on PATH; the script fetches everything else itself on first run — a
pinned Playwright and its Chromium, into `<tmpdir>/colonizer-demo-recorder` (override with
`COLONIZER_DEMO_CACHE`), never into the repository — and serves the cockpit with `vite dev`,
stopping it when it exits. Raw video and palettes stay in that cache directory.

`demo.gif` has a hard budget of 5 MB, and the script exits non-zero rather than write anything
larger; if a regeneration drifts over it, lower `GIF.fps` or `GIF.width` at the top of the
script. ([#683](https://github.com/Colonizer-dev/harness/issues/683))
