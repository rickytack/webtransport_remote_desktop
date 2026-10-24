Project

webtransport_remote_desktop — a Rust workspace for a remote desktop over WebTransport.

    server/ — X11 capture, H.264 encoding (OpenH264), fragmentation and sending video over WebTransport datagrams; receives input over the control stream.

    client/ — receives datagrams, bounded loss-aware reassembly, H.264 decoding, native GUI, input capture.

    common/ — protocol (common/src/protocol/), shared types, input schema.

    docs/protocol.md — wire format and limits. Any format change starts here.

Build and checks:
text

cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

Release builds are required for manual video checks: debug builds make X11 capture and software H.264 dramatically slower.
Rust Rules

    Avoid unwrap() / expect() in production code. Exceptions: tests; lock acquisition where failure means poisoning, not normal control flow. Otherwise prefer Result + ? or explicit handling.

    Do not ignore errors silently. If an error is expected and handled, that must be visible in the code, not hidden behind an empty if let.

    Do not add dependencies unless needed. In a small project every new dependency is a noticeable share of the supply chain.

    Prefer borrowing over .clone() where practical. But not at the cost of complexity: in a small project readability wins.

    Keep code simple and idiomatic. Do not build abstractions "for the future" — they almost never pay off.

Tokio / async

    Assume a Tokio runtime already exists. Never create nested runtimes, never call Runtime::block_on() inside async code, never hide runtime creation inside helpers.

    Do not hold locks across .await.

    Put blocking work in spawn_blocking or a dedicated thread. X11 capture and software encoding already run on dedicated threads; keep that separation.

    Do not use std::thread::sleep() in async code.

    For video, prefer datagrams and loss-aware reassembly; do not "fix" loss by retransmitting over the control stream. A dropped frame is normal, not an error.

Protocol (common/src/protocol/, docs/protocol.md)

    docs/protocol.md is the source of truth for the wire format. Change the document first, then the code.

    The fixed 32-byte fragment header and negotiated datagram sizing must not change without an explicit reason and an entry in docs/protocol.md.

    Client reassembly must stay bounded: memory limits, timeouts, dropping duplicates/reordered/malformed fragments. Do not weaken these checks "to make it work".

    The control stream is a typed, length-prefixed Hello/Welcome. Do not add fields "for the future"; extend the schema when there is a real need.

    Every protocol change comes with a reassembly/parsing test (see "Tests").

Logging

    debug and above are written to the log file. A log call that can fire repeatedly (per packet, frame, input event, loop iteration, or at a rate a peer controls) must not use debug or higher unthrottled.

    For such sites: log::trace! when the event is expected and the line only helps while actively debugging; or a throttled logger when it signals a fault that should still show up in a user's log.

    In the video and input hot paths there is by default nothing to log: counters and metrics are not logs.

Editing Hygiene

    Change only what is required. A small diff beats a "correct" large one.

    Do not refactor unrelated code and do not make formatting-only changes in the same change.

    Keep naming/style consistent with nearby code.

    Comments only for a non-obvious reason, constraint, or workaround. Never restate what the code does.

Tests

    A bug fix gets a regression test for the reported behavior: it fails before the fix and passes after. One to three tests is normal.

    Assert what the user sees or what the API returns. Do not test private state or the order an algorithm runs its steps in without a reason.

    Rust unit tests go in a #[cfg(test)] module beside the code.

    Protocol and reassembly changes require tests: duplicate/reorder/malformed, timeouts, memory limits. These already exist in common/ — keep them and extend them when changing behavior.

    Do not add test infrastructure (browser runners, golden/screenshot harnesses, mock layers) for a single bug fix.

    If describing the behavior keeps requiring more and more tests, the implementation has likely become too complex. Simplify the implementation instead of adding tests.

    A test that needs long setup to reach a state usually describes a case too rare to fix.

Review

    Review only what the diff introduces. List pre-existing problems separately at the end, not among the author's findings.

    State a finding's consequence exactly: "the value is lost" and "the shortcut is inert but the value still saves" are different findings.

    Growth of the diff across review rounds is itself a finding: if the follow-ups added more state than the original fix, say so instead of asking for more handling.