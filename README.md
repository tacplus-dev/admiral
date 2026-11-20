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
ESCALATED with the finding; the triage waits, held with the report, for the filing core (#5).

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
