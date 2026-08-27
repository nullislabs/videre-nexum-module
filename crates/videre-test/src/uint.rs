//! Canonical `uint` conformance vectors: the JSON file publishing the
//! value-flow amount encoding, and the check holding a decoder to it.
//!
//! The vectors give the `types.wit` MUST teeth: the canonical form is
//! minimal big-endian with zero as the empty list (ADR 0001), and the
//! reject vectors fail any decoder that tolerates a non-minimal
//! encoding instead of rejecting it.

use std::fmt::{self, Display};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fixture::{self, FixtureError, FormatVersion, hex_bytes};
use crate::report::{ConformanceReport, Violation, settle};

/// The published canonical `uint` vector file, verbatim.
pub const UINT_VECTORS_JSON: &str = include_str!("../vectors/uint.json");

/// A published set of canonical `uint` vectors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UintVectors {
    /// File-format discriminator; an unknown version fails to parse.
    pub version: FormatVersion,
    /// The vectors, in publication order; never empty in a parsed file.
    #[serde(deserialize_with = "fixture::non_empty")]
    pub vectors: Vec<UintVector>,
}

/// One byte encoding and the outcome a conforming decoder produces.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UintVector {
    /// Stable name a violation is reported under.
    pub name: String,
    /// The encoding under test, lowercase hex in the file.
    #[serde(with = "hex_bytes")]
    pub bytes: Vec<u8>,
    /// What a conforming decoder does with the bytes.
    pub expect: UintExpectation,
    /// Optional prose for readers of the file; the check ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// The outcome a `uint` vector demands of a decoder.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UintExpectation {
    /// The bytes decode to this integer, decimal in the file.
    Value(String),
    /// The bytes are not the canonical encoding; decoding fails.
    Reject,
}

impl Display for UintExpectation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UintExpectation::Value(value) => write!(f, "value {value}"),
            UintExpectation::Reject => f.write_str("reject"),
        }
    }
}

impl UintVectors {
    /// Parse a vector set from its JSON text.
    ///
    /// # Errors
    ///
    /// Returns [`FixtureError::Format`] when the text is not a vector
    /// file of the published format version.
    pub fn from_json(json: &str) -> Result<Self, FixtureError> {
        fixture::from_json(json)
    }

    /// The canonical published form: pretty JSON, trailing newline.
    #[must_use]
    pub fn to_json(&self) -> String {
        fixture::to_json(self)
    }

    /// Load a vector file from disk.
    ///
    /// # Errors
    ///
    /// Returns [`FixtureError::Read`] when the file cannot be read, and
    /// [`FixtureError::Format`] when it does not parse.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, FixtureError> {
        fixture::load(path.as_ref())
    }

    /// Write the vector file in its canonical published form.
    ///
    /// # Errors
    ///
    /// Returns [`FixtureError::Write`] when the file cannot be written.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), FixtureError> {
        fixture::write(path.as_ref(), self)
    }

    /// Check a decoder against every vector, collecting all violations.
    ///
    /// A `value` vector must decode to the published integer (compared
    /// through the decimal `Display` form); a `reject` vector must fail
    /// to decode. An empty set is itself a violation.
    ///
    /// # Errors
    ///
    /// Returns the [`ConformanceReport`] naming every vector the decoder
    /// failed.
    pub fn check<V, E>(
        &self,
        decode: impl Fn(&[u8]) -> Result<V, E>,
    ) -> Result<(), ConformanceReport>
    where
        V: Display,
        E: Display,
    {
        let mut violations = Vec::new();
        if self.vectors.is_empty() {
            violations.push(Violation {
                vector: "<set>".to_owned(),
                detail: "published vector set is empty".to_owned(),
            });
        }
        for vector in &self.vectors {
            if let Err(detail) = vector.check(&decode) {
                violations.push(Violation {
                    vector: vector.name.clone(),
                    detail,
                });
            }
        }
        settle(violations)
    }

    /// [`check`](Self::check), panicking with the full report on any violation.
    ///
    /// # Panics
    ///
    /// Panics when the decoder fails any vector.
    pub fn assert_conforms<V, E>(&self, decode: impl Fn(&[u8]) -> Result<V, E>)
    where
        V: Display,
        E: Display,
    {
        if let Err(report) = self.check(decode) {
            panic!("uint decoder does not conform:\n{report}");
        }
    }
}

impl UintVector {
    /// Check one vector, returning the violation detail on divergence.
    fn check<V, E>(&self, decode: impl Fn(&[u8]) -> Result<V, E>) -> Result<(), String>
    where
        V: Display,
        E: Display,
    {
        match (&self.expect, decode(&self.bytes)) {
            (UintExpectation::Value(expected), Ok(value)) => {
                let decoded = value.to_string();
                if decoded == *expected {
                    Ok(())
                } else {
                    Err(format!("expected value {expected}, decoded {decoded}"))
                }
            }
            (UintExpectation::Value(expected), Err(err)) => {
                Err(format!("expected value {expected}, decode failed: {err}"))
            }
            (UintExpectation::Reject, Ok(value)) => {
                Err(format!("expected a rejection, decoded {value}"))
            }
            (UintExpectation::Reject, Err(_)) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use nexum_sdk::prelude::U256;
    use videre_sdk::value_flow::{decode_uint, encode_uint};

    use super::*;

    fn vector(name: &str, bytes: Vec<u8>, expect: UintExpectation, notes: &str) -> UintVector {
        UintVector {
            name: name.to_owned(),
            bytes,
            expect,
            notes: Some(notes.to_owned()),
        }
    }

    fn value(name: &str, integer: U256, notes: &str) -> UintVector {
        vector(
            name,
            encode_uint(integer),
            UintExpectation::Value(integer.to_string()),
            notes,
        )
    }

    /// Rebuild the published vectors from the shipped encoder and the
    /// hand-written reject encodings.
    fn build_uint_vectors() -> UintVectors {
        let mut overflow = vec![0x01];
        overflow.extend([0x00; 32]);
        UintVectors {
            version: FormatVersion,
            vectors: vec![
                value("zero", U256::ZERO, "zero is the empty list"),
                value("one", U256::from(1u64), "one byte, no padding"),
                value("byte-max", U256::from(255u64), "0xff alone is 255"),
                value(
                    "two-byte-boundary",
                    U256::from(256u64),
                    "256 is 0x0100: the minimal form keeps an interior zero",
                ),
                value("word-max", U256::MAX, "the largest EVM word: 32 0xff bytes"),
                vector(
                    "non-minimal-zero",
                    vec![0x00],
                    UintExpectation::Reject,
                    "zero is the empty list, never 0x00",
                ),
                vector(
                    "non-minimal-one",
                    vec![0x00, 0x01],
                    UintExpectation::Reject,
                    "same integer as 0x01; a conforming decoder rejects the \
                     padding instead of normalising it",
                ),
                vector(
                    "word-padded-zero",
                    vec![0x00; 32],
                    UintExpectation::Reject,
                    "the ABI word form of zero; the canonical form is the \
                     empty list",
                ),
                vector(
                    "word-padded-one",
                    {
                        let mut bytes = vec![0x00; 31];
                        bytes.push(0x01);
                        bytes
                    },
                    UintExpectation::Reject,
                    "the ABI word form of one; a decoder must not exempt the \
                     word width from the minimality rule",
                ),
                vector(
                    "overflow-33-bytes",
                    overflow,
                    UintExpectation::Reject,
                    "33 bytes cannot fit the 32-byte EVM word",
                ),
            ],
        }
    }

    /// A deliberately tolerant decode that normalises padding away: the
    /// non-conforming shape the reject vectors exist to catch.
    fn tolerant(bytes: &[u8]) -> Result<U256, &'static str> {
        if bytes.len() > 32 {
            return Err("too long");
        }
        Ok(U256::from_be_slice(bytes))
    }

    #[test]
    fn published_uint_vectors_match_regeneration() {
        assert_eq!(
            UINT_VECTORS_JSON,
            build_uint_vectors().to_json(),
            "vectors/uint.json has drifted; run the ignored \
             regenerate_uint_vectors test and commit the result",
        );
    }

    #[test]
    fn shipped_decode_conforms() {
        UintVectors::from_json(UINT_VECTORS_JSON)
            .unwrap()
            .assert_conforms(decode_uint);
    }

    #[test]
    fn tolerant_decode_fails_exactly_the_non_minimal_vectors() {
        let report = UintVectors::from_json(UINT_VECTORS_JSON)
            .unwrap()
            .check(tolerant)
            .unwrap_err();
        let failed: Vec<&str> = report
            .violations
            .iter()
            .map(|violation| violation.vector.as_str())
            .collect();
        assert_eq!(
            failed,
            [
                "non-minimal-zero",
                "non-minimal-one",
                "word-padded-zero",
                "word-padded-one",
            ],
        );
        for violation in &report.violations {
            assert!(
                violation.detail.contains("expected a rejection"),
                "detail: {violation}",
            );
        }
    }

    #[test]
    fn diverging_value_is_named() {
        let report = build_uint_vectors()
            .check(|_: &[u8]| Ok::<_, &str>(U256::from(7u64)))
            .unwrap_err();
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.vector == "zero"
                    && violation.detail.contains("expected value 0, decoded 7")),
            "report: {report}",
        );
    }

    #[test]
    #[should_panic(expected = "uint decoder does not conform")]
    fn assert_conforms_panics_with_the_report() {
        build_uint_vectors().assert_conforms(tolerant);
    }

    #[test]
    fn json_form_is_stable_and_round_trips() {
        let vectors = build_uint_vectors();
        let json = vectors.to_json();
        assert_eq!(UintVectors::from_json(&json).unwrap(), vectors);
        // The wire spellings are the contract for non-Rust readers.
        assert!(json.contains("\"version\": 1"));
        assert!(json.contains("\"value\": \"256\""));
        assert!(json.contains("\"reject\""));
        assert!(json.contains("\"bytes\": \"0100\""));
    }

    #[test]
    fn unknown_format_version_fails_closed() {
        let json = build_uint_vectors()
            .to_json()
            .replace("\"version\": 1", "\"version\": 2");
        assert!(matches!(
            UintVectors::from_json(&json),
            Err(FixtureError::Format(_)),
        ));
    }

    #[test]
    fn empty_vector_set_fails_the_check() {
        let empty = UintVectors {
            version: FormatVersion,
            vectors: Vec::new(),
        };
        let report = empty.check(decode_uint).unwrap_err();
        assert_eq!(report.violations.len(), 1, "violations: {report}");
        assert_eq!(report.violations[0].vector, "<set>");
    }

    #[test]
    fn empty_vector_set_fails_to_parse() {
        let json = UintVectors {
            version: FormatVersion,
            vectors: Vec::new(),
        }
        .to_json();
        let Err(FixtureError::Format(detail)) = UintVectors::from_json(&json) else {
            panic!("an empty set must not parse");
        };
        assert!(detail.contains("never empty"));
    }

    /// Rewrite the published file. Run with
    /// `cargo test -p videre-test -- --ignored regenerate_uint` after a
    /// deliberate change, then commit the diff.
    #[test]
    #[ignore = "writes the published fixture file in place"]
    fn regenerate_uint_vectors() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        build_uint_vectors()
            .write(root.join("vectors/uint.json"))
            .unwrap();
    }
}
