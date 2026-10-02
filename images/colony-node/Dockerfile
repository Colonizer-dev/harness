# The colony-node image: the stock node:24-bookworm plus the JavaScript package managers a colony's
# own repository may declare. The stock image carries npm, yarn classic and corepack, but not bun or
# pnpm, and a done-claim verifier that needs one of those reports the check unverifiable
# (crates/colonizer/src/verify.rs). This image adds them so that path can run.
#
# Built by .github/workflows/colony-image.yml, which also checks the base digest below against the
# node row in crates/colonizer/images.lock so the two cannot drift. The node preset keeps using stock
# node:24-bookworm until this image is published and its digest pinned in images.lock.
FROM node:24-bookworm@sha256:64af3819f9275802414d7cdc38c27e9d82bd564dec4d4da87d008255d36c63b4

# Bun, pinned. The amd64 build is deliberately the -baseline one: colony microVMs may not expose
# AVX2, and the default x64 build crashes on a CPU without it. arm64 has no baseline variant. The
# sha256 sums are from the release's SHASUMS256.txt, each also checked by downloading the zip.
# TARGETARCH is set by `docker buildx` (amd64, arm64).
ARG TARGETARCH
ARG BUN_VERSION=1.4.2
ARG BUN_SHA256_amd64=c678040f14fe0440eb839d37cbd0ce4c051a32da72806ac97de6a6aab6bf728f
ARG BUN_SHA256_arm64=54328bbc2d9c8e0c9f892c544d66c57a83b84139e34909e5ee81758f1ac8fda7

# pnpm, pinned. The npm `pnpm` package is only a wrapper: its CLI is a native binary shipped in a
# per-arch `@pnpm/exe.linux-<arch>` package, which npm would otherwise fetch as an unpinned optional
# dependency. So both tarballs are fetched and checked against a sha256, and the wrapper is installed
# with `--ignore-scripts --omit=optional`, so npm fetches neither that dependency nor runs a script;
# the pinned platform package is then unpacked where the wrapper looks for it, and the wrapper's own
# install.js is run to link the binary over its placeholder bin. Every byte comes from a digest here.
ARG PNPM_VERSION=12.8.1
ARG PNPM_SHA256=90762b8105e833164186b011efe5cde13841693ac8eaf784d2974a6e1c01fba3
ARG PNPM_EXE_SHA256_amd64=8178a5581db169d9913fd6f918933b141cd6a760aab75a3c1f25e36a3ff1b5d8
ARG PNPM_EXE_SHA256_arm64=538533d8edc219d75555e436cbb81c21ad89374978d456eca6bcd44c746afadc

# base is buildpack-deps, so curl, ca-certificates and unzip are already present; no apt here.
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) bun_arch=x64-baseline; bun_sha="$BUN_SHA256_amd64"; pnpm_arch=x64; pnpm_sha="$PNPM_EXE_SHA256_amd64" ;; \
      arm64) bun_arch=aarch64; bun_sha="$BUN_SHA256_arm64"; pnpm_arch=arm64; pnpm_sha="$PNPM_EXE_SHA256_arm64" ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/bun.zip \
      "https://github.com/oven-sh/bun/releases/download/bun-v${BUN_VERSION}/bun-linux-${bun_arch}.zip"; \
    echo "${bun_sha}  /tmp/bun.zip" | sha256sum -c -; \
    unzip -q /tmp/bun.zip -d /tmp/bun; \
    install -m 0755 "/tmp/bun/bun-linux-${bun_arch}/bun" /usr/local/bin/bun; \
    ln -s /usr/local/bin/bun /usr/local/bin/bunx; \
    rm -rf /tmp/bun /tmp/bun.zip; \
    curl -fsSL -o /tmp/pnpm.tgz "https://registry.npmjs.org/pnpm/-/pnpm-${PNPM_VERSION}.tgz"; \
    echo "${PNPM_SHA256}  /tmp/pnpm.tgz" | sha256sum -c -; \
    curl -fsSL -o /tmp/pnpm-exe.tgz \
      "https://registry.npmjs.org/@pnpm/exe.linux-${pnpm_arch}/-/exe.linux-${pnpm_arch}-${PNPM_VERSION}.tgz"; \
    echo "${pnpm_sha}  /tmp/pnpm-exe.tgz" | sha256sum -c -; \
    npm install -g --ignore-scripts --omit=optional --no-fund --no-audit /tmp/pnpm.tgz; \
    pnpm_global="$(npm root -g)"; \
    mkdir -p "${pnpm_global}/@pnpm/exe.linux-${pnpm_arch}"; \
    tar -xzf /tmp/pnpm-exe.tgz -C "${pnpm_global}/@pnpm/exe.linux-${pnpm_arch}" --strip-components=1; \
    node "${pnpm_global}/pnpm/install.js"; \
    rm -rf /tmp/pnpm.tgz /tmp/pnpm-exe.tgz

# yarn classic (1.22.22) and corepack already come with the base image, so `yarn` and the verifier's
# `corepack yarn` / `corepack pnpm` paths work unchanged. This line makes a broken install fail the
# build rather than a colony's verification: each tool must report its version.
RUN bun --version && pnpm --version && yarn --version
