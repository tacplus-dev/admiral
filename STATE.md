# State: admiral

Last updated: 2026-11-20 by the looper (#5, filing).

**Version:** none released; tacplus 2.6.16 pinned (`tacplus.lock`, floor 2.6).

**What works:** a tacplus Rust service in /system: proto package `admiral` (an empty Config and state root, not
tenant-owned: one instance per deployment, `/system/services/admiral`), registered as service 109
(tacplus-dev/registry#13). The Skipper-Admiral API is declared (#3): `admiral.Admiral/BrokerSkipperChannel`, one
bidi S2S stream only `skipper` may open, with its typed messages both ways (hello and resume cursors, the report and
its revisions, progress stages, the Admiral's requests and the Skipper's answers), every field bounded; skipper#3
generates its client from it. The intake serves it (#2, `src/intake.rs`): the tenant is the Skipper's verified
token's namespace; the hello comes first and is answered with the reports the Admiral holds no record of and the
updates the Skipper missed; a report is accepted, its highest revision held in memory (at most 4096 per tenant) with
its first update, RECEIVED; a newer stream of a tenant replaces its open one (sent `last`). Every message is
untrusted: the generated wrapper checks its bounds, the Admiral screens a report again for secrets, other tenants'
identifiers and users' identities, redacting in place (`src/screen.rs`), never logs its text, and audits every
message (`admiral::audit`). Each report and newer revision is triaged (#4, `src/core`, `src/advisor`), at most four
at once, on the trace of the message that brought it: the LLM advisor reads it inside a nonce-keyed block and
proposes a typed issue (cause, a target that must fit it, title, a body refused if it leaks a secret's shape, a
tenant identifier or a user's identity, allow-listed labels) through llmproxy (`claude-opus-latest`;
`claude-fable-latest` for hard triage); the deterministic core validates it again, adds `bug` or `not yet
reproduced`, and proposes on its own with no model. Unreproduced reports are grouped by symptom shape across tenants
with no tenant data; three from two tenants escalate once as a bug of their own. The Skipper is sent TRIAGED,
GROUPED and ESCALATED; the triage is held with the report. A triaged report is filed (#5, `src/filing`) by the
deterministic core alone: the proposal is checked again, its target against the allow-list (`seabeam/triage`, or a
tacplus-dev repository in Config's `public_repositories`), the public text for secrets, tenant identifiers, users'
identities and the report's id, and duplicates (one record per report, one public issue per cause). The private
record, a confidential `seabeam/triage` issue, comes first; a platform cause also gets a public tacplus-dev issue of
sanitized evidence and a reproduction from generated fixtures, naming the record by id only. The Skipper is sent
FILED (with the issue's URL); an escalated group's bug is filed too. Records close with their public issue, and 90
days later their evidence is purged and their summary kept (an hourly sweep). Every forge call carries the report's
trace. Credentials are option A (`src/filing/credentials.rs`): the Admiral's kernel-signed token, through GCP
workload identity federation, becomes one service account, which signs the `tacplus-admiral` App's JWT with Cloud
KMS and reads the triage project's token from Secret Manager; Config names the sources (tacplus-dev/cloud#84's ci
output `admiral`), never a secret, all or none, and with none files nothing. The intake handler holds no state
client, token handle or cluster credential; the advisor's side holds no client at all. `just gates` passes: no
tenant ownership, no Kubernetes client, the LLM-rule gate and its fixture test, `tacplus build`, `cargo build`,
`cargo test` (the envelopes round-trip, the bounds refuse, the generated Authorizer admits only a Skipper, the
intake's stream and screening, triage, its proposals and groups, filing against fake forges, the credentials'
exchange chain against fake GCP and GitHub APIs).

**Gaps:** filing has never run against the real GCP or GitHub: the App's key is not yet imported into KMS, the
triage token's secret has no version, and the fleet's Config does not yet name the sources (tacplus-dev/cloud#84's
hand steps), so a deployment files nothing until then; the held reports and the symptom groups are lost on a restart
(the Skippers send the reports again); no fleet routes `claude-fable-latest` yet, so hard triage keeps Opus's
proposal (PROPOSALS.md); no rate limit or hello deadline on a stream (PROPOSALS.md); no release image, so no fleet
runs it; no CI workflows (a person adds them: PROPOSALS.md).
