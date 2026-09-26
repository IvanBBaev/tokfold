//! Fidelity reporting.

/// How faithfully a document survives a compress/decompress cycle.
///
/// Reported **once per pass**: [`Stats`](crate::Stats) carries a single `fidelity`
/// for the whole artifact. There are no segments in the v0.0.1 compression path, so
/// there is nothing finer to report — the segmented layout that would let a lossy
/// codec coexist with the lossless encoders inside one document is reserved in the
/// *container format*, as [`Flags::truncation_tolerated`](crate::format::Flags::truncation_tolerated)
/// and [`Flags::segment_lossy`](crate::format::Flags::segment_lossy), and neither
/// bit is set by anything this version writes.
///
/// Version 0.0.1 emits only [`Fidelity::Lossless`]; the [`Fidelity::Lossy`] variant
/// exists so that neither this API nor the crate name has to change when a lossy
/// codec lands. Its *granularity* would have to change: one value per document
/// cannot express a document some of whose parts round-trip exactly and some of
/// which do not, so the day a lossy codec ships, this report grows a segment with
/// it.
///
/// Note the deliberate wording: the engine is **reversible**, not "lossless" in the
/// marketing sense. The model reads the compressed rendering, not the
/// reconstruction, so byte-reversibility proves nothing about comprehension.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fidelity {
    /// The document reconstructs to a semantically identical value, per the
    /// lexeme-preserving contract: key order, duplicate keys and number lexemes
    /// are exact; whitespace and escape style may be canonicalized. That is the
    /// floor, not what this version does: every archive it writes holds the
    /// original bytes verbatim, so the reconstruction is byte-exact today (see the
    /// crate-level docs), and a later encoder may keep this variant while giving
    /// that up.
    Lossless,

    /// At least one part of the document was encoded by a codec that discards
    /// information. Unreachable in v0.0.1: no shipped encoder is lossy.
    #[non_exhaustive]
    Lossy {
        /// Identifies which lossy codec produced the reconstruction.
        codec_id: u16,
    },
}
