//! The confidence trust-signal carried by every edge (interface contract §1).
//!
//! This is the single most important value in the schema: it is the trust the whole graph rests on.
//! Its serialized form is **byte-compatible with graphify** (R2) — the exact strings `"EXTRACTED"`,
//! `"INFERRED"`, `"AMBIGUOUS"`.

use serde::{Deserialize, Serialize};

/// Classifies how a relationship (edge) was derived.
///
/// Ordering is meaningful: `Extracted < Inferred < Ambiguous`, i.e. ascending uncertainty, so the
/// least-trustworthy edges sort last and are easy to surface for review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Confidence {
    /// Explicit in the source — the relationship is stated, not guessed (highest trust).
    Extracted,
    /// Deduced from context — consistent with the code but not stated outright.
    Inferred,
    /// Uncertain — flagged for human/agent review.
    Ambiguous,
}

impl Confidence {
    /// Returns the canonical wire string (matches the serde representation and graphify).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extracted => "EXTRACTED",
            Self::Inferred => "INFERRED",
            Self::Ambiguous => "AMBIGUOUS",
        }
    }

    /// Returns `true` only for [`Confidence::Extracted`] — the trust floor for "load-bearing" edges.
    #[must_use]
    pub const fn is_trusted(self) -> bool {
        matches!(self, Self::Extracted)
    }
}

impl Default for Confidence {
    /// The conservative default is [`Confidence::Ambiguous`] — never claim trust by omission.
    fn default() -> Self {
        Self::Ambiguous
    }
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_matches_graphify_strings() {
        // R2: byte-compatible with graphify's confidence vocabulary.
        assert_eq!(
            serde_json::to_string(&Confidence::Extracted).unwrap(),
            "\"EXTRACTED\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::Inferred).unwrap(),
            "\"INFERRED\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::Ambiguous).unwrap(),
            "\"AMBIGUOUS\""
        );
    }

    #[test]
    fn deserialize_from_graphify_strings() {
        let c: Confidence = serde_json::from_str("\"INFERRED\"").unwrap();
        assert_eq!(c, Confidence::Inferred);
    }

    #[test]
    fn unknown_string_is_rejected() {
        assert!(serde_json::from_str::<Confidence>("\"MAYBE\"").is_err());
    }

    #[test]
    fn as_str_matches_serde() {
        for c in [
            Confidence::Extracted,
            Confidence::Inferred,
            Confidence::Ambiguous,
        ] {
            let serde = serde_json::to_string(&c).unwrap();
            assert_eq!(serde, format!("\"{}\"", c.as_str()));
        }
    }

    #[test]
    fn ordering_is_ascending_uncertainty() {
        assert!(Confidence::Extracted < Confidence::Inferred);
        assert!(Confidence::Inferred < Confidence::Ambiguous);
        let mut v = vec![
            Confidence::Ambiguous,
            Confidence::Extracted,
            Confidence::Inferred,
        ];
        v.sort_unstable();
        assert_eq!(
            v,
            vec![
                Confidence::Extracted,
                Confidence::Inferred,
                Confidence::Ambiguous
            ]
        );
    }

    #[test]
    fn only_extracted_is_trusted() {
        assert!(Confidence::Extracted.is_trusted());
        assert!(!Confidence::Inferred.is_trusted());
        assert!(!Confidence::Ambiguous.is_trusted());
    }

    #[test]
    fn default_is_conservative() {
        assert_eq!(Confidence::default(), Confidence::Ambiguous);
    }

    #[test]
    fn display_equals_as_str() {
        assert_eq!(Confidence::Extracted.to_string(), "EXTRACTED");
    }
}
