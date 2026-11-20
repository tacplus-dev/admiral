// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! admiral's triage (#4): a deterministic core (`core`, the only part that may hold a client, and that works with no
//! model) and an LLM advisor (`advisor`, which reads the report it is given and proposes a typed issue, nothing
//! more), and the filing core (`filing`, #5), which alone holds the forges' credentials and files what triage
//! proposed after its own checks. The binary (main.rs) serves the Skipper channel through the generated
//! ServiceManager.

pub mod advisor;
pub mod core;
pub mod filing;
