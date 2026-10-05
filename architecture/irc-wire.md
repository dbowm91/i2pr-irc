# IRC wire contract

The codec preserves commands and parameters as bytes, retaining unknown commands and tags. UTF-8 decoding is a presentation-layer choice; parsing does not require UTF-8. A conventional IRC line is at most 512 bytes including CRLF; a tagged line is at most 8191 bytes including CRLF. The tag section is capped at 4094 bytes, parameter count at 15, tag count at 64, and token size at 512 bytes.

`LineDecoder` accepts segmented input, emits only complete CRLF lines, and discards an oversized line through LF before resuming. `Message::parse` is the owned codec; no external IRC codec was adopted because the foundation needs explicit byte and allocation bounds plus unknown-token preservation.
