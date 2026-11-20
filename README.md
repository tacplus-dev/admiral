# Admiral

A deployment's platform agent in tacplus: one per deployment, in `/system`, LLM-driven through llmproxy, holding no
tenant data and no cluster credentials.

- **Reports:** it receives validated reports from the tenants' Skippers over the streams they open, triages them, groups
  unreproduced ones by symptom (symptom only), and files them: a private record in the triage project first, and for a
  platform cause a public tacplus-dev issue with sanitized evidence only.
- **The LLM proposes, the core acts:** its deterministic core alone holds the filing credentials and files only what it
  has checked (target allow-list, no secrets or tenant identifiers, no duplicate).
- **It never acts on the fleet:** fixes reach deployments as patches through the automated canary, and tenant config changes through the Skipper. The Admiral only subscribes, read-only, to the kernel's redacted copy of the deployment config (generated state client, Get/Watch) for triage, never to tenant slices.

Design: tacplus-dev/roadmap `decisions/skipper-and-admiral-2026-11-01.md`. Rust. Apache-2.0.

## The service

A tacplus Rust service, generated from `proto/services/admiral.proto` (`tacplus.yaml`; the release and build image
pinned in `tacplus.lock`). It is a platform service, not tenant-owned: the operator runs one instance per
deployment, as `/system/services/admiral`. Generated code goes to `target/generated-sources` and is never
committed. Its one surface is the Skipper-Admiral stream (`admiral.Admiral/BrokerSkipperChannel`, #3), which the
report intake serves (`src/intake.rs`, #2): only a tenant's Skipper may open it, the tenant is the Skipper's token's,
each message is validated against the declared bounds, a report is screened again for secrets, other tenants'
identifiers and users' identities (`src/screen.rs`) and held in memory, its highest revision, and every message is
audited (log target `admiral::audit`, never a message's text).

**Triage** (#4, `src/core`, `src/advisor`): each report and each newer revision is triaged in the background, at most
four at once. The advisor reads the report only inside a block keyed with a nonce drawn for that triage, and
proposes a typed issue with its one tool, `propose_issue`: the cause (the tenant's code, a platform service, the
generator, unknown), a target repository that must fit it (`tacplus-dev/<service>`, `tacplus-dev/tacplus` for the
generator, `seabeam/triage` otherwise), a one-line title, a body refused if it holds a secret's shape, a tenant
identifier or a user's identity, and allow-listed labels. It asks llmproxy for `claude-opus-latest`, and for
`claude-fable-latest` when the triage is hard (Opus gave no proposal or was under 50% sure, or a symptom group
escalates). The core validates the proposal again, adds the label validation sets (`bug`, or `not yet reproduced`),
and works with no model: it then proposes from the symptom's shape itself, so every report surfaces. Unreproduced
reports are grouped by symptom across tenants: the symptom's shape (identifiers and numbers taken out), with keyed
digests of its reports, so a group holds no tenant data; three reports from two tenants escalate once, as a bug of
their own (`bug`, `symptom-group`), triaged from the symptom only. The Skipper is sent TRIAGED, GROUPED and
ESCALATED with the finding.

**Filing** (#5, `src/filing`): the deterministic filing core files what triage proposed, and only after its own
checks. It runs them whoever proposed: the proposal's own checks again; the target against the allow-list (the
private triage project, or a tacplus-dev repository named in Config's `public_repositories`); the public issue's
whole text, evidence and all, for a secret's shape, a tenant identifier, a user's identity or the report's id; and
duplicates. Every report's full record goes first to the private triage project `seabeam/triage`, as a confidential
issue: one per report, and a revision updates it. Nothing goes public unless that record is written. A platform
cause also gets a public issue in its repository. It holds only sanitized evidence: the symptom's shape, the
reproduction's calls with each request a fixture generated from its shape, the trace events' kinds, services and
methods, and the versions. It names its record by id only, and the record links it. One cause, by its repository and
symptom shape, gets one public issue, which every tenant's report of it shares. The Skipper is sent FILED, with the
public issue's URL when there is one. A record closes when its public issue does. Ninety days after it closed, its
evidence is purged and its summary kept (`Filing::sweep`, hourly). The forges sit behind traits
(`src/filing/forge.rs`): GitHub's and GitLab's REST clients, and in-memory fakes for the tests. Every forge call
carries the trace of the report that caused it (header `X-Tacplus-Trace-Id`).

**The filing core's credentials** (#5, `src/filing/credentials.rs`): no secret in its config, no key in its hands. Its
kernel-signed external token (`token_for_external`, sub = its SPIFFE id) goes through GCP workload identity
federation (STS) to one service account. That account signs the `tacplus-admiral` GitHub App's JWT with a Cloud KMS
key (the App's private key, imported non-exportable), for an hour's installation token, and reads the
`seabeam/triage` project access token from one Secret Manager secret. Each token is cached until shortly before it
expires. Config names the sources, never a secret value: `wif_provider` (and `wif_audience`, by default the
provider's own), `service_account`, `github_app_key` (the key's version), `github_app_id` and
`github_installation_id` (by default the App's, 5853599 and 184144753), and `triage_token_secret` (a secret, read
at its latest version, or one of its versions). All of them or none: with none, the Admiral triages and files nothing
(each filing fails before any forge call and the report stays TRIAGED); with some, the config is refused. The GCP side
is tacplus-dev/cloud#84: its ci stack's output `admiral` gives each of these names.

**Bring-up:** the operator, then llmproxy (the operator deploys it; the Admiral reaches models only through it),
then the Admiral. Skippers connect to it once it is up.

## Gates

`just gates` is what every change to main passes (the land's gates run it):

- `just system`: the proto never declares the Admiral tenant-owned, and it builds no Kubernetes client (it holds no
  cluster credentials).
- `just llm-rule`: an LLM may propose, never hold a token or a write path (the 2026-11-19 addendum).
  `scripts/llm-rule-gate.sh`, after its fixture test, fails if the advisor's side (`src/advisor/`,
  `src/core/proposal.rs`) names the generated model, a chassis crate, a token or client, or a way out of the process.
- `just test`: `tacplus build` (which must leave every tracked file as it was), `cargo build --locked`,
  `cargo test --locked`.

It needs `just`, `cargo` and `protoc`. `tacplus` is the one on the path (in the build image `tacplus.lock` pins),
else `ci/tacplus.sh`, which runs that image by digest; `TACPLUS_BIN` overrides both.
