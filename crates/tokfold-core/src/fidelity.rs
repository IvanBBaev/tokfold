//! Per-segment fidelity reporting.

/// How faithfully a segment survives a compress/decompress cycle.
///
/// Fidelity is reported **per segment** so that a future lossy codec can coexist
/// with the lossless encoders inside one document. Version 0.0.1 emits only
/// [`Fidelity::Lossless`]; the [`Fidelity::Lossy`] variant exists so that neither
/// this API nor the crate name has to change when a lossy codec lands.
///
/// Note the deliberate wording: the engine is **reversible**, not "lossless" in the
/// marketing sense. The model reads the compressed rendering, not the
/// reconstruction, so byte-reversibility proves nothing about comprehension.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fidelity {
    /// The segment reconstructs to a semantically identical document, per the
    /// lexeme-preserving contract: key order, duplicate keys and number lexemes
    /// are exact; whitespace and escape style are canonicalized.
    Lossless,

    /// The segment was encoded by a codec that discards information.
    #[non_exhaustive]
    Lossy {
        /// Identifies which lossy codec produced the segment.
        codec_id: u16,
    },
}
