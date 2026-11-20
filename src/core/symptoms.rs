// Copyright (c) 2026 Jody Scott
// SPDX-License-Identifier: Apache-2.0

//! Unreproduced reports grouped by symptom across tenants (the 2026-11-19 addendum): several reports of the same
//! symptom that no Skipper could reproduce yet escalate as a bug of their own. Deterministic: no model decides what
//! groups.
//!
//! A group holds the symptom only, no tenant data: its key is the symptom's shape (the service, the method, the
//! codes and the error with every tenant's and user's identifier and every number taken out), and its members are
//! keyed digests of (tenant, report), with a key drawn per process, so the group can count reports and tenants and
//! drop a report that is reproduced later without holding who sent it. Groups are in memory: the Admiral keeps no
//! record of its own (the triage record is the filing core's, #5).

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;

use admiral_model::types::services::admiral::Symptom;

/// A group escalates when it holds this many reports...
pub const ESCALATE_AT_REPORTS: usize = 3;
/// ...from at least this many tenants: one tenant's Skipper alone cannot make a public bug of its reports.
pub const ESCALATE_AT_TENANTS: usize = 2;
/// The most groups held; past it the least recently grown group that has not escalated is dropped.
pub const MAX_GROUPS: usize = 16384;
/// The most reports one group counts: it escalated long before.
pub const MAX_MEMBERS: usize = 4096;
/// The longest error shape a key keeps.
pub const MAX_ERROR_SHAPE_BYTES: usize = 512;

/// A symptom's shape: what is compared across tenants. It holds no tenant's or user's identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymptomKey {
    pub service: String,
    pub method: String,
    pub grpc_code: i32,
    pub http_status: i32,
    pub error: String,
}

impl SymptomKey {
    /// The shape of `symptom`, reported by the tenant `tenant_names` name.
    pub fn of(symptom: &Symptom, tenant_names: &[String]) -> Self {
        SymptomKey {
            service: shape(&symptom.service, tenant_names),
            method: shape(&symptom.method, tenant_names),
            grpc_code: symptom.grpc_code,
            http_status: symptom.http_status,
            error: shape(&symptom.error, tenant_names),
        }
    }
}

/// `text` lowercased, with the reporting tenant's names, every `tenants/<name>` and `/users/<name>`, and every word
/// with a digit in it (ids, counts, addresses, times) replaced, and its whitespace collapsed: two tenants' reports of
/// one failure have one shape.
pub fn shape(text: &str, tenant_names: &[String]) -> String {
    let names: Vec<String> = tenant_names.iter().filter(|n| !n.is_empty()).map(|n| n.to_ascii_lowercase()).collect();
    let lower = text.to_ascii_lowercase();
    let tokens = tokens(&lower);
    let mut out = String::new();
    for (i, token) in tokens.iter().copied().enumerate() {
        let after = |marker: &str| i >= 2 && tokens[i - 1] == "/" && tokens[i - 2] == marker;
        let replaced = if token.starts_with(char::is_whitespace) {
            if out.is_empty() {
                continue;
            }
            " "
        } else if !token.starts_with(is_word_char) {
            token
        } else if after("tenants") || names.iter().any(|n| n == token) {
            "[tenant]"
        } else if after("users") {
            "[user]"
        } else if token.chars().any(|c| c.is_ascii_digit()) {
            "#"
        } else {
            token
        };
        if out.len() + replaced.len() > MAX_ERROR_SHAPE_BYTES {
            break;
        }
        out.push_str(replaced);
    }
    out.trim_end().to_string()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@')
}

/// `text` as runs of word characters, runs of whitespace, and single other characters.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let len = if is_word_char(c) {
            rest.find(|c: char| !is_word_char(c)).unwrap_or(rest.len())
        } else if c.is_whitespace() {
            rest.find(|c: char| !c.is_whitespace()).unwrap_or(rest.len())
        } else {
            c.len_utf8()
        };
        out.push(&rest[..len]);
        rest = &rest[len..];
    }
    out
}

/// Where a report stands in its group, after `record`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grouped {
    pub key: SymptomKey,
    /// The reports the group holds now, this one included.
    pub reports: usize,
    /// The tenants they came from.
    pub tenants: usize,
    /// True exactly once per group: when this report made it reach ESCALATE_AT_REPORTS from ESCALATE_AT_TENANTS.
    pub escalate: bool,
}

struct Group {
    members: HashSet<u64>,
    /// Each tenant's digest, with how many of the members are its.
    tenants: HashMap<u64, usize>,
    escalated: bool,
    /// When it last grew, in record calls: the oldest is dropped first.
    grown: u64,
}

/// The groups, by symptom key.
pub struct SymptomGroups {
    groups: HashMap<SymptomKey, Group>,
    /// Each member's digest, with its tenant's digest and its group.
    members: HashMap<u64, (u64, SymptomKey)>,
    digests: RandomState,
    clock: u64,
}

impl Default for SymptomGroups {
    fn default() -> Self {
        SymptomGroups { groups: HashMap::new(), members: HashMap::new(), digests: RandomState::new(), clock: 0 }
    }
}

impl SymptomGroups {
    fn tenant_digest(&self, tenant: &str) -> u64 {
        self.digests.hash_one(("tenant", tenant))
    }

    fn member_digest(&self, tenant: &str, report_id: &str) -> u64 {
        self.digests.hash_one(("report", tenant, report_id))
    }

    /// Records an unreproduced report of `tenant` under `key`; a report recorded before under another key (a
    /// revision whose symptom changed) moves.
    pub fn record(&mut self, tenant: &str, report_id: &str, key: SymptomKey) -> Grouped {
        let member = self.member_digest(tenant, report_id);
        let tenant = self.tenant_digest(tenant);
        if self.members.get(&member).is_some_and(|(_, k)| *k != key) {
            self.drop_member(member);
        }
        if !self.groups.contains_key(&key) && self.groups.len() >= MAX_GROUPS {
            self.evict();
        }
        self.clock += 1;
        let clock = self.clock;
        let group = self.groups.entry(key.clone()).or_insert_with(|| Group {
            members: HashSet::new(),
            tenants: HashMap::new(),
            escalated: false,
            grown: clock,
        });
        if !group.members.contains(&member) && group.members.len() < MAX_MEMBERS {
            group.members.insert(member);
            *group.tenants.entry(tenant).or_default() += 1;
            group.grown = clock;
            self.members.insert(member, (tenant, key.clone()));
        }
        let escalate = !group.escalated
            && group.members.len() >= ESCALATE_AT_REPORTS
            && group.tenants.len() >= ESCALATE_AT_TENANTS;
        group.escalated |= escalate;
        Grouped { key, reports: group.members.len(), tenants: group.tenants.len(), escalate }
    }

    /// Takes a report out of its group: it was reproduced (it is a bug of its own now) or closed.
    pub fn remove(&mut self, tenant: &str, report_id: &str) {
        let member = self.member_digest(tenant, report_id);
        self.drop_member(member);
    }

    /// Whether the report is in the group of `key`.
    pub fn contains(&self, key: &SymptomKey, tenant: &str, report_id: &str) -> bool {
        self.members.get(&self.member_digest(tenant, report_id)).is_some_and(|(_, k)| k == key)
    }

    /// How many groups there are.
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    fn drop_member(&mut self, member: u64) {
        let Some((tenant, key)) = self.members.remove(&member) else { return };
        let Some(group) = self.groups.get_mut(&key) else { return };
        group.members.remove(&member);
        if let Some(n) = group.tenants.get_mut(&tenant) {
            *n -= 1;
            if *n == 0 {
                group.tenants.remove(&tenant);
            }
        }
        if group.members.is_empty() {
            self.groups.remove(&key);
        }
    }

    /// Drops the least recently grown group that has not escalated (or, if every group has, the least recently
    /// grown one).
    fn evict(&mut self) {
        let oldest = |escalated_too: bool| {
            self.groups
                .iter()
                .filter(|(_, g)| escalated_too || !g.escalated)
                .min_by_key(|(_, g)| g.grown)
                .map(|(k, _)| k.clone())
        };
        let Some(key) = oldest(false).or_else(|| oldest(true)) else { return };
        if let Some(group) = self.groups.remove(&key) {
            for member in group.members {
                self.members.remove(&member);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use admiral_model::types::services::admiral::Symptom;

    use crate::core::symptoms::{
        shape, SymptomGroups, SymptomKey, ESCALATE_AT_REPORTS, MAX_ERROR_SHAPE_BYTES, MAX_GROUPS,
    };

    fn symptom(error: &str) -> Symptom {
        Symptom {
            service: "gateway".to_string(),
            method: "gateway.Gateway/Route".to_string(),
            grpc_code: 16,
            http_status: 401,
            error: error.to_string(),
            ..Default::default()
        }
    }

    fn names(n: &str) -> Vec<String> {
        vec![n.to_string()]
    }

    #[test]
    fn two_tenants_reports_of_one_failure_have_one_shape() {
        let acme = SymptomKey::of(
            &symptom("token for /tenants/acme/services/web expired at 1731000000 (id 7f3a9c)"),
            &names("acme"),
        );
        let globex = SymptomKey::of(
            &symptom("Token for /tenants/globex/services/web  expired at 1731999999 (id 0b11e2)"),
            &names("globex"),
        );
        assert_eq!(acme, globex);
        assert_eq!(acme.error, "token for /tenants/[tenant]/services/web expired at # (id #)");
        assert_ne!(acme, SymptomKey::of(&symptom("permission denied"), &names("acme")));
    }

    #[test]
    fn a_shape_holds_no_tenants_or_users_identifier() {
        assert_eq!(
            shape("ACME's call by /users/alice@example.com failed", &names("acme")),
            "[tenant]'s call by /users/[user] failed"
        );
        assert_eq!(shape("tenants/initech", &[]), "tenants/[tenant]");
        assert!(shape(&"x ".repeat(1000), &[]).len() <= MAX_ERROR_SHAPE_BYTES);
    }

    #[test]
    fn several_reports_from_two_tenants_escalate_once() {
        let mut groups = SymptomGroups::default();
        let key = SymptomKey::of(&symptom("deadline exceeded"), &[]);
        let first = groups.record("/tenants/acme", "r-1", key.clone());
        assert_eq!((first.reports, first.tenants, first.escalate), (1, 1, false));
        // The same report again (a revision) counts once.
        assert_eq!(groups.record("/tenants/acme", "r-1", key.clone()).reports, 1);
        // One tenant alone does not escalate, however many reports it sends.
        for i in 2..=ESCALATE_AT_REPORTS + 1 {
            let g = groups.record("/tenants/acme", &format!("r-{i}"), key.clone());
            assert!(!g.escalate);
        }
        let g = groups.record("/tenants/globex", "r-1", key.clone());
        assert_eq!((g.reports, g.tenants, g.escalate), (ESCALATE_AT_REPORTS + 2, 2, true));
        let again = groups.record("/tenants/initech", "r-1", key.clone());
        assert_eq!((again.tenants, again.escalate), (3, false));
        assert!(groups.contains(&key, "/tenants/globex", "r-1"));
        assert!(!groups.contains(&key, "/tenants/globex", "r-2"));
    }

    #[test]
    fn a_report_reproduced_later_leaves_its_group() {
        let mut groups = SymptomGroups::default();
        let key = SymptomKey::of(&symptom("deadline exceeded"), &[]);
        groups.record("/tenants/acme", "r-1", key.clone());
        groups.record("/tenants/globex", "r-1", key.clone());
        groups.remove("/tenants/globex", "r-1");
        let g = groups.record("/tenants/acme", "r-2", key.clone());
        assert_eq!((g.reports, g.tenants, g.escalate), (2, 1, false));
        groups.remove("/tenants/acme", "r-1");
        groups.remove("/tenants/acme", "r-2");
        assert!(groups.is_empty());
    }

    #[test]
    fn a_revision_with_another_symptom_moves() {
        let mut groups = SymptomGroups::default();
        let old = SymptomKey::of(&symptom("deadline exceeded"), &[]);
        let new = SymptomKey::of(&symptom("permission denied"), &[]);
        groups.record("/tenants/acme", "r-1", old.clone());
        let g = groups.record("/tenants/acme", "r-1", new.clone());
        assert_eq!(g.reports, 1);
        assert!(groups.contains(&new, "/tenants/acme", "r-1"));
        assert!(!groups.contains(&old, "/tenants/acme", "r-1"));
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn the_groups_are_bounded() {
        let mut groups = SymptomGroups::default();
        for i in 0..MAX_GROUPS + 1 {
            let key = SymptomKey {
                service: format!("s{i}"),
                method: String::new(),
                grpc_code: 2,
                http_status: 0,
                error: String::new(),
            };
            groups.record("/tenants/acme", &format!("r-{i}"), key);
        }
        assert_eq!(groups.len(), MAX_GROUPS);
        let first = SymptomKey {
            service: "s0".to_string(),
            method: String::new(),
            grpc_code: 2,
            http_status: 0,
            error: String::new(),
        };
        assert!(!groups.contains(&first, "/tenants/acme", "r-0"));
    }
}
