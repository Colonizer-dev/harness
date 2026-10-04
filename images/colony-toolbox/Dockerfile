# The toolbox image for the python, rust and go presets (#753): the pinned upstream base plus the
# same small, language-agnostic tool set colony-node carries — python3 with pip and venv, PyYAML,
# jq, ripgrep, curl, git, make, unzip and ca-certificates. One generic Dockerfile serves every
# non-node preset (they differ only in the base): the pinned reference from
# crates/colonizer/images.lock (`name:tag@sha256:...`) arrives as the BASE build-arg, so this file
# keeps no digest of its own and the lock stays the single pin. Built by
# .github/workflows/colony-image.yml, which also runs images/toolbox-test.sh against the result.
ARG BASE
FROM ${BASE}

# PyYAML, pinned, for whichever `python3` ends up first on PATH. On rust:1-bookworm and
# golang:1-bookworm that is Debian's own python3 (3.11), which the python3-yaml package below
# already covers. On python:3.13-bookworm the image's interpreter sits at /usr/local/bin and
# precedes Debian's, so it never sees Debian's dist-packages yaml: the check below finds that and
# installs PyYAML into that interpreter instead, which `PIP_BREAK_SYSTEM_PACKAGES` below lets pip do.
ARG PYYAML_VERSION=6.0.3

# The Debian security upgrade and the toolbox install, the same apt layer colony-node carries.
RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get -y upgrade --no-install-recommends; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      python3 python3-pip python3-venv python3-yaml jq ripgrep curl git make unzip ca-certificates; \
    rm -rf /var/lib/apt/lists/*

# Disposable VM: `pip3 install` may write past PEP 668 into Debian's externally-managed python3.
ENV PIP_BREAK_SYSTEM_PACKAGES=1

RUN set -eux; \
    if ! python3 -c 'import yaml' 2>/dev/null; then \
      pip3 install --no-cache-dir "PyYAML==${PYYAML_VERSION}"; \
    fi
