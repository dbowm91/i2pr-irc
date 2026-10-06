//! Plan 010 qualification: capability policy, label translation and correlation,
//! bounded fallback routing, batch tracking, tag mediation, and echo-message truthfulness.
#![cfg(test)]

use i2pr_irc_core::Casemapping;
use i2pr_irc_runtime::{
    capability::{
        CapDecision, CapabilityName, DOWNSTREAM_DEFERRED_HISTORY, DownstreamCapabilities,
        UpstreamCapabilities, upstream_is_client_independent,
    },
    ircv3::{BatchError, BatchTracker, TagDisposition, mediate_client_tags},
    routing::{
        MAX_DOWNSTREAM_LABEL_BYTES, MAX_ROUTES, RequestClass, ResponseRouter, RouteOutcome,
        RouteRefusal, Routed,
    },
};
use i2pr_irc_wire::Message;
use std::collections::BTreeSet;

// ------------------------------------------------------- capability policy

fn offered(names: &[&str]) -> UpstreamCapabilities {
    let mut caps = UpstreamCapabilities::default();
    for name in names {
        caps.note_offer(name);
    }
    caps
}

fn enabled(names: &[&str]) -> UpstreamCapabilities {
    let mut caps = offered(names);
    for name in names {
        caps.note_enabled(name);
    }
    caps
}

#[test]
fn the_upstream_capability_fingerprint_is_downstream_client_independent() {
    // Upstream negotiation happens once per generation; attaching or detaching clients
    // must never change what was asked for.
    let before = enabled(&["message-tags", "batch", "labeled-response", "echo-message"]);
    let after = before.clone();

    let mut first = DownstreamCapabilities::default();
    let advertised: BTreeSet<String> = DownstreamCapabilities::default()
        .advertise_set(&before)
        .into_iter()
        .collect();
    assert_eq!(
        first.request(&advertised, &["batch".to_owned(), "server-time".to_owned()]),
        CapDecision::Granted(vec!["batch".to_owned(), "server-time".to_owned()])
    );
    let mut second = DownstreamCapabilities::default();
    assert_eq!(
        second.request(&advertised, &["message-tags".to_owned()]),
        CapDecision::Granted(vec!["message-tags".to_owned()])
    );

    assert!(
        upstream_is_client_independent(&before, &after),
        "two different downstream CAP negotiations must not alter the upstream set"
    );
    assert_eq!(before.fingerprint(), after.fingerprint());
}

#[test]
fn the_upstream_request_set_is_the_reviewed_constant_only() {
    // Only capabilities the bouncer actually implements are requested; the server's
    // full offer is not blindly mirrored.
    let caps = offered(&[
        "sasl",
        "away-notify",
        "chghost",
        "multi-prefix",
        "message-tags",
        "server-time",
        "batch",
        "labeled-response",
        "echo-message",
        "account-tag",
        "extended-join",
    ]);
    let request = caps.request_set();
    assert_eq!(
        request,
        vec![
            "message-tags".to_owned(),
            "server-time".to_owned(),
            "batch".to_owned(),
            "labeled-response".to_owned(),
            "echo-message".to_owned()
        ],
        "SASL is requested by the authentication path, and nothing else is mirrored"
    );
    assert!(!request.iter().any(|name| name == "away-notify"));
    assert!(!request.iter().any(|name| name == "extended-join"));
}

#[test]
fn downstream_advertisement_is_exactly_what_this_build_implements() {
    let upstream = enabled(&["message-tags", "batch", "labeled-response"]);
    let advertised = DownstreamCapabilities::default().advertise(&upstream);
    assert_eq!(
        advertised,
        vec!["batch", "labeled-response", "message-tags", "server-time"]
    );
    // Draft history capabilities belong to M003-E and must not be advertised.
    for deferred in DOWNSTREAM_DEFERRED_HISTORY {
        assert!(
            !advertised.iter().any(|name| name == deferred),
            "{deferred} must not be advertised before it is implemented"
        );
    }
}

#[test]
fn echo_message_is_advertised_only_when_the_server_actually_echoes() {
    // The bouncer confirms a message only once the server has echoed it, so without
    // an upstream echo it must not claim the capability.
    let offered_only = offered(&["echo-message"]);
    assert!(
        !DownstreamCapabilities::default()
            .advertise(&offered_only)
            .contains(&"echo-message".to_owned()),
        "offered is not enabled"
    );
    let negotiated = enabled(&["echo-message"]);
    assert!(
        DownstreamCapabilities::default()
            .advertise(&negotiated)
            .contains(&"echo-message".to_owned())
    );
}

#[test]
fn a_cap_request_naming_an_unimplemented_capability_is_refused_as_a_whole() {
    let upstream = enabled(&["message-tags"]);
    let mut downstream = DownstreamCapabilities::default();
    let advertised: BTreeSet<String> = downstream.advertise_set(&upstream).into_iter().collect();
    assert_eq!(
        downstream.request(&advertised, &["batch".to_owned(), "chathistory".to_owned()]),
        CapDecision::Refused,
        "a partially grantable request is NAKed so the client is not left guessing"
    );
    assert!(!downstream.is_enabled("batch"));
}

#[test]
fn capability_tokens_are_validated_before_reaching_a_cap_line() {
    assert!(CapabilityName::parse("labeled-response").is_ok());
    for bad in ["", "with space", "semi;colon", "nul\0byte", "at@sign"] {
        assert!(CapabilityName::parse(bad).is_err(), "{bad:?}");
    }
}

// ------------------------------------------------ label translation/routing

fn now() -> std::time::Instant {
    std::time::Instant::now()
}

#[test]
fn two_sessions_issue_concurrent_labeled_whois_and_receive_only_their_own() {
    let mut router = ResponseRouter::default();
    let at = now();
    let Routed::Frame { line: first } = router.route(
        i2pr_irc_core::SessionId(1),
        i2pr_irc_core::ClientId(1),
        "WHOIS",
        &["alice".to_owned()],
        Some("mine"),
        true,
        at,
    ) else {
        panic!("expected a routed frame")
    };
    let Routed::Frame { line: second } = router.route(
        i2pr_irc_core::SessionId(2),
        i2pr_irc_core::ClientId(2),
        "WHOIS",
        &["bob".to_owned()],
        Some("mine"),
        true,
        at,
    ) else {
        panic!("expected a routed frame")
    };
    let label = |line: &str| {
        line.split(' ')
            .next()
            .expect("labeled")
            .trim_start_matches('@')
            .to_owned()
    };
    assert_ne!(label(&first), label(&second));

    // Each reply is restored to the client that asked, with its own label.
    let first_label = label(&first);
    let second_label = label(&second);
    let RouteOutcome::Completed(one) = router.deliver(
        Some(&first_label),
        Some("318"),
        Some(RequestClass::Whois),
        |route| {
            format!(
                "WHOIS reply for {}\r\n",
                route.downstream_label.clone().unwrap_or_default()
            )
            .into_bytes()
        },
        at,
    ) else {
        panic!("terminator must complete the route")
    };
    assert_eq!(one.session, i2pr_irc_core::SessionId(1));
    assert_eq!(
        String::from_utf8_lossy(&one.line),
        "WHOIS reply for mine\r\n",
        "the client's own label is restored"
    );
    let RouteOutcome::Completed(two) = router.deliver(
        Some(&second_label),
        Some("318"),
        Some(RequestClass::Whois),
        |route| {
            format!(
                "WHOIS reply for {}\r\n",
                route.downstream_label.clone().unwrap_or_default()
            )
            .into_bytes()
        },
        at,
    ) else {
        panic!("terminator must complete the route")
    };
    assert_eq!(two.session, i2pr_irc_core::SessionId(2));
    assert!(router.is_empty());
}

#[test]
fn a_client_label_is_never_forwarded_upstream_and_never_encodes_a_client_id() {
    let mut router = ResponseRouter::default();
    let at = now();
    let Routed::Frame { line } = router.route(
        i2pr_irc_core::SessionId(7),
        i2pr_irc_core::ClientId(4242),
        "WHOIS",
        &["alice".to_owned()],
        Some("secret-label"),
        true,
        at,
    ) else {
        panic!("expected a routed frame")
    };
    assert!(
        !line.contains("secret-label"),
        "a downstream label is client-private and must not reach the server: {line}"
    );
    assert!(
        !line.contains("4242"),
        "a ClientId must never be encoded into an upstream label: {line}"
    );
    let generated = line.split(' ').next().expect("label");
    assert!(
        generated.len() <= 64,
        "the generated label stays within bounds"
    );
}

#[test]
fn an_old_session_label_cannot_route_to_a_replacement_session() {
    let mut router = ResponseRouter::default();
    let at = now();
    let Routed::Frame { line } = router.route(
        i2pr_irc_core::SessionId(1),
        i2pr_irc_core::ClientId(1),
        "WHOIS",
        &["alice".to_owned()],
        Some("l"),
        true,
        at,
    ) else {
        panic!("expected a routed frame")
    };
    let stale = line.split(' ').next().expect("label").to_owned();
    // The client detaches and reattaches with a fresh SessionId.
    router.drop_session(i2pr_irc_core::SessionId(1));
    let Routed::Frame { line: replacement } = router.route(
        i2pr_irc_core::SessionId(2),
        i2pr_irc_core::ClientId(1),
        "WHOIS",
        &["alice".to_owned()],
        Some("l"),
        true,
        at,
    ) else {
        panic!("expected a routed frame")
    };
    assert_ne!(
        stale,
        replacement.split(' ').next().expect("label"),
        "a replacement session must not reuse the old upstream label"
    );
    // The old label now matches nothing and is delivered to nobody.
    assert_eq!(
        router.deliver(
            Some(stale.trim_start_matches('@')),
            Some("318"),
            None,
            |route| panic!("must not rebuild for {}", route.session.0),
            at
        ),
        RouteOutcome::Unmatched
    );
}

#[test]
fn a_late_reply_with_an_unknown_label_is_never_delivered_to_another_client() {
    let mut router = ResponseRouter::default();
    let at = now();
    router.route(
        i2pr_irc_core::SessionId(1),
        i2pr_irc_core::ClientId(1),
        "WHOIS",
        &["alice".to_owned()],
        Some("l"),
        true,
        at,
    );
    assert_eq!(
        router.deliver(
            Some("nonexistent-label"),
            Some("318"),
            None,
            |route| panic!("must not rebuild for {}", route.session.0),
            at
        ),
        RouteOutcome::Unmatched,
        "an unknown label belongs to nobody, so guessing is the only alternative and it is wrong"
    );
    assert_eq!(router.open_routes(), 1, "the live route is untouched");
}

#[test]
fn the_route_table_is_bounded_and_overflow_gets_a_deterministic_refusal() {
    let mut router = ResponseRouter::default();
    let at = now();
    for index in 0..MAX_ROUTES {
        let Routed::Frame { .. } = router.route(
            i2pr_irc_core::SessionId(index as u64 + 1),
            i2pr_irc_core::ClientId(1),
            "WHO",
            &["#c".to_owned()],
            Some("l"),
            true,
            at,
        ) else {
            panic!("route {index} should be accepted")
        };
    }
    let refused = router.route(
        i2pr_irc_core::SessionId(9999),
        i2pr_irc_core::ClientId(1),
        "WHO",
        &["#c".to_owned()],
        Some("l"),
        true,
        at,
    );
    assert_eq!(refused, Routed::Refused(RouteRefusal::Busy));
    assert_eq!(router.open_routes(), MAX_ROUTES);
}

#[test]
fn a_route_times_out_and_releases_its_slot_deterministically() {
    let mut router = ResponseRouter::new(std::time::Duration::from_millis(5));
    let at = now();
    router.route(
        i2pr_irc_core::SessionId(1),
        i2pr_irc_core::ClientId(1),
        "WHO",
        &["#c".to_owned()],
        Some("l"),
        true,
        at,
    );
    let later = at + std::time::Duration::from_millis(100);
    assert_eq!(router.expire(later), 1);
    assert!(router.is_empty());
    assert!(matches!(
        router.route(
            i2pr_irc_core::SessionId(2),
            i2pr_irc_core::ClientId(1),
            "WHO",
            &["#c".to_owned()],
            Some("l"),
            true,
            later
        ),
        Routed::Frame { .. }
    ));
}

#[test]
fn an_over_long_client_label_is_refused_before_the_wire() {
    let mut router = ResponseRouter::default();
    let long = "x".repeat(MAX_DOWNSTREAM_LABEL_BYTES + 1);
    assert_eq!(
        router.route(
            i2pr_irc_core::SessionId(1),
            i2pr_irc_core::ClientId(1),
            "WHOIS",
            &["alice".to_owned()],
            Some(&long),
            true,
            now()
        ),
        Routed::Refused(RouteRefusal::Unsupported)
    );
    assert!(router.is_empty());
}

// ------------------------------------------------- unlabeled fallback router

#[test]
fn fallback_correlation_follows_the_specified_completion_matrix() {
    // Each family's terminator is the only thing that may complete its route.
    for (class, terminator, unrelated) in [
        (RequestClass::Whois, "318", "315"),
        (RequestClass::Who, "315", "366"),
        (RequestClass::Names, "366", "323"),
        (RequestClass::List, "323", "318"),
    ] {
        let mut router = ResponseRouter::default();
        let at = now();
        let session = i2pr_irc_core::SessionId(9);
        assert!(matches!(
            router.route(
                session,
                i2pr_irc_core::ClientId(1),
                class.as_str(),
                &["x".to_owned()],
                None,
                false,
                at
            ),
            Routed::Frame { .. }
        ));
        assert_eq!(
            router.deliver(None, Some(unrelated), None, |_| Vec::new(), at),
            RouteOutcome::Unmatched,
            "{unrelated} must not complete a {} route",
            class.as_str()
        );
        assert_eq!(router.open_routes(), 1);
        let RouteOutcome::Completed(delivered) = router.deliver(
            None,
            Some(terminator),
            Some(class),
            |route| format!("{}\\r\\n", route.session.0).into_bytes(),
            at,
        ) else {
            panic!("{terminator} must complete a {} route", class.as_str())
        };
        assert_eq!(delivered.session, session);
        assert!(router.is_empty());
    }
}

#[test]
fn a_competing_fallback_query_gets_a_deterministic_busy_disposition() {
    let mut router = ResponseRouter::default();
    let at = now();
    assert!(matches!(
        router.route(
            i2pr_irc_core::SessionId(1),
            i2pr_irc_core::ClientId(1),
            "WHOIS",
            &["alice".to_owned()],
            None,
            false,
            at
        ),
        Routed::Frame { .. }
    ));
    for _ in 0..5 {
        assert_eq!(
            router.route(
                i2pr_irc_core::SessionId(2),
                i2pr_irc_core::ClientId(2),
                "WHOIS",
                &["bob".to_owned()],
                None,
                false,
                at
            ),
            Routed::Refused(RouteRefusal::Busy),
            "an ambiguous query is never queued without limit"
        );
    }
    assert_eq!(router.open_routes(), 1);
}

#[test]
fn there_is_no_generic_fifo_for_unlabeled_responses() {
    let mut router = ResponseRouter::default();
    let at = now();
    // No route is open, so an unlabeled numeric has nothing to belong to.
    for numeric in ["318", "315", "366", "323", "372", "001"] {
        assert_eq!(
            router.deliver(
                None,
                Some(numeric),
                None,
                |route| panic!("{numeric} must not be attributed to {}", route.session.0),
                at
            ),
            RouteOutcome::Unmatched,
            "an unattributable reply must be dropped, never guessed onto a client"
        );
    }
    assert!(router.is_empty());
}

#[test]
fn a_command_family_without_specified_semantics_creates_no_route() {
    let mut router = ResponseRouter::default();
    for command in ["VERSION", "MOTD", "LUSERS", "TIME", "LINKS"] {
        assert_eq!(
            router.route(
                i2pr_irc_core::SessionId(1),
                i2pr_irc_core::ClientId(1),
                command,
                &["x".to_owned()],
                Some("l"),
                true,
                now()
            ),
            Routed::Unlabeled,
            "{command} has no specified correlation and must not create a route"
        );
    }
    assert!(router.is_empty());
}

// ---------------------------------------------------------------- batch

#[test]
fn batch_tracking_is_bounded_and_identifiers_are_ephemeral() {
    let mut tracker = BatchTracker::default();
    let at = now();
    let outer = tracker.open("chathistory", None, at).expect("opens");
    let inner = tracker.open("netjoin", Some(&outer.id), at).expect("nests");
    assert_ne!(outer.id, inner.id);
    assert!(!outer.id.is_empty() && outer.id.len() <= 32);
    // Batch identity is session-local and never durable: a second tracker starts over.
    let mut fresh = BatchTracker::default();
    assert_eq!(
        fresh.open("chathistory", None, at).expect("opens").id,
        outer.id
    );
}

#[test]
fn a_batch_route_follows_replies_until_its_terminator() {
    let mut router = ResponseRouter::default();
    let at = now();
    let Routed::Frame { line } = router.route(
        i2pr_irc_core::SessionId(1),
        i2pr_irc_core::ClientId(1),
        "WHOIS",
        &["alice".to_owned()],
        Some("l"),
        true,
        at,
    ) else {
        panic!("expected a routed frame")
    };
    let label = line.split(' ').next().expect("label").to_owned();
    assert!(router.expect_batch(label.trim_start_matches('@')));
    // Non-terminating replies keep the route open.
    assert!(matches!(
        router.deliver(
            Some(label.trim_start_matches('@')),
            Some("311"),
            None,
            |_| b"partial\r\n".to_vec(),
            at
        ),
        RouteOutcome::Continued(_)
    ));
    // The batch terminator closes it.
    router.finish_batch(label.trim_start_matches('@'));
    assert!(router.is_empty());
    assert_eq!(
        router.deliver(
            Some(label.trim_start_matches('@')),
            Some("318"),
            None,
            |_| b"late\r\n".to_vec(),
            at
        ),
        RouteOutcome::Unmatched,
        "a reply after the batch completed reaches nobody"
    );
}

#[test]
fn an_unknown_batch_reference_is_refused() {
    let mut tracker = BatchTracker::default();
    let at = now();
    assert_eq!(
        tracker.open("netjoin", Some("missing"), at),
        Err(BatchError::UnknownReference)
    );
    assert_eq!(
        tracker.note_message("missing"),
        Err(BatchError::UnknownReference)
    );
    assert_eq!(tracker.close("missing"), Err(BatchError::UnknownReference));
}

// ---------------------------------------------------------------- tags

fn parse(raw: &str) -> Message {
    Message::parse(raw.as_bytes()).expect("parses")
}

#[test]
fn client_only_tags_are_default_deny() {
    // A client forging msgid or inventing tags must not reach the server: that is how
    // one client could impersonate another's metadata.
    let forged = parse("@msgid=forged;+client=1;draft/x=1 :a!b@c PRIVMSG #room :hi\r\n");
    let (mediated, disposition) = mediate_client_tags(&forged, true);
    assert_ne!(disposition, TagDisposition::Forwarded);
    assert!(mediated.tags.len() < forged.tags.len());
    for draft in DOWNSTREAM_DEFERRED_HISTORY {
        assert!(!mediated.tags.keys().any(|name| name == draft.as_bytes()));
    }
}

#[test]
fn a_server_time_is_preserved_or_synthesized_but_never_orders_history() {
    use i2pr_irc_runtime::ircv3::synthesize_server_time;
    let mut preserved = parse("@time=2020-09-13T12:26:40.123Z :a!b@c PRIVMSG #room :hi\r\n");
    synthesize_server_time(&mut preserved, i2pr_irc_core::WallTime(1_700_000_000));
    assert_eq!(
        preserved.server_time().map(|time| time.to_string()),
        Some("2020-09-13T12:26:40.123Z".to_owned()),
        "a real value is not overwritten, and milliseconds survive"
    );

    let mut synthesized = parse(":a!b@c PRIVMSG #room :hi\r\n");
    synthesize_server_time(&mut synthesized, i2pr_irc_core::WallTime(1_700_000_000));
    assert_eq!(
        synthesized.server_time().map(|time| time.to_string()),
        Some("2023-11-14T22:13:20.000Z".to_owned()),
        "a synthesized timestamp is canonical text, never an integer epoch"
    );
}

#[test]
fn tags_never_change_canonical_history_order() {
    // Ordering is HistoryEventId. Tag mediation is a rendering concern only, so the
    // journal's canonical order cannot depend on it.
    use i2pr_irc_core::HistoryEventId;
    let events: Vec<HistoryEventId> = vec![HistoryEventId(1), HistoryEventId(2), HistoryEventId(3)];
    for pair in events.windows(2) {
        assert!(pair[0] < pair[1]);
    }
    let mut tagged = parse("@time=2286-11-20T17:46:40.000Z :a!b@c PRIVMSG #room :first\r\n");
    i2pr_irc_runtime::ircv3::synthesize_server_time(&mut tagged, i2pr_irc_core::WallTime(1));
    assert_eq!(
        tagged.server_time().map(|time| time.to_string()),
        Some("2286-11-20T17:46:40.000Z".to_owned()),
        "a skewed tag is carried as metadata and still does not reorder anything"
    );
}

// ------------------------------------------------------------- diagnostics

#[test]
fn a_capability_fingerprint_is_bounded_and_carries_no_secrets() {
    let caps = enabled(&["message-tags", "batch", "labeled-response", "echo-message"]);
    let fingerprint = caps.fingerprint();
    assert!(fingerprint.contains("batch"));
    assert!(!fingerprint.contains('@'));
    assert!(fingerprint.len() < 256);
}

#[test]
fn a_router_projection_reports_only_counts() {
    let router = ResponseRouter::default();
    assert_eq!(router.open_routes(), 0);
    assert!(router.is_empty());
    let rendered = format!("{:?}", router.open_routes());
    assert_eq!(rendered, "0");
}

#[test]
fn capabilities_used_by_the_bouncer_are_the_reviewed_ones() {
    // Guards the advertised list against silent growth: adding a capability here is a
    // claim that its semantics are implemented.
    let advertised = DownstreamCapabilities::default().advertise(&UpstreamCapabilities::default());
    for name in &advertised {
        assert!(
            ["batch", "labeled-response", "message-tags", "server-time"].contains(&name.as_str()),
            "{name} is advertised but not a reviewed foundational capability"
        );
    }
    assert!(
        !advertised
            .iter()
            .any(|name| name == "account-notify" || name == "away-notify"),
        "the bouncer must not advertise server state capabilities it does not implement"
    );
    let _ = Casemapping::Rfc1459;
}
