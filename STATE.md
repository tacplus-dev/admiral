# State: admiral

Last updated: 2026-11-20 by the looper (#1, the scaffold).

**Version:** none released; tacplus 2.6.13 pinned (`tacplus.lock`, floor 2.6).

**What works:** an empty tacplus Rust service in /system: proto package `admiral` (an empty Config and state root,
not tenant-owned: one instance per deployment, `/system/services/admiral`), registered as service 109
(tacplus-dev/registry#13). Its handler holds nothing. `just gates` passes: no tenant ownership, no Kubernetes
client, `tacplus build`, `cargo build`, `cargo test`.

**Gaps:** no surface of its own yet (the Skipper-Admiral stream is #3; intake, triage and filing #2, #4, #5); no
release image, so no fleet runs it; no CI workflows (a person adds them: PROPOSALS.md).
