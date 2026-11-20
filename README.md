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

**Bring-up:** the operator, then llmproxy (the operator deploys it; the Admiral reaches models only through it),
then the Admiral. Skippers connect to it once it is up.

## Gates

`just gates` is what every change to main passes (the land's gates run it):

- `just system`: the proto never declares the Admiral tenant-owned, and it builds no Kubernetes client (it holds no
  cluster credentials).
- `just test`: `tacplus build` (which must leave every tracked file as it was), `cargo build --locked`,
  `cargo test --locked`.

It needs `just`, `cargo` and `protoc`. `tacplus` is the one on the path (in the build image `tacplus.lock` pins),
else `ci/tacplus.sh`, which runs that image by digest; `TACPLUS_BIN` overrides both.
