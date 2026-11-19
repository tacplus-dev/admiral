# Roadmap: admiral

Owned by Jody; changed only by an agent at Jody's request. Loopers propose in PROPOSALS.md.
Design: tacplus-dev/roadmap `decisions/skipper-and-admiral-2026-11-01.md`.

## Now
- Scaffold Admiral as a tacplus Rust service in /system, pinned in tacplus.lock, gates green (#1). Hub, at Jody's request, 2026-11-19.
- Declare the Skipper-Admiral API: the stream's typed messages both ways (#3). Hub, at Jody's request, 2026-11-19.
- Report intake: serve the Skippers' streams, every message untrusted (#2). Hub, at Jody's request, 2026-11-19.
- Triage: the LLM proposes a typed issue; unreproduced reports grouped by symptom (#4). Hub, at Jody's request, 2026-11-19.
- The deterministic filing core: private record in seabeam/triage first, a sanitized public tacplus-dev issue for a platform cause, 90-day evidence purge (#5). Hub, at Jody's request, 2026-11-19.

## Later
- Fleet actions (v2): the Admiral asks the operator to act, through the operator's narrow typed API.
- A reporting service in place of the GitLab triage project (REVISIT, Jody 2026-11-19).

## Not doing
- Merging with the operator: the operator stays a separate deterministic service.
- Any write or token on the LLM's side (the platform rule).
