# Copyright (c) 2026 Jody Scott
# SPDX-License-Identifier: Apache-2.0

# `just gates`: the checks every change to main passes, as ci.yml runs them and as the build pipeline
# (tacplus-dev/build) runs them in the build image tacplus.lock pins.

# tacplus: the one on the path (the build image's, the release tacplus.lock pins), else ci/tacplus.sh, which runs
# that build image by digest; TACPLUS_BIN overrides both.
TACPLUS := env_var_or_default("TACPLUS_BIN", `command -v tacplus >/dev/null && echo tacplus || echo ci/tacplus.sh`)

default: gates

# Generate target/generated-sources (not committed). Generated code is never committed or edited by hand, so
# generation must leave every tracked file as it found it.
generate:
  #!/usr/bin/env bash
  set -euo pipefail
  if [[ -n "$(git ls-files target)" ]]; then
    echo "generate: target/ is generated and must not be committed" >&2; exit 1
  fi
  before="$(git diff HEAD | sha256sum)"
  {{TACPLUS}} build
  if [[ "$(git diff HEAD | sha256sum)" != "$before" ]]; then
    echo "generate: tacplus build changed tracked files:" >&2; git status --short >&2; exit 1
  fi

# protoc on the path: a dependency's build script (prost-wkt-types) runs it.
build: generate
  cargo build --locked

test: build
  cargo test --locked

# The Admiral runs in /system, one per deployment, and holds no cluster credentials (README; tacplus-dev/roadmap
# decisions/skipper-and-admiral-2026-11-01.md): its proto never declares it tenant-owned, and it builds no
# Kubernetes client, so a prompt injection that reaches it finds no way to the cluster.
system:
  #!/usr/bin/env bash
  set -euo pipefail
  if grep -rnE '^[^/]*tenant_owned[[:space:]]*:' proto/services; then
    echo "system: the Admiral is a /system service, one per deployment: proto/services must not declare it tenant-owned" >&2; exit 1
  fi
  kube="$(grep -E '^name = "(kube|kube-client|kube-runtime|k8s-openapi)"$' Cargo.lock || true)"
  test -z "$kube" || { echo "system: the Admiral holds no cluster credentials and builds no Kubernetes client: $kube" >&2; exit 1; }

# The gates every change passes before it lands.
gates: system test
