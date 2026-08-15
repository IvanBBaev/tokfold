//! Contract tests for [`tokfold_mcp::EXPERIMENTAL_NOTICE`], the crate's one warning.
//!
//! The protocol surface — the JSON-RPC framing, the dual-era dispatcher, the tool
//! registry — is real and is exercised in `session.rs` and `contract.rs`. This file
//! covers the one exported item that is not protocol: the notice `tokfold mcp` prints
//! to stderr before it starts serving. It is not decoration. The server it introduces
//! is unhardened and unaudited, it sits in the secrets path — a transcript passed
//! through it is fully visible to it — and the audit that would make that acceptable
//! is a separate milestone gating any public launch. The notice is the only thing
//! standing between a user and pointing that component at a production transcript, so
//! its wording and its shape are a contract.
//!
//! These are integration tests deliberately: they see the crate exactly as the
//! downstream consumer (`tokfold-cli`) does, so demoting the constant out of the
//! public API breaks the build here rather than silently somewhere else.

use tokfold_mcp::EXPERIMENTAL_NOTICE;

/// `cmd_mcp` in `tokfold-cli` emits the notice with a single `eprintln!`, so the
/// constant must be exactly one line.
///
/// This is the least obvious way the notice can rot. The literal is written with a
/// Rust line-continuation escape — a lone `\` at end of source line — which is
/// invisible in review and which rustfmt neither adds nor protects. Delete it and the
/// constant silently gains a newline plus the source indentation in mid-sentence,
/// turning one stderr line into two ragged ones. Nothing else in the workspace would
/// notice: the existing CLI assertion only checks that stderr *contains* a keyword.
#[test]
fn notice_is_one_unbroken_line() {
    assert!(
        !EXPERIMENTAL_NOTICE.contains('\n'),
        "the notice must be a single line, but it embeds a newline: {EXPERIMENTAL_NOTICE:?}"
    );
    assert!(
        !EXPERIMENTAL_NOTICE.contains('\r'),
        "the notice must be a single line, but it embeds a carriage return: {EXPERIMENTAL_NOTICE:?}"
    );
}

/// The same line-continuation hazard, one step further along.
///
/// A source-level rewrap that keeps the `\` but changes the indentation, or a
/// hand-joining of the two source lines that forgets to collapse the leading spaces,
/// produces a notice that reads `not covered     by the ...` on a user's terminal.
/// That is a lower-stakes failure than a split line, so it is easier to ship.
#[test]
fn notice_carries_no_stray_whitespace() {
    assert!(
        !EXPERIMENTAL_NOTICE.contains("  "),
        "the notice must not contain a run of spaces: {EXPERIMENTAL_NOTICE:?}"
    );
    assert!(
        !EXPERIMENTAL_NOTICE.contains('\t'),
        "the notice must not contain a tab: {EXPERIMENTAL_NOTICE:?}"
    );
    assert_eq!(
        EXPERIMENTAL_NOTICE.trim(),
        EXPERIMENTAL_NOTICE,
        "the notice must not be padded with leading or trailing whitespace"
    );
}

/// The notice is written to a raw stderr stream that is frequently a terminal, while
/// the server's framed JSON-RPC messages go to stdout.
///
/// An embedded control character is therefore two problems at once: a terminal escape
/// sequence in a security warning is an injection vector, and a stray control byte is
/// exactly what corrupts a line-framed stream if a diagnostic ever reaches the wrong
/// one — the transport's one absolute rule is that nothing but MCP messages go to
/// stdout, and the notice is the first thing the binary writes. This is
/// not a hypothetical failure mode for this repository — source files here have
/// previously carried raw control bytes, undetected, because the tooling that would
/// have surfaced them treated the file as binary and stayed silent.
#[test]
fn notice_contains_no_control_or_terminal_escape_characters() {
    let offender = EXPERIMENTAL_NOTICE.chars().find(|c| c.is_control());
    assert!(
        offender.is_none(),
        "the notice must contain no control characters, found {offender:?} in {EXPERIMENTAL_NOTICE:?}"
    );
    assert!(
        !EXPERIMENTAL_NOTICE.contains('\u{1b}'),
        "the notice must not carry an ANSI escape: {EXPERIMENTAL_NOTICE:?}"
    );
}

/// The all-caps keyword is a cross-crate contract, not a stylistic choice.
///
/// `crates/tokfold-cli/tests/cli.rs` asserts that the `mcp` subcommand's stderr
/// contains `EXPERIMENTAL`, with that exact casing. This crate owns the string, so the
/// promise has to be pinned here too — otherwise the only way to discover the coupling
/// is to break another crate's test suite and work backwards.
#[test]
fn notice_shouts_the_experimental_keyword_in_caps() {
    assert!(
        EXPERIMENTAL_NOTICE.contains("EXPERIMENTAL"),
        "the notice must carry the literal all-caps keyword that the CLI test greps for: \
         {EXPERIMENTAL_NOTICE:?}"
    );
}

/// The single highest-stakes claim in the string.
///
/// The crate docs, the crate README and `main.rs` all justify shipping an unaudited
/// server that sits in the secrets path on the grounds that it announces itself and
/// loudly refuses secrets before it serves a single byte. If the sentence that actually
/// does the refusing is ever edited away, that justification evaporates while every
/// other test in the workspace stays green.
#[test]
fn notice_warns_against_production_secrets() {
    assert!(
        EXPERIMENTAL_NOTICE.contains("production secrets"),
        "the notice must name production secrets explicitly: {EXPERIMENTAL_NOTICE:?}"
    );
    assert!(
        EXPERIMENTAL_NOTICE.contains("Do not use"),
        "naming secrets is not enough — the notice must tell the user not to: \
         {EXPERIMENTAL_NOTICE:?}"
    );
}

/// The reversibility guarantee is the engine's headline promise, and this component is
/// explicitly outside it.
///
/// A user who has read the core crate's guarantees will reasonably assume they extend
/// to everything shipped under the same name. The notice is the only place that
/// contradicts that assumption at the point of use.
#[test]
fn notice_disclaims_the_reversibility_guarantees() {
    assert!(
        EXPERIMENTAL_NOTICE.contains("reversibility guarantees"),
        "the notice must mention the reversibility guarantees it is excluded from: \
         {EXPERIMENTAL_NOTICE:?}"
    );
    assert!(
        EXPERIMENTAL_NOTICE.contains("not covered"),
        "mentioning the guarantees is not enough — the notice must disclaim them: \
         {EXPERIMENTAL_NOTICE:?}"
    );
}

/// A warning has to identify what it is warning about.
///
/// The notice is printed with no surrounding context, and a user reading a scrollback
/// full of agent output needs to be able to attribute it. It also has to keep matching
/// the subcommand's real spelling if that is ever renamed.
#[test]
fn notice_names_the_subcommand_it_guards() {
    assert!(
        EXPERIMENTAL_NOTICE.contains("`mcp`"),
        "the notice must name the subcommand it guards: {EXPERIMENTAL_NOTICE:?}"
    );
}

/// The manifest description is the notice's twin for anyone who never runs the binary.
///
/// Publication of this crate is held deliberately, because the server sits in the
/// secrets path unaudited. Whenever that hold is lifted, the description is the first and often
/// only thing a registry visitor reads, so it has to carry the same refusal the runtime
/// notice does. Pinning it here means a manifest reword cannot quietly drop the warning
/// between now and publication.
#[test]
fn package_description_warns_that_the_crate_is_not_for_production() {
    let description = env!("CARGO_PKG_DESCRIPTION");
    assert!(
        description.contains("EXPERIMENTAL"),
        "the package description must open with the same all-caps warning the runtime \
         notice uses: {description:?}"
    );
    assert!(
        description.contains("not for production"),
        "the package description must refuse production use: {description:?}"
    );
}
