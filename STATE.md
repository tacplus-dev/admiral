# State: admiral

Last updated: 2026-11-20 by the looper (#3, the Skipper-Admiral API).

**Version:** none released; tacplus 2.6.13 pinned (`tacplus.lock`, floor 2.6).

**What works:** a tacplus Rust service in /system: proto package `admiral` (an empty Config and state root,
not tenant-owned: one instance per deployment, `/system/services/admiral`), registered as service 109
(tacplus-dev/registry#13). The Skipper-Admiral API is declared (#3): `admiral.Admiral/BrokerSkipperChannel`, one
bidi S2S stream only `skipper` may open, with its typed messages both ways (hello and resume cursors, the report and
its revisions, progress stages, the Admiral's requests and the Skipper's answers), every field bounded; skipper#3
generates its client from it. The channel answers UNIMPLEMENTED until intake (#2) serves it. Its handler holds
nothing. `just gates` passes: no tenant ownership, no Kubernetes client, `tacplus build`, `cargo build`,
`cargo test` (the envelopes round-trip, the bounds refuse, the generated Authorizer admits only a Skipper).

**Gaps:** intake, triage and filing (#2, #4, #5); no release image, so no fleet runs it; no CI workflows (a person
adds them: PROPOSALS.md).
