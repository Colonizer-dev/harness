#!/bin/sh
# The executable inside Colonizer.app (Contents/MacOS/Colonizer), built into the app by
# scripts/build-dmg.sh. Opening the app installs or updates Colonizer from the archive the app
# carries, makes sure the LaunchAgent dev.colonizer.mothership is set up, and opens the cockpit.
#
# It reuses the installer rather than copying it: Contents/Resources/install-release.sh is
# scripts/install-release.sh, run with COLONIZER_LOCAL_ARCHIVE pointing at the bundled
# colonizer-darwin-arm64.tar.gz, so slots, the ~/.local/bin link, Keychain re-signing and
# COLONIZER_KEEP_PREVIOUS behave exactly as they do for `curl ... | sh`. The LaunchAgent is the one
# `colonizer login-item enable` writes. Everything is logged to ~/Library/Logs/Colonizer-app.log.
#
#   COLONIZER_APP_DRY_RUN=1   print the steps this would take and do none of them (tests)
#   COLONIZER_APP_NO_OPEN=1   do not open the cockpit at the end (tests)
#   COLONIZER_APP_NO_LOGIN_ITEM=1
#                             do not touch launchd (tests)
#
# HOME, COLONIZER_APP, COLONIZER_BIND and the rest of the installer's environment are honoured, so a
# test can run all of this in a temp HOME.
set -eu

res=$(cd "$(dirname "$0")/../Resources" && pwd)
archive=$res/colonizer-darwin-arm64.tar.gz
installer=$res/install-release.sh
app=${COLONIZER_APP:-$HOME/.local/share/colonizer/app}
bind=${COLONIZER_BIND:-127.0.0.1:7878}
dry=${COLONIZER_APP_DRY_RUN:-0}

if [ "$dry" != 1 ]; then
  mkdir -p "$HOME/Library/Logs"
  exec >> "$HOME/Library/Logs/Colonizer-app.log" 2>&1
  printf '\n==== %s: Colonizer.app opened\n' "$(date '+%Y-%m-%d %H:%M:%S')"
fi

say() { printf '==> %s\n' "$1"; }
# A Finder-launched app has no terminal, so a failure has to be shown some other way.
die() {
  printf 'Colonizer: %s\n' "$1" >&2
  if [ "$dry" != 1 ] && command -v osascript >/dev/null 2>&1; then
    osascript -e 'on run argv' -e 'display dialog (item 1 of argv) with title "Colonizer" buttons {"OK"} default button "OK" with icon caution' -e 'end run' \
      "$1 The log is ~/Library/Logs/Colonizer-app.log." >/dev/null 2>&1 || true
  fi
  exit 1
}
run() { if [ "$dry" = 1 ]; then printf 'DRY RUN: %s\n' "$*"; else "$@"; fi; }

[ -f "$archive" ] && [ -f "$installer" ] || die "this copy of the app is missing its installer payload; download Colonizer-arm64.dmg again."

bundled=$(cat "$res/VERSION" 2>/dev/null || echo unknown)
installed=$(cat "$app/VERSION" 2>/dev/null || true)

# Install when there is nothing installed, or when the app carries a different version. The same
# version again is left alone, so reopening the app does not churn the slots.
if [ -x "$app/bin/colonizer" ] && [ "$installed" = "$bundled" ]; then
  say "Colonizer $installed is already installed"
else
  [ -z "$installed" ] || say "updating Colonizer $installed to $bundled"
  # Updating beside a mothership that may be running: keep the old slot for it to sweep up itself,
  # as an in-place update does.
  keep=0
  [ ! -e "$app" ] || keep=1
  if [ "$dry" = 1 ]; then
    printf 'DRY RUN: COLONIZER_LOCAL_ARCHIVE=%s COLONIZER_KEEP_PREVIOUS=%s sh %s\n' "$archive" "$keep" "$installer"
  else
    COLONIZER_LOCAL_ARCHIVE=$archive COLONIZER_KEEP_PREVIOUS=$keep sh "$installer" ||
      die "the install failed (offline? Colonizer fetches Claude Code and Node.js for colonies while installing)."
  fi
fi

colonizer=$HOME/.local/bin/colonizer
if [ "${COLONIZER_APP_NO_LOGIN_ITEM:-0}" != 1 ]; then
  # Writes the dev.colonizer.mothership LaunchAgent (or refreshes it) and loads it, which starts the mothership.
  run "$colonizer" login-item enable || die "could not set up the login item (LaunchAgent dev.colonizer.mothership)."
fi

if [ "${COLONIZER_APP_NO_OPEN:-0}" != 1 ]; then
  if [ "$dry" != 1 ]; then
    # Give a mothership that was just started a moment to listen; `colonizer open` works without
    # one, but the sign-in link is only useful once it does.
    hostport=${bind#http://}
    i=0
    while [ "$i" -lt 30 ]; do
      curl -fsS -o /dev/null "http://$hostport/api/status" 2>/dev/null && break
      i=$((i + 1))
      sleep 1
    done
  fi
  run "$colonizer" open
fi
