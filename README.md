# Admiral

A deployment's platform agent in tacplus: one per deployment, in `/system`, LLM-driven through llmproxy, holding no
tenant data and no cluster credentials.

- **Reports:** it receives validated reports from the tenants' Skippers over the streams they open, triages them, groups
  unreproduced ones by symptom (symptom only), and files them: a private record in the triage project first, and for a
  platform cause a public tacplus-dev issue with sanitized evidence only.
- **The LLM proposes, the core acts:** its deterministic core alone holds the filing credentials and files only what it
  has checked (target allow-list, no secrets or tenant identifiers, no duplicate).
- **It asks, the operator acts:** the operator stays a separate deterministic service; the Admiral reaches it only
  through the operator's narrow typed API (fleet actions are a later version).

Design: tacplus-dev/roadmap `decisions/skipper-and-admiral-2026-11-01.md`. Rust. Apache-2.0.
