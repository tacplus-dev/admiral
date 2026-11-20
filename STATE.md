# State: admiral

Last updated: 2026-11-20 by the looper (#2, report intake).

**Version:** none released; tacplus 2.6.13 pinned (`tacplus.lock`, floor 2.6).

**What works:** a tacplus Rust service in /system: proto package `admiral` (an empty Config and state root,
not tenant-owned: one instance per deployment, `/system/services/admiral`), registered as service 109
(tacplus-dev/registry#13). The Skipper-Admiral API is declared (#3): `admiral.Admiral/BrokerSkipperChannel`, one
bidi S2S stream only `skipper` may open, with its typed messages both ways (hello and resume cursors, the report and
its revisions, progress stages, the Admiral's requests and the Skipper's answers), every field bounded; skipper#3
generates its client from it. The intake serves it (#2, `src/intake.rs`): the tenant is the Skipper's verified
token's namespace; the hello comes first and is answered with the reports the Admiral holds no record of and the
updates the Skipper missed; a report is accepted, its highest revision held in memory (at most 4096 per tenant) with
its first update, RECEIVED; a newer stream of a tenant replaces its open one (sent `last`). Every message is untrusted:
the generated wrapper checks its bounds, the Admiral screens a report again for secrets, other tenants' identifiers
and users' identities, redacting in place (`src/screen.rs`), never logs its text, and audits every message
(`admiral::audit`). The handler holds no state client, token handle or cluster credential. `just gates` passes: no
tenant ownership, no Kubernetes client, `tacplus build`, `cargo build`, `cargo test` (the envelopes round-trip, the
bounds refuse, the generated Authorizer admits only a Skipper, the intake's stream and screening).

**Gaps:** triage and filing (#4, #5): no report goes past RECEIVED, and the held reports are lost on a restart (the
Skippers send them again); no rate limit or hello deadline on a stream (PROPOSALS.md); no release image, so no
fleet runs it; no CI workflows (a person adds them: PROPOSALS.md).
