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
