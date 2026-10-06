# IRC conformance corpus

Independently authored protocol vectors used by Research 002 to compare the owned
`i2pr-irc-wire` codec and the owned IRC state/CAP model against primary
specifications and against other Rust IRC implementations.

## Provenance and licensing

Every vector here was written from a primary specification or a published
specification-derived behavior description. No fixture body was copied from
`ircv3_parse`, `irc`/`irc-proto`, `vinezombie`, `obby-proto`, `obby-client`, or any
other third-party repository. Vectors encode *expectations* drawn from RFC 2812,
the current IRCv3 specifications, and the ISUPPORT/NUMERIC reply semantics the
runtime depends on; where a specification is silent, the vector says so in its
`why` field and the expectation is only "does not crash and stays bounded".

## Layout

```text
research/irc-conformance/
  README.md
  vectors/
    wire.txt      # message parse/encode vectors: framing, limits, tags
    framing.txt   # incremental LineDecoder vectors
    state.txt     # ISUPPORT, casemapping, modes, NAMES, join semantics
    cap.txt       # downstream registration, CAP, SASL sequencing
  results/
    i2pr-irc.md           # observed owned-implementation results
    ircv3-parse.md        # observed external results (harness outside the workspace)
    irc-proto.md          # observed external results (harness outside the workspace)
    vinezombie.md         # reference analysis, not executed
    obby-proto.md         # reference analysis, not executed
```

## Vector format

Records are separated by blank lines. A record begins with `## <id> | <title>`.
Every record carries `spec`, `status` (`stable` or `draft`), and `why`
(bouncer relevance). Byte fields use explicit escapes so that byte-exact and
invalid-UTF-8 cases are unambiguous:

| escape | meaning |
| --- | --- |
| `\r` `\n` `\t` | CR, LF, tab |
| `\0` | NUL |
| `\\` | literal backslash |
| `\xHH` | raw byte `HH` |

### `wire.txt` and `framing.txt`

| field | meaning |
| --- | --- |
| `utf8` | `relevant` when the vector's meaning depends on UTF-8 validity |
| `input` | one complete line including its terminating CRLF |
| `expect` | `ok` or `reject` |
| `command` / `prefix` / `param` / `tag` | structural assertions for `ok`; `prefix` is the raw prefix token; `param` and `tag` are repeatable and order-preserving, and `tag` is written `key=value` or bare `key` |
| `chunks` | byte fragments separated by `\|`, in push order |
| `lines` | number of complete lines the decoder must yield successfully |
| `line` / `line_reject` | decoded line or rejected-fragment expectation, in order |

### `state.txt`

Upstream lines are applied in order with `line:`. Fields are evaluated in written
order, so an assertion observes the state produced by the lines above it; `check:`
marks a group boundary:

| field | meaning |
| --- | --- |
| `joined` | observed self membership, comma separated |
| `pending` / `rejected` | generation-local join attempt records |
| `nick_same` | `<a>,<b>=<true\|false>` casemapping comparison |
| `members` / `symbols` | `<channel>: <rendered name>` or `<channel>: <nick>=<symbol>` pairs |
| `topic` / `modes` | `<channel>: <value>` |
| `modes_complete` / `members_complete` | `<channel>=<true\|false>` |
| `isupport` | retained ISUPPORT tokens, space separated (a token may contain commas) |
| `total_members` | aggregate member ceiling counter |

### `cap.txt`

Session vectors. `state_line:` seeds observed upstream state, `client:` sends one
client line, `check:` starts a new expectation group, and the `expect:` lines that
follow it apply to the session state produced by the `client:` line above. Field
order is significant, so the corpus is evaluated in written order.

| field | meaning |
| --- | --- |
| `check` | start of an expectation group; required before each group |
| `ready` / `negotiating` | session registration and CAP state |
| `welcome_count` | number of `001` frames emitted so far |
| `contains` / `absent` | substring that must/must not appear in the client output so far |

## Running the owned implementation

```sh
cargo test -p i2pr-irc-wire --test conformance
cargo test -p i2pr-irc-runtime --test conformance
```

Both runners read these files directly, so a corpus change is immediately
observable. A vector that disagrees with the owned implementation fails its test
and is a research finding, not a silently updated expectation.

External comparisons were executed in temporary harnesses outside this workspace
so that no candidate crate enters `Cargo.lock`. Only the summarized outputs are
committed under `results/`.