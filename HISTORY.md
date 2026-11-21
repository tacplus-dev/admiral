# History: admiral

Append-only: add entries at the end, never edit or remove one (CI enforces it). One line each:
`- <YYYY-MM-DD> <who: looper|agent|jody> <kind: issue|decision|release|deploy> <one line> (<links>)`

- 2026-11-19 jody decision Created, from the Skipper/Admiral huddles (tacplus-dev/roadmap decisions/skipper-and-admiral-2026-11-01.md)
- 2026-11-20 looper issue Scaffold Admiral as a tacplus Rust service in /system: tacplus.yaml, its proto package, tacplus.lock pin, just gates (#1)
- 2026-11-20 looper issue Declare the Skipper-Admiral API (the stream's typed messages) for both sides to generate (#3)
- 2026-11-20 looper issue Report intake: serve the Skippers' streams and receive validated reports (#2)
- 2026-11-20 looper issue Triage: the LLM proposes a typed issue; group unreproduced reports by symptom (#4)
- 2026-11-20 looper issue The deterministic filing core: private triage record first, sanitized public tacplus-dev issue for a platform cause (#5)
- 2026-11-21 looper issue The forge HTTP clients' X-Tacplus-Trace-Id header has no test (GitHub, GitLab) (#6)
