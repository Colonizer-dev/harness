#!/bin/sh
# The toolbox every colony image preset promises: a small set of languages-agnostic tools a
# colony's own build or verification step may reach for. Run inside a built image (colony-image.yml
# runs it via `docker run ... sh /toolbox-test.sh`), and locally in a bookworm sandbox, so the
# package list and the tools themselves are checked on every pull request rather than discovered
# missing by a colony.
#
# Each tool is exercised, not just installed: `--version` (or `import`) is what a colony actually
# calls, and a package that installs a broken binary should fail here. Every missing tool is named
# on the way out, so one red run reports the whole gap rather than only the first one.
set -u

missing=""
check() {
    name=$1
    shift
    if "$@" >/dev/null 2>&1; then
        printf 'ok   %s\n' "$name"
    else
        printf 'MISS %s\n' "$name" >&2
        missing="$missing $name"
    fi
}

check 'python3 import yaml' python3 -c 'import yaml'
check 'pip3 --version' pip3 --version
check 'python3 -m venv' python3 -m venv --help
check 'jq --version' jq --version
check 'rg --version' rg --version
check 'curl --version' curl --version
check 'git --version' git --version
check 'make --version' make --version
check 'unzip -v' unzip -v

if [ -n "$missing" ]; then
    echo "toolbox is missing:$missing" >&2
    exit 1
fi
echo 'toolbox ok'
