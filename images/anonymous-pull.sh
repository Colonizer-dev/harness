#!/bin/sh
# Can anyone pull this GHCR image? Usage: anonymous-pull.sh <owner/repo> <sha256:digest>
#
# Colonies pull their image anonymously, and a new GHCR package starts private, so a published
# digest is only safe to pin in crates/colonizer/images.lock once this says public=true. It asks
# GHCR for an anonymous pull token and then for the manifest by digest, exactly what a colony's
# first pull does, and prints `public=true` or `public=false` (a line for $GITHUB_OUTPUT). A private
# package is reported, not failed: the push worked, and only an org owner can change visibility.
# Exits non-zero only on a malformed argument.
set -eu

repo=${1:?usage: anonymous-pull.sh <owner/repo> <sha256:digest>}
digest=${2:?usage: anonymous-pull.sh <owner/repo> <sha256:digest>}
case "$digest" in
  sha256:*) ;;
  *) echo "not a digest: $digest" >&2; exit 2 ;;
esac

token=$(curl -fsS "https://ghcr.io/token?scope=repository:${repo}:pull&service=ghcr.io" 2>/dev/null \
  | sed -n 's/.*"token":"\([^"]*\)".*/\1/p') || token=
code=$(curl -s -o /dev/null -w '%{http_code}' -I \
  -H "Authorization: Bearer ${token}" \
  -H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json' \
  "https://ghcr.io/v2/${repo}/manifests/${digest}") || code=000

if [ "$code" = 200 ]; then
  echo "public=true"
else
  echo "::warning::ghcr.io/${repo}@${digest} is not pullable anonymously (HTTP ${code}); an org owner must make the package public before images.lock can pin it" >&2
  echo "public=false"
fi
