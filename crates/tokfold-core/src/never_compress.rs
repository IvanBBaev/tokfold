//! Versioned, data-driven list of lines that must survive compression verbatim.
//!
//! Some lines carry more value than their token cost: a compiler error, an HTTP
//! `4xx`/`5xx` status, an `EACCES`, a panic trace, a `denied`/`unauthorized`
//! message, a certificate failure. Folding, deduplicating or reordering these
//! degrades exactly the output an agent most needs to read back accurately. This
//! module names those lines so the encoders can leave them untouched.
//!
//! # Why literal-only, no regex
//!
//! Matching is a **case-folded literal substring** test, never a regular
//! expression. Regex over attacker-influenced input is a denial-of-service
//! surface: a pathological pattern/input pair can cost super-linear time
//! (catastrophic backtracking). Every rule here is a fixed literal, so a match is
//! a bounded, linear scan of the line and the crate takes no regex dependency.
//!
//! Case folding is **ASCII-only**, which is sufficient because every literal is
//! ASCII: a UTF-8 continuation byte (`>= 0x80`) can never equal an ASCII byte
//! under ASCII case folding, so folding cannot produce a spurious match inside a
//! multi-byte character.
//!
//! # The verbatim-with-position contract
//!
//! The intent: content this table protects should be reproduced **byte-for-byte at
//! its original position** relative to the surrounding content — not folded into a
//! legend, not deduped against a merely-similar line, not moved. This module decides
//! only *membership*. It does not enforce anything, and the enforcement is not
//! uniform across encoders, so the rule is stated here as intent and the implemented
//! behaviour is spelled out immediately below.
//!
//! ## What v0.0.1 actually implements
//!
//! * **E1 (minify) is the only encoder that consults this table.** It calls
//!   [`is_protected`] on each JSON *string lexeme* — quotes and escapes included, so
//!   the unit is a lexeme, not a line — and on a hit copies that lexeme byte-for-byte
//!   rather than canonicalizing its escapes. E1 never folds, dedups or reorders, so
//!   position is preserved for everything it emits. One known gap: the match runs on
//!   the *escaped* spelling, so a protected phrase written with `\u` escapes misses
//!   the carve-out (documented on E1's `emit_string`).
//! * **E2 (tabular) never reads this table at all.** Its faithfulness is structural:
//!   every value lexeme is copied verbatim from its source span and rows keep source
//!   order. But E2 hoists an array's dominant key set once into the table header, so a
//!   protected string occurring among those hoisted *keys* is written once, at the
//!   header, instead of once per element. (Rows whose shape deviates from the header
//!   are self-describing and still spell out their own keys.)
//! * **Passthrough** re-emits the input verbatim, and the recovery archive stores the
//!   original bytes whichever encoder shaped the rendering — so reconstruction is
//!   exact regardless, and what this contract governs is only the *rendering* the
//!   model reads.
//!
//! # Related integrity rules (kept alongside membership)
//!
//! * **Dedup only on byte-identical lines.** Repeated lines may be collapsed only
//!   when they are byte-for-byte identical. There is no near-duplicate clustering
//!   in v0.0.1, so a protected error line can never be merged into a line that is
//!   only similar to it.
//! * **No content-adaptive windows.** Whether a line is kept must never depend on
//!   the *content* of its neighbours. Membership is decided per line in isolation;
//!   content must not open or close a "keep window" over adjacent lines.
//!
//! # Open question: may byte-identical protected content be collapsed?
//!
//! The two rules stated above disagree, and this version does not decide between
//! them. Read strictly, *verbatim-with-position* forbids collapsing a protected line
//! at all; *dedup only on byte-identical lines* permits collapsing it precisely when
//! the copies are byte-for-byte identical. Both are recorded here on purpose; neither
//! is dropped.
//!
//! Nothing in v0.0.1 forces the question at the level it is written — no encoder
//! collapses repeated *lines*. But E2's key hoisting is the same shape of transform
//! applied to a repeated protected *key*, and it ships, so the ambiguity is not
//! academic.
//!
//! Resolving it is a normative, format-affecting decision reserved for the repo
//! owner, because either answer changes emitted bytes:
//!
//! * **Position wins** — collapsing protected content is forbidden even when the
//!   copies are identical, so E2 must repeat a protected key on every row and give up
//!   the saving on that key.
//! * **Byte-identity wins** — collapsing byte-identical protected content is allowed,
//!   E2's current behaviour is already correct, and the position rule narrows to "not
//!   moved or merged relative to content that is not byte-identical to it".
//!
//! Until it is decided, treat the position rule as the stated *intent* and the
//! "What v0.0.1 actually implements" section as the description of behaviour.
//!
//! # Non-claim
//!
//! This is a fidelity safeguard, not a security control. It preserves error and
//! security lines; it does **not** inspect, sanitize or authorize anything. The
//! crate is **not** a prompt-injection filter and must not be marketed as reducing
//! injection risk: a hostile line that matches a rule is preserved verbatim, not
//! neutralized.

/// Version of the rule table.
///
/// Bumping it is a **deliberate, reviewed act**: the list ships as data, and
/// changing which lines are protected changes compressor output. Downstream
/// prompt-cache stability depends on this staying fixed within a format version,
/// so the value is asserted in tests to catch an accidental edit.
pub const LIST_VERSION: u32 = 1;

/// One entry in the never-compress table.
///
/// Rules are grouped by [`class`](Self::class) so the list is inspectable and
/// testable by category. A line matches when [`literal`](Self::literal) occurs as
/// a substring, compared ASCII-case-insensitively unless
/// [`case_sensitive`](Self::case_sensitive) is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct NeverCompressRule {
    /// Semantic category, e.g. `"compiler"` or `"certificate"`. Groups the table
    /// for auditing and per-class tests; carries no matching behaviour.
    pub class: &'static str,
    /// The fixed substring to search for. Always non-empty (asserted in tests):
    /// an empty literal would match every line and disable compression entirely.
    pub literal: &'static str,
    /// When `true`, match byte-for-byte; when `false`, fold ASCII case. Symbolic
    /// constants such as `EACCES` are case-sensitive; human-readable phrases are
    /// not.
    pub case_sensitive: bool,
}

impl NeverCompressRule {
    /// Whether this rule's literal occurs in `line` under the rule's case policy.
    fn matches_line(&self, line: &str) -> bool {
        if self.case_sensitive {
            line.contains(self.literal)
        } else {
            contains_ascii_case_insensitive(line, self.literal)
        }
    }
}

/// The frozen rule table (`LIST_VERSION` 1).
///
/// Ordered by class. Matching walks the table in this order and returns the first
/// hit, so the order is the deterministic tie-break when a line matches more than
/// one rule (for example an HTTP status line that also contains `Unauthorized`).
/// Over-matching is deliberately preferred to under-matching: an extra protected
/// line only lowers the compression ratio, whereas a missed error line degrades
/// fidelity.
static RULES: &[NeverCompressRule] = &[
    // --- compiler / build errors ---
    NeverCompressRule {
        class: "compiler",
        literal: "error[",
        case_sensitive: false,
    }, // Rust: error[E0308]
    NeverCompressRule {
        class: "compiler",
        literal: "error:",
        case_sensitive: false,
    }, // clang/gcc/generic
    NeverCompressRule {
        class: "compiler",
        literal: "error TS",
        case_sensitive: false,
    }, // TypeScript: error TS2322
    // --- HTTP 4xx/5xx status lines (canonical reason phrases, RFC 9110) ---
    NeverCompressRule {
        class: "http-status",
        literal: "400 Bad Request",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "401 Unauthorized",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "403 Forbidden",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "404 Not Found",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "405 Method Not Allowed",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "408 Request Timeout",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "409 Conflict",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "410 Gone",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "422 Unprocessable",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "429 Too Many Requests",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "500 Internal Server Error",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "501 Not Implemented",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "502 Bad Gateway",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "503 Service Unavailable",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "http-status",
        literal: "504 Gateway Timeout",
        case_sensitive: false,
    },
    // --- errno symbols in the access/permission family (case-sensitive constants) ---
    NeverCompressRule {
        class: "errno",
        literal: "EACCES",
        case_sensitive: true,
    },
    NeverCompressRule {
        class: "errno",
        literal: "EPERM",
        case_sensitive: true,
    },
    NeverCompressRule {
        class: "errno",
        literal: "EROFS",
        case_sensitive: true,
    },
    // --- panic traces ---
    NeverCompressRule {
        class: "panic",
        literal: "panicked at",
        case_sensitive: false,
    }, // Rust
    NeverCompressRule {
        class: "panic",
        literal: "panic:",
        case_sensitive: false,
    }, // Go
    // --- stack traces / backtraces ---
    NeverCompressRule {
        class: "stack-trace",
        literal: "Traceback (most recent call last)",
        case_sensitive: false,
    }, // Python
    NeverCompressRule {
        class: "stack-trace",
        literal: "stack backtrace:",
        case_sensitive: false,
    }, // Rust
    NeverCompressRule {
        class: "stack-trace",
        literal: "goroutine ",
        case_sensitive: false,
    }, // Go stack dump
    // --- denial ---
    NeverCompressRule {
        class: "denial",
        literal: "denied",
        case_sensitive: false,
    },
    // --- authorization ---
    NeverCompressRule {
        class: "authz",
        literal: "unauthorized",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "authz",
        literal: "forbidden",
        case_sensitive: false,
    },
    // --- certificate / TLS failures ---
    NeverCompressRule {
        class: "certificate",
        literal: "certificate verify failed",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "certificate",
        literal: "certificate verification failed",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "certificate",
        literal: "certificate has expired",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "certificate",
        literal: "certificate is not valid",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "certificate",
        literal: "self-signed certificate",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "certificate",
        literal: "self signed certificate",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "certificate",
        literal: "unable to get local issuer certificate",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "certificate",
        literal: "SSL certificate problem",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "certificate",
        literal: "x509: certificate",
        case_sensitive: false,
    },
    NeverCompressRule {
        class: "certificate",
        literal: "tls: bad certificate",
        case_sensitive: false,
    },
    // --- warnings ---
    NeverCompressRule {
        class: "warning",
        literal: "warning:",
        case_sensitive: false,
    },
];

/// The full rule table, for auditing and grouping by [`class`](NeverCompressRule::class).
pub fn rules() -> &'static [NeverCompressRule] {
    RULES
}

/// The rule protecting `line`, or `None` if the line is compressible.
///
/// Callers pass a single line (no trailing newline required). On a match the line
/// must be preserved verbatim with its position; see the module contract. When a
/// line matches several rules the first in table order wins, so the result is
/// deterministic.
pub fn is_protected(line: &str) -> Option<&'static NeverCompressRule> {
    RULES.iter().find(|rule| rule.matches_line(line))
}

/// ASCII-case-insensitive substring test.
///
/// Returns whether `needle` occurs in `haystack`, folding only ASCII letters.
/// Uses `.get(..)` rather than slice indexing so it cannot panic on any input.
fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    let hay = haystack.as_bytes();
    let need = needle.as_bytes();
    if need.is_empty() {
        return true;
    }
    if need.len() > hay.len() {
        return false;
    }
    let last_start = hay.len() - need.len();
    for start in 0..=last_start {
        if let Some(window) = hay.get(start..start + need.len()) {
            if window
                .iter()
                .zip(need.iter())
                .all(|(h, n)| h.eq_ignore_ascii_case(n))
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{
        LIST_VERSION, NeverCompressRule, contains_ascii_case_insensitive, is_protected, rules,
    };
    use crate::encoder::{self, Encoder};
    use crate::tape;

    /// The E1 rendering of `input`, sentinel line included.
    fn minified(input: &str) -> String {
        let parsed = tape::parse(input, 512).unwrap();
        encoder::render(Encoder::E1Minify, &parsed, input).unwrap()
    }

    /// One realistic line per semantic class must be protected, and it must land
    /// in the expected class (guards both membership and table ordering).
    #[test]
    fn each_class_matches_a_realistic_line() {
        let cases: &[(&str, &str)] = &[
            ("error[E0308]: mismatched types", "compiler"),
            ("HTTP/1.1 404 Not Found", "http-status"),
            ("open failed: EACCES", "errno"),
            ("thread 'main' panicked at src/main.rs:10:5", "panic"),
            ("Traceback (most recent call last):", "stack-trace"),
            ("bind: Permission denied", "denial"),
            ("access token unauthorized for this scope", "authz"),
            ("SSL certificate problem: unable to verify", "certificate"),
            ("warning: unused variable `x`", "warning"),
        ];
        for (line, expected_class) in cases {
            let hit = is_protected(line);
            assert!(hit.is_some(), "expected {line:?} to be protected");
            assert_eq!(
                hit.map(|r| r.class),
                Some(*expected_class),
                "wrong class for {line:?}"
            );
        }
    }

    /// Case-insensitive rules match regardless of case; the substring helper folds
    /// ASCII in both directions and respects bounds.
    #[test]
    fn case_folding_matches_regardless_of_case() {
        assert!(is_protected("ACCESS DENIED").is_some());
        assert!(is_protected("Access Denied").is_some());
        assert!(is_protected("access denied").is_some());

        assert!(contains_ascii_case_insensitive("HELLO WORLD", "hello"));
        assert!(contains_ascii_case_insensitive("hello", "HELLO"));
        assert!(!contains_ascii_case_insensitive("abc", "abcd"));
        assert!(!contains_ascii_case_insensitive("", "x"));
        // A UTF-8 continuation byte must not fold into an ASCII letter.
        assert!(!contains_ascii_case_insensitive("é", "e"));
    }

    /// Window boundaries of the substring scan: the needle must be found in the
    /// first window, in the last window, and when it spans the whole haystack; a
    /// needle longer than the haystack is rejected before the scan starts.
    ///
    /// The *width* of the scan range is deliberately not asserted. Windows are taken
    /// with `.get(..)`, which yields `None` past the end of the haystack, so
    /// scanning beyond the last legal start costs time and changes no answer. That
    /// makes widening the bound an equivalent mutation, and no honest test can kill
    /// it.
    #[test]
    fn substring_scan_covers_both_window_boundaries() {
        assert!(contains_ascii_case_insensitive("abcdef", "ABC")); // first window
        assert!(contains_ascii_case_insensitive("abcdef", "DEF")); // last window
        assert!(contains_ascii_case_insensitive("abc", "ABC")); // whole haystack
        assert!(!contains_ascii_case_insensitive("abc", "abcd")); // needle too long
        assert!(!contains_ascii_case_insensitive("abcdef", "efg")); // runs off the end
        assert!(contains_ascii_case_insensitive("", "")); // empty needle
        assert!(contains_ascii_case_insensitive("abc", ""));
    }

    /// Case-sensitive rules do not fold: `EACCES` is protected, `eacces` is not
    /// (and the sample line carries no other trigger).
    #[test]
    fn case_sensitive_rule_does_not_fold() {
        assert!(is_protected("open failed: EACCES").is_some());
        assert!(is_protected("open failed: eacces path").is_none());
    }

    /// A benign line matches nothing.
    #[test]
    fn benign_line_is_not_protected() {
        assert!(is_protected("the quick brown fox jumps over the lazy dog").is_none());
        assert!(is_protected("{\"user\":\"alice\",\"count\":42}").is_none());
        assert!(is_protected("").is_none());
    }

    /// When a line matches several rules, the first in table order wins.
    #[test]
    fn first_rule_in_table_order_wins() {
        // Contains "401 Unauthorized" (http-status, earlier) and "Unauthorized"
        // (authz, later). The earlier class must be returned.
        assert_eq!(
            is_protected("HTTP/1.1 401 Unauthorized").map(|r| r.class),
            Some("http-status")
        );
    }

    /// Matching is deterministic: same input yields the same rule instance.
    #[test]
    fn matching_is_deterministic() {
        let line = "bind: Permission denied";
        let a = is_protected(line);
        let b = is_protected(line);
        assert_eq!(a, b);
        match (a, b) {
            (Some(ra), Some(rb)) => assert!(std::ptr::eq(ra, rb)),
            _ => panic!("expected {line:?} to match on both calls"),
        }
    }

    /// `LIST_VERSION` is pinned; a bump must be a conscious edit to this test.
    #[test]
    fn list_version_is_pinned() {
        assert_eq!(LIST_VERSION, 1);
    }

    /// No rule may carry an empty literal or class.
    #[test]
    fn no_rule_has_empty_fields() {
        for rule in rules() {
            let NeverCompressRule { class, literal, .. } = rule;
            assert!(!literal.is_empty(), "empty literal in class {class:?}");
            assert!(!class.is_empty(), "empty class for literal {literal:?}");
        }
    }

    /// `rules()` must hand out the populated table, not an empty or detached slice.
    ///
    /// Every other test here reaches the table through `is_protected`, which reads
    /// the static directly; the accessor is the only view auditors and downstream
    /// tooling get, so its contents are pinned on their own. An empty table would
    /// leave matching intact and silently hollow out every audit built on it.
    #[test]
    fn rules_exposes_the_populated_table() {
        let table = rules();
        assert!(!table.is_empty(), "the rule table must not be empty");

        // Size per class. A bump here is a table edit and must be deliberate.
        let expected_counts: &[(&str, usize)] = &[
            ("compiler", 3),
            ("http-status", 15),
            ("errno", 3),
            ("panic", 2),
            ("stack-trace", 3),
            ("denial", 1),
            ("authz", 2),
            ("certificate", 10),
            ("warning", 1),
        ];
        for (class, count) in expected_counts {
            assert_eq!(
                table.iter().filter(|rule| rule.class == *class).count(),
                *count,
                "rule count changed for class {class:?}"
            );
        }
        assert_eq!(
            table.len(),
            expected_counts.iter().map(|&(_, n)| n).sum::<usize>(),
            "the table holds rules in a class this test does not know about"
        );

        // Each class occupies one contiguous run, in this order: the order is the
        // documented tie-break when a line matches more than one rule.
        let mut class_runs: Vec<&str> = table.iter().map(|rule| rule.class).collect();
        class_runs.dedup();
        assert_eq!(
            class_runs,
            [
                "compiler",
                "http-status",
                "errno",
                "panic",
                "stack-trace",
                "denial",
                "authz",
                "certificate",
                "warning",
            ]
        );

        // A representative literal from every class must actually be present.
        for literal in [
            "error[",
            "404 Not Found",
            "EACCES",
            "panicked at",
            "stack backtrace:",
            "denied",
            "unauthorized",
            "x509: certificate",
            "warning:",
        ] {
            assert!(
                table.iter().any(|rule| rule.literal == literal),
                "literal {literal:?} is missing from the table"
            );
        }

        // Only the errno symbols are matched case-sensitively.
        for rule in table {
            assert_eq!(
                rule.case_sensitive,
                rule.class == "errno",
                "case policy changed for literal {:?}",
                rule.literal
            );
        }
    }

    /// Every literal in the table is reachable through `is_protected`, and every
    /// rule `is_protected` returns is an element of the table `rules()` exposes.
    ///
    /// This is the tie between the accessor and the matcher: they must be the same
    /// data, so an audit over `rules()` describes what actually gets protected.
    #[test]
    fn every_exposed_literal_is_matched_by_the_same_table() {
        let table = rules();
        assert!(!table.is_empty(), "the rule table must not be empty");
        for rule in table {
            let hit = is_protected(rule.literal);
            assert!(
                hit.is_some(),
                "literal {:?} is in the table but matches nothing",
                rule.literal
            );
            let found = hit.unwrap();
            assert!(
                table.iter().any(|candidate| std::ptr::eq(candidate, found)),
                "matching {:?} returned a rule outside the exposed table",
                rule.literal
            );
        }
    }

    /// End-to-end: the safeguard decides which bytes E1 emits.
    ///
    /// A protected string lexeme is copied byte-for-byte, so a redundant `\/`
    /// escape survives it; the same lexeme without a protected literal is
    /// canonicalized and the escape is unwound. Drop the carve-out and the two
    /// renderings are spelled the same way.
    #[test]
    fn protected_lexeme_is_copied_verbatim_by_e1() {
        let protected = "{\"m\":\"error[E0308] a\\/b\"}";
        let plain = "{\"m\":\"ok a\\/b\"}";

        // Precondition: matching is applied to the raw lexeme, quotes included.
        assert!(is_protected("\"error[E0308] a\\/b\"").is_some());
        assert!(is_protected("\"ok a\\/b\"").is_none());

        let protected_out = minified(protected);
        assert!(
            protected_out.ends_with("{\"m\":\"error[E0308] a\\/b\"}"),
            "protected value was rewritten: {protected_out}"
        );

        let plain_out = minified(plain);
        assert!(
            plain_out.ends_with("{\"m\":\"ok a/b\"}"),
            "unprotected value was not canonicalized: {plain_out}"
        );
    }
}
