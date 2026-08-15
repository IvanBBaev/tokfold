//! Protocol-level constants for the Model Context Protocol.
//!
//! Everything here is a wire constant lifted from the published specification. It is
//! kept in one module, free of logic, so that a spec bump is a diff against a table
//! rather than a hunt through the dispatcher.
//!
//! # Eras
//!
//! Revision `2026-07-28` removed the `initialize` handshake and made the protocol
//! stateless: every request carries its protocol version and the client's capabilities
//! in [`meta`] fields, and [`method::SERVER_DISCOVER`] replaces the handshake as the way
//! a client learns what a server speaks. The specification calls those revisions
//! **modern**, and everything up to and including `2025-11-25` **legacy**.
//!
//! This server is **dual-era**: it answers `server/discover` and per-request `_meta`
//! for modern clients, and still answers `initialize` for legacy ones. The
//! specification permits exactly this, and it is the only shape that works today —
//! deployed clients are overwhelmingly still legacy.

/// Every protocol revision this server can speak, newest first.
///
/// Order is load-bearing: it is the preference order reported by
/// `server/discover` and used to pick a fallback when a legacy client asks for a
/// revision that is not on the list.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

/// The newest revision this server implements, and the one it prefers.
pub const LATEST_PROTOCOL_VERSION: &str = "2026-07-28";

/// The newest revision this server implements that still has an `initialize` handshake.
///
/// This is what a legacy handshake settles on when the client asks for a revision this
/// server does not implement. Answering with [`LATEST_PROTOCOL_VERSION`] would name a
/// revision in which `initialize` does not exist — a version the client cannot go on to
/// speak, since it has just proved it speaks the handshake.
pub const LATEST_LEGACY_VERSION: &str = "2025-11-25";

/// The oldest revision that is *modern* — stateless, with per-request `_meta`.
///
/// A revision string that sorts at or above this is served without a handshake.
/// The comparison is a plain lexicographic one on `YYYY-MM-DD`, which is correct
/// for as long as revisions are named by date; [`is_modern_version`] is the only
/// place that relies on it.
pub const FIRST_MODERN_VERSION: &str = "2026-07-28";

/// Whether a protocol revision uses the stateless, per-request-metadata model.
///
/// Unknown revision strings are treated as legacy. That is the safe direction: a
/// legacy answer to a modern client fails loudly at the client's version check,
/// whereas a modern answer to a legacy client looks like a malformed handshake.
#[must_use]
pub fn is_modern_version(version: &str) -> bool {
    version >= FIRST_MODERN_VERSION
}

/// Whether this server implements the named revision.
#[must_use]
pub fn is_supported_version(version: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
}

/// Reserved `_meta` keys carrying per-request protocol metadata.
///
/// The `io.modelcontextprotocol/` prefix is reserved by the specification; these are
/// the four keys a stdio server sees or sets.
pub mod meta {
    /// Protocol revision the request is written against. Required on modern requests.
    pub const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

    /// Client capabilities relevant to the request. Required on modern requests.
    pub const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";

    /// Client name and version. Optional and explicitly unverified by the
    /// specification; this server neither reads it nor reports it anywhere.
    pub const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";

    /// Server name and version, set on every result this server returns.
    pub const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
}

/// JSON-RPC and MCP method names.
///
/// Not every name here is dispatched on. The two notifications are dropped before
/// their method is ever compared — that is the rule, not an optimization — so they
/// appear as named constants a client or a test can refer to rather than as branches.
pub mod method {
    /// Modern discovery RPC. The specification requires every server to implement it.
    pub const SERVER_DISCOVER: &str = "server/discover";

    /// Legacy handshake opener. Its presence selects legacy semantics.
    pub const INITIALIZE: &str = "initialize";

    /// Legacy post-handshake notification. Carries no id, so it is dropped unanswered
    /// along with every other notification; the server keeps no state for it.
    pub const NOTIFICATIONS_INITIALIZED: &str = "notifications/initialized";

    /// Liveness probe, removed in `2026-07-28`. Answered in either era: a modern
    /// client never sends it, and refusing a legacy client's probe buys nothing.
    pub const PING: &str = "ping";

    /// Client abandoning an in-flight request. Carries no id, so it is dropped
    /// unanswered. This server handles one request at a time and has nothing to
    /// cancel by the time it could read one.
    pub const NOTIFICATIONS_CANCELLED: &str = "notifications/cancelled";

    /// Tool catalogue.
    pub const TOOLS_LIST: &str = "tools/list";

    /// Tool invocation.
    pub const TOOLS_CALL: &str = "tools/call";
}

/// JSON-RPC and MCP error codes.
///
/// The specification partitions the JSON-RPC implementation-defined range: `-32000`
/// to `-32019` is grandfathered and must not be used by new code, `-32020` to
/// `-32099` is reserved for the specification itself. This server therefore emits
/// only the five base JSON-RPC codes and the one specification code below.
pub mod error_code {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i32 = -32700;

    /// The payload was valid JSON but not a valid JSON-RPC request object.
    pub const INVALID_REQUEST: i32 = -32600;

    /// The method does not exist.
    pub const METHOD_NOT_FOUND: i32 = -32601;

    /// The method exists but the parameters are wrong — including a modern request
    /// that omits a required `_meta` field, and `tools/call` naming an unknown tool.
    pub const INVALID_PARAMS: i32 = -32602;

    /// An unexpected condition inside the server.
    pub const INTERNAL_ERROR: i32 = -32603;

    /// The request declared a protocol revision this server does not implement. The
    /// error's `data` carries `supported` and `requested`.
    pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;
}

/// `resultType` discriminator required on every modern result.
///
/// This server never performs a multi round-trip request, so it only ever emits
/// `"complete"`. Legacy clients ignore the field, which is why it is safe to set
/// unconditionally.
pub const RESULT_TYPE_COMPLETE: &str = "complete";

/// Freshness hint on cacheable results, in milliseconds.
///
/// The tool catalogue of this server is a compile-time constant, so the only reason
/// not to advertise an unbounded lifetime is that a client should notice a server
/// upgrade. One hour is the specification's own example value for a static server.
pub const CACHE_TTL_MS: u64 = 3_600_000;

/// Cache scope on cacheable results.
///
/// `public` is correct here and worth stating explicitly: the tool list is identical
/// for every caller — it carries no authorization-dependent or user-specific entries —
/// so a shared intermediary may cache one copy for everyone.
pub const CACHE_SCOPE_PUBLIC: &str = "public";

/// Server name reported in `serverInfo`.
pub const SERVER_NAME: &str = "tokfold";

/// Server version reported in `serverInfo`, taken from the crate manifest.
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::{
        FIRST_MODERN_VERSION, LATEST_LEGACY_VERSION, LATEST_PROTOCOL_VERSION,
        SUPPORTED_PROTOCOL_VERSIONS, is_modern_version, is_supported_version,
    };

    #[test]
    fn latest_version_is_the_head_of_the_supported_list() {
        assert_eq!(
            SUPPORTED_PROTOCOL_VERSIONS.first().copied(),
            Some(LATEST_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn supported_versions_are_ordered_newest_first() {
        // The list doubles as a preference order, so an out-of-order entry would
        // silently make the server prefer an older revision.
        let mut sorted = SUPPORTED_PROTOCOL_VERSIONS.to_vec();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(sorted, SUPPORTED_PROTOCOL_VERSIONS);
    }

    #[test]
    fn every_supported_version_is_recognized() {
        for version in SUPPORTED_PROTOCOL_VERSIONS {
            assert!(is_supported_version(version), "{version} not recognized");
        }
    }

    #[test]
    fn unknown_versions_are_neither_supported_nor_modern() {
        assert!(!is_supported_version("1900-01-01"));
        assert!(!is_modern_version("1900-01-01"));
    }

    #[test]
    fn the_era_boundary_splits_the_supported_list_correctly() {
        assert!(is_modern_version(FIRST_MODERN_VERSION));
        assert!(is_modern_version(LATEST_PROTOCOL_VERSION));
        // Everything the specification calls legacy must land on the legacy side.
        for version in ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"] {
            assert!(
                !is_modern_version(version),
                "{version} classified as modern"
            );
        }
    }

    #[test]
    fn the_legacy_head_is_the_newest_revision_that_still_has_a_handshake() {
        assert!(is_supported_version(LATEST_LEGACY_VERSION));
        assert!(!is_modern_version(LATEST_LEGACY_VERSION));
        // Derived rather than asserted by hand: whatever the table says, the constant
        // has to be its first non-modern entry, or the handshake fallback would offer
        // a client something older than it could have had.
        assert_eq!(
            SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .find(|version| !is_modern_version(version))
                .copied(),
            Some(LATEST_LEGACY_VERSION)
        );
    }

    #[test]
    fn a_future_dated_revision_is_treated_as_modern() {
        // Date ordering is the whole rule; a revision newer than ours is modern even
        // though `is_supported_version` will (correctly) still reject it.
        assert!(is_modern_version("2027-01-01"));
        assert!(!is_supported_version("2027-01-01"));
    }
}
