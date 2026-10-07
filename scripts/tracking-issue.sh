#!/bin/sh
# Keeps the issue the pin-update workflows open in place of a pull request, while the repository
# doesn't let GitHub Actions open one, and closes it once a run finds nothing left to propose.
#
#   tracking-issue.sh update TITLE BODY_FILE   edits the open issue titled TITLE, or opens one
#   tracking-issue.sh close TITLE MESSAGE      closes every open issue titled TITLE, commenting MESSAGE
#
# Needs gh with a token (GH_TOKEN) that can write issues in the current repository.
set -eu

usage() {
  echo "usage: $0 update TITLE BODY_FILE | close TITLE MESSAGE" >&2
  exit 2
}
[ $# -eq 3 ] || usage
command=$1
title=$2

# The title search is fuzzy, so the exact match happens here. It is done in shell rather than in the
# --jq filter, which takes no arguments and would need the title quoted into the expression. The
# list is captured on its own line so a failing gh stops the script instead of reading as "none".
list=$(gh issue list --state open --search "\"$title\" in:title" --json number,title --jq '.[] | "\(.number)\t\(.title)"')
tab=$(printf '\t')
issues=$(printf '%s\n' "$list" | while IFS=$tab read -r number name; do
  if [ "$name" = "$title" ]; then echo "$number"; fi
done)

case $command in
  update)
    issue=$(printf '%s\n' "$issues" | head -1)
    if [ -n "$issue" ]; then
      gh issue edit "$issue" --body-file "$3"
      echo "Updated issue #$issue"
    else
      gh issue create --title "$title" --body-file "$3"
    fi
    ;;
  close)
    if [ -z "$issues" ]; then
      echo "No open issue titled \"$title\"."
      exit 0
    fi
    for issue in $issues; do
      gh issue close "$issue" --comment "$3"
      echo "Closed issue #$issue"
    done
    ;;
  *) usage ;;
esac
