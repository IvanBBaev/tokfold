# tokfold-mcp

**EXPERIMENTAL.** The MCP stdio server crate of
[tokfold](https://github.com/IvanBBaev/tokfold).

## What it does

Exposes the compression engine as three Model Context Protocol tools —
`tokfold_compress`, `tokfold_decompress`, `tokfold_estimate` — over a line-framed
stdio transport, so an agent can shrink a large tool result before it enters the
prompt and recover the original bytes afterwards.

The server is reached through the `mcp` subcommand of the `tokfold` binary. That
binary ships on npm at `0.0.1` — `npm i -g tokfold` — and `tokfold mcp` is then
your MCP client's command; [Quickstart](#quickstart) walks through installing it,
connecting a client and a first session. This crate itself is **not on crates.io**:
there is no `cargo add tokfold-mcp`, no `cargo install tokfold` and no docs.rs page,
so embedding the library means building from this repository.

Both protocol eras are served: the `initialize` handshake for clients on `2025-11-25`
and earlier, and stateless per-request metadata plus `server/discover` for
`2026-07-28`.

All protocol logic sits behind `Server::handle_line`, a pure text-to-text function;
the stdio loop only adds the streams. JSON, base64, and the JSON-RPC envelope are
written here rather than pulled in, so the crate adds nothing to the dependency tree.

### Reading a compress result

`tokfold_compress` returns the recovery archive in `structuredContent.archive` and
nowhere else. The `content` block carries the rendering alone, so **a client that
reads only `content` keeps a compressed rendering it can never decompress** — reading
`structuredContent` is required if the original is ever to be recovered. Reading only
`content` is spec-conformant and is the common shape for older clients, so the tool
description says this too; it is a limitation of the current wire shape, not a bug in
such a client. `tokfold_decompress` is unaffected: it puts the restored text in both
blocks.

## Quickstart

The server is **experimental**: unhardened, unaudited, and outside the reversibility
guarantees of `tokfold-core` (see [Status](#status)). Everything passed to it is
visible to it; do not route production secrets through it.

### 1. Install

From the npm registry, which carries prebuilt binaries for macOS (arm64, x64), Linux
glibc (x64, arm64) and Windows x64:

```sh
npm i -g tokfold
tokfold mcp < /dev/null                # prints the EXPERIMENTAL notice on stderr, exits 0
```

Or build from the git repository with a Rust toolchain of 1.85 or later. The crates
are not on crates.io, so `cargo install tokfold` does not exist; install from the
repository instead:

```sh
cargo install --locked --git https://github.com/IvanBBaev/tokfold tokfold-cli
```

or from a checkout:

```sh
git clone https://github.com/IvanBBaev/tokfold
cd tokfold
cargo build --release -p tokfold-cli   # binary at target/release/tokfold
```

A source build of this repository reports version `0.0.1` — `tokfold --version`
and the server's `serverInfo` alike — so the version string does not tell a source
build from the npm release.

### 2. Connect a client

The server speaks MCP over stdio: the client starts `tokfold mcp` as a child process
and exchanges one JSON-RPC message per line. There are no flags and no environment
variables to set.

**Claude Code.** With `tokfold` on your `PATH`:

```sh
claude mcp add tokfold -- tokfold mcp
claude mcp list                        # tokfold: tokfold mcp - ✔ Connected
```

That registers the server for the current project, private to you; add
`--scope user` to register it for every project. Two other spellings work the same
way: `claude mcp add tokfold -- npx -y tokfold mcp` runs the npm package without a
global install, and a source build is registered by its absolute path,
`claude mcp add tokfold -- /abs/path/to/target/release/tokfold mcp`.
`--scope project` writes the entry to a shared `.mcp.json` instead, and Claude Code
then lists the server as pending until it is approved in an interactive session.

**Claude Desktop.** Add an entry under `mcpServers` in `claude_desktop_config.json`
(on macOS it lives in `~/Library/Application Support/Claude/`) and restart the app:

```json
{
  "mcpServers": {
    "tokfold": {
      "command": "/absolute/path/to/tokfold",
      "args": ["mcp"]
    }
  }
}
```

Use the absolute path that `which tokfold` prints. A desktop app does not
necessarily start with your shell's `PATH`, so a bare `tokfold` that resolves in a
terminal (for example under a Node version manager) may not resolve there. This
entry has not been exercised against Claude Desktop in this repository; it is the
standard `mcpServers` shape, the same command and arguments Claude Code stores for
the server.

**Any other stdio client.** Configure the command `tokfold` (or its absolute path)
with the single argument `mcp`. The client must read the server's stdout while it
writes requests — see
[What the transport expects of a client](#what-the-transport-expects-of-a-client).

### 3. A session by hand

The session below uses the `initialize` handshake of protocol `2025-06-18`, then
lists the tools, estimates and compresses a small JSON document, restores it from
the archive, and sends a line that is not JSON:

<!-- quickstart-session -->
```sh
tokfold mcp <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"quickstart","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tokfold_estimate","arguments":{"text":"{\"issues\":[{\"id\":101,\"state\":\"open\",\"title\":\"Crash on empty config\",\"labels\":[\"bug\",\"p1\"],\"assignee\":null},{\"id\":102,\"state\":\"open\",\"title\":\"Add --json flag\",\"labels\":[\"enhancement\"],\"assignee\":\"ana\"},{\"id\":103,\"state\":\"closed\",\"title\":\"Typo in README\",\"labels\":[\"docs\"],\"assignee\":\"ben\"}]}"}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"tokfold_compress","arguments":{"text":"{\"issues\":[{\"id\":101,\"state\":\"open\",\"title\":\"Crash on empty config\",\"labels\":[\"bug\",\"p1\"],\"assignee\":null},{\"id\":102,\"state\":\"open\",\"title\":\"Add --json flag\",\"labels\":[\"enhancement\"],\"assignee\":\"ana\"},{\"id\":103,\"state\":\"closed\",\"title\":\"Typo in README\",\"labels\":[\"docs\"],\"assignee\":\"ben\"}]}"}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"tokfold_decompress","arguments":{"archive":"VEtGRAEAAAAAAKICSsbJGM8oSpisi7bG1wKfpsNFrkD/thMlRr5KOCrqjSJ7Imlzc3VlcyI6W3siaWQiOjEwMSwic3RhdGUiOiJvcGVuIiwidGl0bGUiOiJDcmFzaCBvbiBlbXB0eSBjb25maWciLCJsYWJlbHMiOlsiYnVnIiwicDEiXSwiYXNzaWduZWUiOm51bGx9LHsiaWQiOjEwMiwic3RhdGUiOiJvcGVuIiwidGl0bGUiOiJBZGQgLS1qc29uIGZsYWciLCJsYWJlbHMiOlsiZW5oYW5jZW1lbnQiXSwiYXNzaWduZWUiOiJhbmEifSx7ImlkIjoxMDMsInN0YXRlIjoiY2xvc2VkIiwidGl0bGUiOiJUeXBvIGluIFJFQURNRSIsImxhYmVscyI6WyJkb2NzIl0sImFzc2lnbmVlIjoiYmVuIn1dfQ=="}}}
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"tokfold_compress","arguments":{"text":"plain log line, not JSON"}}}
EOF
```

stderr carries the one-line EXPERIMENTAL notice and nothing else; stdout carries six
replies, one per request — the notification gets none — and the process exits `0`
when its input ends. Trimmed at each `…` (the `instructions` text, the tool
definitions, the `_meta` object that every reply ends with, and in the fourth and
fifth replies the stats, the rendering's rows, the archive and the restored text),
they read:

<!-- quickstart-replies -->
```text
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"tokfold","version":"0.0.1"},"instructions":"…","resultType":"complete","_meta":…}}
{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"tokfold_compress",…},{"name":"tokfold_decompress",…},{"name":"tokfold_estimate",…}],"ttlMs":3600000,"cacheScope":"public","resultType":"complete","_meta":…}}
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"encoder=tabular bytes 290->233 tokens(est) 174->136"}],"structuredContent":{"compressed":true,"stats":{"bytesBefore":290,"bytesAfter":233,"estTokensBefore":174,"estTokensAfter":136,"byteRatio":0.803448275862069,"tokenRatio":0.7816091954022989,"encoder":"tabular","encoderId":2,"tokenizerId":0,"minSavingBps":0,"fidelity":"lossless"}},"isError":false,"resultType":"complete","_meta":…}}
{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"⟦tkfd:v1:tbl⟧\n{\"issues\":\n#3[…]\n}"}],"structuredContent":{"compressed":true,"stats":{…},"rendering":"⟦tkfd:v1:tbl⟧\n{\"issues\":\n#3[…]\n}","archive":"VEtGRAEAAAAAAKIC…fQ=="},"isError":false,"resultType":"complete","_meta":…}}
{"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"{\"issues\":[{\"id\":101,…}]}"}],"structuredContent":{"text":"{\"issues\":[{\"id\":101,…}]}"},"isError":false,"resultType":"complete","_meta":…}}
{"jsonrpc":"2.0","id":6,"result":{"content":[{"type":"text","text":"plain log line, not JSON"}],"structuredContent":{"compressed":false,"reason":"invalid JSON at byte 0: expected a JSON value","reasonCode":"invalid_json","rendering":"plain log line, not JSON"},"isError":false,"resultType":"complete","_meta":…}}
```

The rendering of the fourth reply, unescaped, is what an agent puts in its context
instead of the 290-byte original — 233 bytes, and 136 tokens against 174 by the
built-in estimator (`tokenizerId` `0`, an arithmetic approximation rather than a
real model tokenizer):

<!-- quickstart-rendering -->
```text
⟦tkfd:v1:tbl⟧
{"issues":
#3["id","state","title","labels","assignee"]
+[101,"open","Crash on empty config",["bug","p1"],null]
+[102,"open","Add --json flag",["enhancement"],"ana"]
+[103,"closed","Typo in README",["docs"],"ben"]
}
```

The archive in that reply is the one the fifth request sends back, and the text the
fifth reply returns is byte-for-byte the original input. The tool results of this
session (replies 3 to 6) are byte-identical between this repository and the published
`0.0.1` binary; replies 1 and 2 differ only in the `instructions` text and the
`tokfold_compress` description, both reworded since that release. Those two strings
account for the whole difference — 560 bytes of the first reply's line and 110 of the
second — so the rest of each line, tool order and input schemas included, is
byte-identical as well.

That pair of runs is the evidence for the replies-3-to-6 byte-identity and for the two
reworded strings being the whole of replies 1 and 2's difference — not for a
byte-identity across all six, which replies 1 and 2 refute. It is also one platform's. On
2026-10-03 the seven lines this session sends were replayed through the `bin/tokfold` of
the published `tokfold-darwin-arm64` package (SHA-1
`8e2ed283eb205aca9cf34051a7b3ede6f4f70508`) and through a release build of this
repository, on macOS arm64. A build command is part of a local binary's identity here,
and the 2026-10-03 run did not record one; the whole comparison was re-run on 2026-10-10
against `cargo build --release -p tokfold-cli` (SHA-1 `834c01f2…`) and every number in
this paragraph reproduced — replies 3 to 6 byte-identical at 527, 1 502, 921 and 386
bytes, reply 1 growing 761 → 1 321 and reply 2 2 411 → 2 521, and overwriting only those
two strings in the release replies reproducing the working tree's lines exactly. The
other four platform packages were not run, and for them this is reasoning rather than
measurement. No reply text here is platform-conditional:
`tokfold-core` carries no `cfg(unix)`, `cfg(windows)` or `cfg(target_os)` in its source
or its tests, this crate's only such switch picks how `serve_stdio` obtains stdin and
stdout rather than what any reply contains, and no crate in this workspace declares a
target-conditional dependency table. And the reason `tokfold-core` does not claim
identity across targets — `FxHasher`'s table-shape interning, which computes a different
value on a 32-bit target and through a different multiply on `sparc64` and `wasm64` —
does not separate the five platform packages: every one of them is a 64-bit target and
none is either of those two architectures.

The other era needs no handshake: at protocol `2026-07-28` every request carries
`"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}`
in its `params`, and `server/discover` lists the supported versions. An `initialize`
naming a version this server does not know is answered with `2025-11-25`, the
newest handshake-era version it serves.

### 4. The tools

- **`tokfold_estimate`** — arguments `text` and optional `profile`. Reports what
  compressing would save without returning a rendering or an archive: `content`
  is the one-line summary shown in reply 3, `structuredContent` the same `stats`
  that `tokfold_compress` returns. Use it to decide whether a payload is worth
  compressing.
- **`tokfold_compress`** — arguments `text` and optional `profile`
  (`conservative`, `balanced` or `aggressive`; `balanced` is the default, and every
  profile is reversible). Returns the rendering in `content` and, in
  `structuredContent`, `compressed`, `stats`, `rendering` and the base64
  `archive`. **The archive is only in `structuredContent`** — see
  [Reading a compress result](#reading-a-compress-result). Input the engine
  declines — not JSON, or nested deeper than 512 levels — is not a tool error: it
  comes back unchanged with `compressed: false`, a `reason` and a `reasonCode`, as
  reply 6 shows, provided the reply fits the frame (see [Limits](#5-limits)). The
  reply carries such text twice, so it stops fitting a little *under* 16 MiB, and a
  larger call is answered with the JSON-RPC error `-32602` rather than a
  `compressed: false` result. JSON with nothing to save comes back with
  `compressed: true`, encoder `passthrough`, and the input verbatim behind a
  `⟦tkfd:v1:raw⟧` marker line.
- **`tokfold_decompress`** — argument `archive`, the base64 string from
  `tokfold_compress`. Returns the original text in both `content` and
  `structuredContent.text`. A failure is a tool error (`isError: true`) carrying
  `structuredContent.code`, and no partial text ever comes back with it. Four of the
  codes mean the archive is damaged: `unsupported_version`, `reserved_bits_set`,
  `checksum_mismatch` and `corrupt`. Three do not. `invalid_base64` is decided
  before an archive exists — the argument never decoded. `bad_magic` covers bytes
  that are not this format, and also an empty argument, whose message names the size
  (`the archive is empty (0 bytes)`) rather than the magic. `not_utf8` comes from an
  *intact* archive: the header parsed, the checksum held, the payload was restored
  byte for byte, and the only problem is that those bytes are not a string, so there
  is no `text` to put them in. Those seven are the whole set a caller can reach,
  pinned as a set by
  `every_decompress_code_a_caller_can_reach_is_reached_here_and_the_catch_all_is_not`
  in `tests/contract.rs`. A written eighth, `unrecognized`, is reached by no input:
  `DecompressError` has five variants and the mapping names all five, so that arm
  exists only because the enum is `#[non_exhaustive]` and a variant added in a later
  tokfold-core has to land somewhere. The archive is **not** encrypted or
  obfuscated — at `0.0.1` it holds the original bytes verbatim behind a header — so
  store it as carefully as the text itself.

### 5. Limits

- **Frame size.** One message may be at most 32 MiB in either direction, newline
  excluded. A reply that would be larger is replaced by a JSON-RPC error `-32602`,
  never truncated; the error is addressed to the request unless the request's id
  alone is too wide to leave room for it. Because a reply carries the payload
  more than once, the practical input ceiling for `tokfold_compress` is well below
  32 MiB, and it depends on the text: about 10 MB of text that needs no escaping,
  when the rendering barely shrinks and an archive comes back (measured under
  [What the transport expects of a client](#what-the-transport-expects-of-a-client)),
  and less for text the reply must escape. A run of the control byte `0x01`, which
  JSON spells `\u0001` in both copies, was answered at 2,700,000 bytes (a
  32,400,338-byte reply) and refused at 2,900,000.
- **Engine input.** The engine accepts at most 16 MiB of text. `tokfold_estimate`
  reports a larger input as `input_too_large`; `tokfold_compress` given the same
  input answers `-32602` instead, because the unchanged text comes back twice in
  the reply. So the frame, not the engine, sets the passthrough ceiling, and it lies
  below 16 MiB. A declined reply repeats the text twice and carries the parse error
  that declined it, so the envelope belongs to the fixture: for a run of `a`, whose
  reason is `invalid JSON at byte 0: expected a JSON value`, the reply measures `337 +
  d + 2T` bytes, `d` being the request id's decimal digits and `T` the text's length.
  A one-digit id therefore answers 16,777,047 bytes of `a` (169 under 16 MiB) with an
  exactly 32 MiB reply and refuses 16,777,048 with `-32602`. An extra digit of the id
  costs one byte of reply where an extra byte of text costs two, so the ceiling falls
  by one byte for every *two* extra digits rather than for every digit: ids `10` and
  `100` share 16,777,046 and `1000` and `10000` share 16,777,045 — each measured at
  its boundary on 2026-10-10, answered at that length and refused `-32602` one byte
  above it. A declined shape whose reason is longer lowers the ceiling too,
  escape-free though it is: `[1,2` reports `unexpected end of input` and its envelope
  is 361. Text the reply must escape lowers it much further.
- **One request at a time.** Requests are answered in order, each reply written
  before the next line is read.
- **Experimental** means: no security review has been done, the wire shape may
  change between releases, the reversibility guarantees of `tokfold-core` do not
  extend to the server, and several protocol-contract questions are open (for
  example, the tool schemas declare `additionalProperties: false` but an unknown
  argument is accepted). Nothing here is production-ready.

### 6. Troubleshooting

- **A warning on stderr at start-up.** Expected: `tokfold mcp` always prints the
  EXPERIMENTAL notice there. stdout carries protocol messages only, so a client
  that shows server stderr as a log line is working correctly.
- **The client cannot start the server.** A GUI client may not inherit your
  shell's `PATH`; configure the absolute path from `which tokfold`. Running
  `tokfold mcp < /dev/null` in a terminal should print the notice and exit `0`.
- **Pending approval in Claude Code.** A server added with `--scope project` must
  be approved in an interactive `claude` session before it connects.
- **Compressed text that cannot be restored.** The client read only `content`.
  Keep `structuredContent.archive` from each `tokfold_compress` call; nothing
  else can recover the original, and the server keeps no copy.
- **A `-32602` error on a large call.** The reply would exceed the 32 MiB frame
  (see [Limits](#5-limits)). Split the payload, and keep your own copy of it: the
  error carries no text.
- **A hand-written client hangs.** It is writing a burst of requests before
  reading any reply; read stdout concurrently (see
  [What the transport expects of a client](#what-the-transport-expects-of-a-client)).
- **Which binary is running?** The version string reads `0.0.1` for the release
  and for a source build alike, so it cannot tell them apart. `which tokfold` shows
  which one your `PATH` resolves; configuring the client with an absolute path
  removes the question.

## What the transport expects of a client

The server handles one request at a time and writes each reply before it reads the
next line, so **a client must drain stdout concurrently with writing stdin**. Every
mainstream MCP client SDK already does this — a stdio transport reads the server's
output on a separate task — so this is a precondition rather than a bug you are likely
to meet through a normal client.

A client that writes a burst of requests and only then starts reading will deadlock:
once its replies fill the stdout pipe the server blocks in its write and stops draining
stdin, and the client blocks in its own write once the stdin pipe and the server's
8 KiB read buffer are full too. The threshold is what those buffers hold together, not
a large payload: on macOS, with each call a 41-byte `ping` line answered with a 144-byte line,
2,195 pipelined calls completed and 2,200 deadlocked. Making the loop read and write concurrently would mean a second thread or
an async runtime in a crate that is deliberately sans-io and dependency-free, so the
requirement is documented rather than designed away.

One frame limit applies in both directions: 32 MiB is the largest message the transport
will read *and* the largest it will write, measured the same way on both sides — on the
message, with the newline that ends the line left out. The write side is the one that
binds, because the reply carries the payload more than once: it comes back both as
`content` and as `structuredContent`, plus a base64 archive when compression succeeds.
Against the whole **call**, a large payload tends to 2.00x when the input is passed
through (no archive) and to 3.33x when it compresses but the rendering barely shrinks —
two copies of a rendering nearly the input's size, plus a base64 archive a third larger
than the bytes it carries (at v0.0.1 the archive never shrinks: it is the input behind a
header). Those are limits approached from above, not ceilings: the reply's fixed
envelope is several times the call's, so a small call measures more — counting each
line's newline, `{"k":"z"}` answers a 121-byte call with 608 bytes, 5.02x, and the same
shape measures 3.53x at a 1,000-byte value and 3.35x at 10,000; a one-byte passthrough
measures 3.13x. What the
limit does bound is the part of the reply that grows with the payload, and escaping
can only pull that part *down*, because it inflates the call and both copies in the
reply together: with the call's payload inflated by a factor *e*, the payload terms
come to 2 + 4/(3*e*) times the call's, and *e* is 1 only when nothing needed escaping.
Only a large call can approach the frame limit, and there the envelope is noise. Against the **payload** there is no ceiling of that
kind, because only the reply's two copies are escaped: an array of 50,000 one-letter
strings, `["a","b",…]`, measured 4.34x its payload and 2.89x its call.

The threshold is measured rather than derived from those ratios, and it belongs to a
shape, not to the server: for the escape-free `{"k":"z"×n}` that nears 3.33x, the
largest call whose answer still fits is **10,066,259 bytes** — a 10,066,148-byte
payload, under a third of what this same reader admits as input — and it comes back as 33,554,431 bytes against a
33,554,432-byte cap (both messages, newline left out, as the cap counts them). One more
byte of payload is refused, and what the client gets is a JSON-RPC error (`-32602`)
addressed to its own request id, not a truncated frame and not silence. The exception is a
request whose id alone leaves no room for the
addressed error — a string id within about a hundred bytes of the cap. It gets the
id-less form of the same error: still a bounded frame, but that call is uncorrelated and
its client waits. A payload the engine passes through meets the cap much later — a 5,000,000-byte
run of `A`, which is not JSON, is answered with 10,000,338 bytes (newline left out, as the
cap counts it).

Note the "against" — a reply is not unconditionally larger than the call it answers, and
the reverse is easy to arrange. A client that escapes non-ASCII as `\uXXXX` (what
Python's `json.dumps` does by default) spends six bytes per character on the way in and
gets raw UTF-8 back, so a 152 KB Cyrillic payload measured here answered a 424 KB call
with a 304 KB reply, 0.72x. That shape has a floor rather than a ceiling, and the floor
depends on the width of what was escaped: a character spelled as an escape costs six
bytes in the call (twelve as a surrogate pair) and comes back at its UTF-8 width *w*,
twice, so on the no-archive path the ratio tends to 2*w*/6 — 2/3 for Cyrillic and for a
surrogate pair, 1 for a three-byte script, and **1/3 for ASCII spelled `\u00XX`**, which
nothing stops a client from sending: `\u0041` × 100 000 measured here answered a
600,107-byte call with a 200,338-byte reply, 0.3338x (the byte counts in this paragraph,
unlike the 5.02x above, leave each line's newline out). When an archive is returned it
holds the raw bytes at 4/3 in base64, so a rendering that shrinks toward nothing pushes
the ratio toward (4/3)*w*/6, which is 2/9 for ASCII; a million `\u0020` spread over ten
thousand rows measured 0.2703x (a 6,128,998-byte call, a 1,656,953-byte reply). All of
it is harmless; it is only the other direction that meets the cap.

The limit belongs to `Server::handle_line`, not to the stdio loop, so an embedder that
writes its own transport gets it too; `Server::with_max_message_bytes` sets a different
one when the receiving side has a smaller budget. In a batch, a member whose answer does
not fit is replaced by that error alone — addressed to the member's id unless that id is
too wide for the error to fit, and naming the bytes the array had left
as well as the limit, since the member was measured against the remainder. Members are
fitted in order and each takes the widest form that fits the room left at it, reserving
nothing for the members after it, so the degradation is not monotonic in the limit: a
refusal that fits can starve a later member of its answer, and when a later member
cannot fit even as an id-less error the whole batch collapses to a single id-less error
— although a smaller limit, leaving the earlier member a narrower refusal, answered
every member. The `handle_batch` rustdoc in `src/server.rs` derives the windows where
this happens;
reserving room for later members is an open contract question.

## Status

**Not hardened.** The server sits in the secrets path — a transcript passed through it
is fully visible to it — and the audit that would make that acceptable has not been
done. Shipping did not wait for it: the `0.0.1` npm release carries this server as it
stands, labelled experimental, and `tokfold mcp` prints that warning to stderr on
start-up. So being released is not evidence of being reviewed. Nothing here is
production-ready or audited, and nothing here is covered by the reversibility
guarantees of
[`tokfold-core`](https://github.com/IvanBBaev/tokfold/tree/main/crates/tokfold-core).
Do not use it with production secrets.

Deliberately out of scope: the *proxy* shape — an upstream connection, a
content-addressed archive store, a `retrieve` tool — which needs its own threat model
before any of it is written. What exists is the server: tools in, tools out, no
persistence, no network.

## Licence

Dual-licensed under either of MIT ([LICENSE-MIT](https://github.com/IvanBBaev/tokfold/blob/main/LICENSE-MIT)) or Apache
License 2.0 ([LICENSE-APACHE](https://github.com/IvanBBaev/tokfold/blob/main/LICENSE-APACHE)) at your option. Both texts
also sit next to this file inside the crate directory, so `cargo package --list`
shows them beside it in the tarball it builds. Nothing has published that tarball:
crates.io is still on hold.

Minimum supported Rust version: 1.85 (edition 2024).
