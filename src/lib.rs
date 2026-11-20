// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! admiral's triage (#4): a deterministic core (`core`, the only part that may hold a client, and that works with no
//! model) and an LLM advisor (`advisor`, which reads the report it is given and proposes a typed issue, nothing
//! more). The binary (main.rs) serves the Skipper channel through the generated ServiceManager.

pub mod advisor;
pub mod core;
