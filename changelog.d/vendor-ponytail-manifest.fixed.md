**The daily vendored-plugin update proposes ponytail again.** Since ponytail v5.1.0 its `skills/`
folder carries a `plugin.json` manifest beside the six skills, and staging read it as an unknown skill
and failed, so no plugin update (ECC, superpowers, Google's skills, archify, caveman or ponytail) had
been proposed since. The manifest is now recognised and left out, like ponytail's other upstream
manifests; a genuinely new skill still fails staging until it is reviewed.
