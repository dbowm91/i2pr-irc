//! The bounded current-state projection one client receives after registration.
//!
//! A client must never be shown something untrue. This projection therefore includes
//! only what is known: observed membership, a topic the server sent, a mode snapshot
//! that is known complete, and a NAMES list only when membership is known complete.
//! Desired intent and unconfirmed join attempts are deliberately excluded, so a
//! projection can never claim a channel the bouncer is not actually in.
use crate::{RuntimeError, session::SessionHandle, state::NetworkState};
use i2pr_irc_wire::{IrcTimestamp, MAX_LINE_BYTES};
use std::collections::BTreeMap;

/// Sends the whole projection to one session handle.
///
/// `read_markers` is `Some` only for a client that negotiated `draft/read-marker`, and
/// carries the current marker per channel. Passing `None` suppresses the draft's
/// initial-marker frame entirely, so the bouncer never sends a command a client did
/// not negotiate.
#[allow(clippy::too_many_arguments)]
pub fn project(
    handle: &SessionHandle,
    state: &NetworkState,
    target: &str,
    read_markers: Option<&BTreeMap<String, IrcTimestamp>>,
) -> Result<(), RuntimeError> {
    handle.queue_normal(&format!(":bouncer 001 {target} :Welcome\r\n"))?;
    for token in &state.isupport {
        handle.queue_normal(&format!(
            ":bouncer 005 {target} {token} :are supported by this server\r\n"
        ))?;
    }
    // The bouncer's own history surface is advertised only to a client that actually
    // negotiated the capability, and only for what this build implements. A client
    // that never negotiated it must not be invited to send a request it has no batch
    // support to read.
    if handle.capabilities().manages_own_history() {
        for token in crate::chathistory::isupport_tokens() {
            handle.queue_normal(&format!(
                ":bouncer 005 {target} {token} :are supported by this server\r\n"
            ))?;
        }
    }
    for channel in state.joined_channels() {
        handle.queue_normal(&format!(":{} JOIN {channel}\r\n", state.nick))?;
        // The read-marker draft requires the server to send the channel's marker after
        // the JOIN and before RPL_ENDOFNAMES. It is emitted from the JOIN rather than
        // from the NAMES block so it still arrives when membership is incomplete, and
        // a channel with no marker yet is announced as `*` -- the draft's own
        // unknown-marker sentinel, which is also what a client would receive from a
        // `MARKREAD` get.
        if let Some(markers) = read_markers {
            handle.queue_normal(&crate::owner::frame(
                crate::chathistory::render_marker_reply(&channel, markers.get(&channel).copied()),
            ))?;
        }
        let Some(channel_state) = state.channels.get(&channel) else {
            continue;
        };
        if let Some(topic) = &channel_state.topic {
            let prefix = format!(":bouncer 332 {target} {channel} :");
            handle.queue_normal(&bounded_line(&prefix, topic))?;
        }
        // An incomplete mode snapshot is omitted rather than projected falsely.
        if let Some(modes) = channel_state.modes.render()
            && !modes.is_empty()
        {
            let line = format!(":bouncer 324 {target} {channel} +{modes}\r\n");
            if line.len() <= MAX_LINE_BYTES {
                handle.queue_normal(&line)?;
            }
        }
        // Membership is projected only when it is known to be complete.
        if channel_state.names_seen && channel_state.members_complete {
            let mut names: Vec<String> = channel_state
                .members
                .iter()
                .map(|member| member.display())
                .collect();
            names.sort();
            let list = names.join(" ");
            let prefix = format!(":bouncer 353 {target} = {channel} :");
            let budget = MAX_LINE_BYTES.saturating_sub(prefix.len() + 2).max(1);
            let mut start = 0;
            while start < list.len() {
                let mut end = (start + budget).min(list.len());
                while end > start && !list.is_char_boundary(end) {
                    end -= 1;
                }
                handle.queue_normal(&bounded_line(&prefix, &list[start..end]))?;
                start = end;
            }
            if list.is_empty() {
                handle.queue_normal(&format!("{prefix}\r\n"))?;
            }
            handle.queue_normal(&format!(
                ":bouncer 366 {target} {channel} :End of NAMES list\r\n"
            ))?;
        }
    }
    Ok(())
}

/// Joins a prefix and a possibly-oversized value into one line that respects
/// `MAX_LINE_BYTES` and only cuts on a character boundary.
fn bounded_line(prefix: &str, value: &str) -> String {
    let budget = MAX_LINE_BYTES.saturating_sub(prefix.len() + 2).max(1);
    let mut end = value.len().min(budget);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{prefix}{}\r\n", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_line_respects_the_ceiling_and_cuts_on_char_boundaries() {
        let prefix = ":bouncer 353 bot = #room :";
        let line = bounded_line(prefix, &"a".repeat(MAX_LINE_BYTES));
        assert!(line.len() <= MAX_LINE_BYTES);
        let multibyte = "é".repeat(MAX_LINE_BYTES);
        let line = bounded_line(prefix, &multibyte);
        assert!(line.len() <= MAX_LINE_BYTES);
        assert!(line.ends_with("\r\n"));
        // Truncation must never split a UTF-8 sequence.
        assert!(std::str::from_utf8(line.as_bytes()).is_ok());
    }
}
