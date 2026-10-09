//! Bounded local watch matching. Inputs are already parsed IRC messages; matching
//! never examines retained history or sends data outside attached local sessions.

use i2pr_irc_core::{BufferId, Casemapping};
use i2pr_irc_store::{BufferKind, MAX_WATCH_RULES, WatchMatchKind, WatchRule};
use i2pr_irc_wire::Message;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub(crate) const MAX_WATCH_HITS_PER_MESSAGE: usize = 8;
const RULE_COALESCE: Duration = Duration::from_secs(2);

/// Per-owner limiter: one timestamp per configured rule, with a hard rule-count cap.
#[derive(Default)]
pub(crate) struct WatchLimiter {
    last_sent: BTreeMap<u32, Instant>,
    cursor_rule: u32,
}

impl WatchLimiter {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn eligible(
        &mut self,
        rules: &[WatchRule],
        kind: BufferKind,
        target: &str,
        buffer: Option<BufferId>,
        message: &Message,
        casemapping: Casemapping,
        now: Instant,
    ) -> Vec<u32> {
        self.last_sent
            .retain(|_, at| now.saturating_duration_since(*at) < Duration::from_secs(60));
        let Some(prefix) = message.prefix.as_deref() else {
            return Vec::new();
        };
        let sender = prefix
            .split(|byte| *byte == b'!')
            .next()
            .unwrap_or_default();
        let body = message.params.get(1).map(Vec::as_slice).unwrap_or_default();
        let folded_body = casemapping.fold(body);
        let mut hits = Vec::new();
        let rule_count = rules.len().min(MAX_WATCH_RULES);
        if rule_count == 0 {
            return hits;
        }
        let start = rules[..rule_count]
            .iter()
            .position(|rule| rule.id > self.cursor_rule)
            .unwrap_or(0);
        for offset in 0..rule_count {
            let rule = &rules[(start + offset) % rule_count];
            if rule.kind != kind
                || rule.buffer.is_some_and(|scope| Some(scope) != buffer)
                || rule.target.as_ref().is_some_and(|scope| {
                    !folded_eq(casemapping, scope.as_bytes(), target.as_bytes())
                })
            {
                continue;
            }
            let term = rule.term.as_bytes();
            let matched = match rule.matcher {
                WatchMatchKind::Sender => folded_eq(casemapping, sender, term),
                WatchMatchKind::Keyword => contains_folded(&folded_body, term, casemapping),
            };
            if matched
                && self
                    .last_sent
                    .get(&rule.id)
                    .is_none_or(|at| now.saturating_duration_since(*at) >= RULE_COALESCE)
            {
                self.last_sent.insert(rule.id, now);
                hits.push(rule.id);
                self.cursor_rule = rule.id;
                if hits.len() >= MAX_WATCH_HITS_PER_MESSAGE {
                    break;
                }
            }
        }
        hits
    }
}

fn contains_folded(folded_haystack: &[u8], needle: &[u8], mapping: Casemapping) -> bool {
    let folded_needle = mapping.fold(needle);
    if folded_needle.is_empty() || folded_needle.len() > folded_haystack.len() {
        return false;
    }
    folded_haystack
        .windows(folded_needle.len())
        .any(|candidate| candidate == folded_needle)
}

fn folded_eq(mapping: Casemapping, left: &[u8], right: &[u8]) -> bool {
    mapping.fold(left) == mapping.fold(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use i2pr_irc_core::NetworkId;

    fn rule(id: u32, matcher: WatchMatchKind, term: &str) -> WatchRule {
        WatchRule {
            id,
            network: NetworkId(1),
            buffer: Some(BufferId(1)),
            kind: BufferKind::Channel,
            target: Some("#Room".into()),
            matcher,
            term: term.into(),
        }
    }

    #[test]
    fn matching_is_casemap_scoped_bounded_and_coalesced() {
        let rules = vec![
            rule(1, WatchMatchKind::Keyword, "urgent"),
            rule(2, WatchMatchKind::Sender, "Alice"),
        ];
        let message = Message::parse(b":alice!u@h PRIVMSG #room :An URGENT issue\r\n").unwrap();
        let now = Instant::now();
        let mut limiter = WatchLimiter::default();
        assert_eq!(
            limiter.eligible(
                &rules,
                BufferKind::Channel,
                "#ROOM",
                Some(BufferId(1)),
                &message,
                Casemapping::Rfc1459,
                now
            ),
            vec![1, 2]
        );
        assert!(
            WatchLimiter::default()
                .eligible(
                    &rules,
                    BufferKind::Channel,
                    "#room",
                    Some(BufferId(2)),
                    &message,
                    Casemapping::Rfc1459,
                    now,
                )
                .is_empty(),
            "BufferId scopes do not cross into another buffer"
        );
        assert!(
            limiter
                .eligible(
                    &rules,
                    BufferKind::Channel,
                    "#room",
                    Some(BufferId(1)),
                    &message,
                    Casemapping::Rfc1459,
                    now + Duration::from_millis(50)
                )
                .is_empty()
        );
        assert_eq!(
            limiter.eligible(
                &rules,
                BufferKind::Channel,
                "#room",
                Some(BufferId(1)),
                &message,
                Casemapping::Rfc1459,
                now + RULE_COALESCE
            ),
            vec![1, 2]
        );
        assert!(
            limiter
                .eligible(
                    &rules,
                    BufferKind::Channel,
                    "#else",
                    Some(BufferId(1)),
                    &message,
                    Casemapping::Rfc1459,
                    now + Duration::from_secs(5)
                )
                .is_empty()
        );
    }

    #[test]
    fn match_bursts_are_coalesced_and_hits_per_message_are_capped() {
        let message =
            Message::parse(b":alice!u@h PRIVMSG #room :urgent urgent urgent\r\n").unwrap();
        let rules: Vec<_> = (1..=128)
            .map(|id| rule(id, WatchMatchKind::Keyword, "urgent"))
            .collect();
        let now = Instant::now();
        let mut limiter = WatchLimiter::default();
        assert_eq!(
            limiter
                .eligible(
                    &rules,
                    BufferKind::Channel,
                    "#room",
                    Some(BufferId(1)),
                    &message,
                    Casemapping::Rfc1459,
                    now
                )
                .len(),
            MAX_WATCH_HITS_PER_MESSAGE
        );
        let second = limiter.eligible(
            &rules,
            BufferKind::Channel,
            "#room",
            Some(BufferId(1)),
            &message,
            Casemapping::Rfc1459,
            now + Duration::from_millis(1),
        );
        assert_eq!(second, (9..=16).collect::<Vec<_>>());
        let mut total = MAX_WATCH_HITS_PER_MESSAGE + second.len();
        for tick in 2..1000 {
            let hits = limiter.eligible(
                &rules,
                BufferKind::Channel,
                "#room",
                Some(BufferId(1)),
                &message,
                Casemapping::Rfc1459,
                now + Duration::from_millis(tick),
            );
            assert!(hits.len() <= MAX_WATCH_HITS_PER_MESSAGE);
            total += hits.len();
        }
        assert_eq!(
            total, MAX_WATCH_RULES,
            "each configured rule fires at most once in the coalescing window"
        );
    }
}
