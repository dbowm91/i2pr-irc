# IRC wire contract

## Byte limits

The owned codec is byte-oriented and does not require UTF-8 for ordinary command/prefix/parameter bytes. NUL, embedded CR, and embedded LF are rejected. IRCv3 tag values are unescaped and validated as UTF-8; malformed UTF-8 tag values are retained as keys with absent values, matching the specification's permitted drop-value behavior.

RFC 2812 §2.3 limits an ordinary message body including CRLF to 512 bytes. IRCv3 Message Tags gives a separate 8191-byte maximum for the tag prefix including `@` and its terminating space; the remaining IRC message still has the 512-byte limit. Thus the maximum aggregate tagged line is 8703 bytes, checked as two independent regions. The tag data sent by a client and the tag data added by a server are each bounded to 4094 bytes by the direction-specific validator. The parser's overall structural region is bounded independently so it can represent relayed client-only and server tags.

The current implementation also freezes maximum parameter count at 15, distinct tag-key count at 128, and token/prefix length at 512 bytes. These are local resource ceilings, not IRC protocol claims. Oversized lines are rejected and discarded through LF; a subsequent complete valid line can be decoded without retaining attacker-sized input. A single `push()` returns at most 256 decoded messages plus one `TooManyMessages` error; on that overload it discards the rest of that input chunk and, if needed, discards through the next LF to restore framing.

## Representation and preservation

`Message` retains unknown commands/numerics, byte parameters, prefixes, and case-sensitive opaque tags. Duplicate tag keys retain the final value. Empty tag values normalize to missing values. Tag key order is not semantically meaningful. Invalid escapes discard the escape backslash; a final lone backslash emits no byte, as specified by IRCv3. The encoder validates output framing and preserves a leading colon in a trailing parameter.

The parser is owned rather than based on an IRC codec dependency because the bouncer needs independent tagged/body limits, bounded incremental framing, opaque unknown values, and explicit hostile-input disposition. Tests cover exact/max-plus-one limits, golden semantic round trips, every split of representative messages, concatenation, and deterministic arbitrary-byte no-panic/bounded-state properties.

Specifications: [RFC 2812 §2.3](https://www.rfc-editor.org/info/rfc2812/); [IRCv3 Message Tags](https://ircv3.net/specs/extensions/message-tags.html).
