# Proposals: admiral

Loopers propose here; Jody decides, and an accepted proposal becomes a ROADMAP line.

- **A release image and release workflow** (Dockerfile, `release.yml`, as llmproxy's: non-root 65532, base by digest,
  the launcher pinned), so a fleet can run the Admiral. Not part of the scaffold (#1): nothing deploys it yet.
- **The Admiral in the fleets' manifests, after llmproxy** (the bring-up order: operator, llmproxy, Admiral;
  tacplus-dev/roadmap decisions/skipper-and-admiral-2026-11-01.md): a change in the fleet repositories, once there is
  an image.
- **The LLM-rule build gate here** (the 2026-11-19 addendum: the advisor's module gets no state client and no token
  handle, and a gate fails the build if it imports one): with the triage advisor (#4), extend `just system`.
- **CI workflows** (#1): `.github/workflows/ci.yml` running `just gates` (rustup stable, `protobuf-compiler` and
  `just` from apt, `TACPLUS_BIN=ci/tacplus.sh`), and `planning-files.yml` calling
  `tacplus-dev/looper/.github/workflows/planning-files.yml@main` (HISTORY.md says CI enforces it append-only), as
  llmproxy has them. The looper's credential may not push `.github/workflows/`: a person adds them.
- **Drop the generated crates' license clarifications** (#1): tacplus 2.6.13 writes no `license` into the manifests of
  the 22 crates it generates (the chassis and `admiral-model`), so `deny.toml` clarifies each as Apache-2.0, the SPDX
  header tacplus.yaml puts on every generated file, as tacplus-dev/skipper does. The fix is the generator's
  (tacplus-dev/tacplus#368, its 2.6 backport tacplus-dev/tacplus#390): once `tacplus.lock` pins the 2.6 patch carrying
  it, delete the `[[licenses.clarify]]` entries.
- **Three unmaintained crates reach the Admiral only through the generated chassis** (cargo-deny, reported but not
  failing the policy): `chrono` (RUSTSEC-2026-0543), `rustls-pemfile` (RUSTSEC-2025-0134), `unsafe-libyaml`
  (RUSTSEC-2024-0449), as in tacplus-dev/skipper. The fix is tacplus's generator, not this repository.
- **A wire test of the Skipper-Admiral stream across both builds** (#3): skipper generates its client from
  `proto/services/admiral.proto` and the Admiral its server, in separate builds. Once intake (#2) and skipper#3 exist,
  a cross test that opens the stream from the Skipper's generated `Rotating` client to the Admiral's server, across a
  token rotation and a reconnect with `SkipperHello` cursors, pins that the two generations agree.
- **`Evidence.trace_events` embeds `trace.TraceEvent`, whose `rpc` is the build's generated `AccountingMethod`** (#3):
  the Skipper's and the Admiral's builds generate different sets of methods, so an `rpc` the Admiral's build does not
  know decodes as an unknown field. The design wants trace data typed (its sanitizing generated); if triage (#4) needs
  the method of every hop, a fleet-wide `AccountingMethod` (tacplus) or a method name beside it is the fix.
- **The intake's reports outlive a restart only through the Skippers** (#2): the Admiral holds the reports in memory
  (its state root is empty), so after a restart every hello names them as unknown and the Skippers send them again.
  Once the filing core (#5) writes the private triage record, the intake can answer a hello from that record instead.
- **A per-stream message rate and a hello deadline in the intake** (#2): a stream is bounded per message and a
  tenant's held reports to 4096, but a Skipper may send revisions as fast as it likes, and a stream that never sends
  its hello stays open. Rate-limit per tenant and close a stream with no hello within a deadline.
- **Nonce-keyed provenance blocks for the triage advisor** (#4): the intake screens and holds a report but gives no
  LLM any of its text. The advisor is where report text first meets a model; it must reach it only inside nonce-keyed
  blocks (tacplus-dev/looper#40), with the intake's screening counts beside it.
