//! Install-time body-version handshake over the `[venue]` manifest
//! section: a keeper declares the one version it encodes, an adapter the
//! set it decodes, and a keeper boots only when every installed adapter
//! decodes its version.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::anyhow;
use nexum_runtime::manifest::ExtensionSections;
use serde::Deserialize;
use tracing::error;

use crate::registry::VenueId;

/// The manifest section the videre platform claims.
pub(crate) const SECTION: &str = "venue";

/// The claimed-section list handed to the runtime.
pub(crate) const SECTIONS: &[&str] = &[SECTION];

/// Keeper-side `[venue]`: the one body-schema version it encodes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeeperSection {
    body_version: u32,
}

/// Parse one `[venue]` value as `S`, tagging failures with the owner.
fn parse<S: for<'de> Deserialize<'de>>(owner: &str, value: &toml::Value) -> anyhow::Result<S> {
    value
        .clone()
        .try_into()
        .map_err(|e| anyhow!("{owner} [venue]: {e}"))
}

/// Membership predicate: a worker declaring `[venue] body_version` is
/// admitted only when every registered venue's `body_versions` contains
/// it, so one non-decoding venue refuses the keeper. An absent section is
/// admitted (opt-out).
pub(crate) fn admit_worker(
    worker: &str,
    sections: &ExtensionSections,
    registered: &BTreeMap<VenueId, BTreeSet<u32>>,
) -> anyhow::Result<()> {
    let Some(value) = sections.get(SECTION) else {
        return Ok(());
    };
    let KeeperSection { body_version } = parse(worker, value)?;
    // A venue registering an empty set has opted out of the handshake; it
    // neither satisfies nor refuses a keeper.
    let declaring: Vec<(&VenueId, &BTreeSet<u32>)> = registered
        .iter()
        .filter(|(_, versions)| !versions.is_empty())
        .collect();
    if declaring.is_empty() {
        return Err(refuse(
            worker,
            body_version,
            "no registered venue declares [venue] body_versions",
        ));
    }
    for (venue, versions) in declaring {
        if !versions.contains(&body_version) {
            return Err(refuse(
                worker,
                body_version,
                &format!("{venue} decodes {versions:?}"),
            ));
        }
    }
    Ok(())
}

/// Log and build the refusal for one keeper/adapter pairing.
fn refuse(worker: &str, body_version: u32, decoded: &str) -> anyhow::Error {
    error!(
        keeper = %worker,
        body_version,
        %decoded,
        "body-version handshake refused the keeper/adapter pair",
    );
    anyhow!("keeper {worker} encodes body version {body_version}; {decoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections(toml: &str) -> ExtensionSections {
        let table: toml::Table = toml.parse().expect("parse");
        table.into_iter().collect()
    }

    /// The registered-venue table the predicate reads, from `(id, versions)`
    /// pairs.
    fn registered(
        venues: impl IntoIterator<Item = (&'static str, &'static [u32])>,
    ) -> BTreeMap<VenueId, BTreeSet<u32>> {
        venues
            .into_iter()
            .map(|(id, versions)| {
                (
                    VenueId::new(id).expect("valid venue id"),
                    versions.iter().copied().collect(),
                )
            })
            .collect()
    }

    /// A keeper whose version every registered venue decodes is admitted.
    #[test]
    fn matching_pair_is_admitted() {
        let keeper = sections("[venue]\nbody_version = 2");
        let venues = registered([("cow", &[1, 2][..]), ("uni", &[2, 3][..])]);
        admit_worker("keeper", &keeper, &venues).expect("admitted");
    }

    /// A keeper whose version no venue decodes is refused, and the refusal
    /// names the version and the declared set.
    #[test]
    fn mismatched_pair_is_refused() {
        let keeper = sections("[venue]\nbody_version = 2");
        let venues = registered([("cow", &[1][..])]);
        let err = admit_worker("keeper", &keeper, &venues).expect_err("refused");
        let msg = err.to_string();
        assert!(msg.contains("body version 2"), "{msg}");
        assert!(msg.contains("cow decodes {1}"), "{msg}");
    }

    /// One non-decoding venue refuses the keeper even when another decodes
    /// its version.
    #[test]
    fn one_non_decoding_venue_refuses_the_keeper() {
        let keeper = sections("[venue]\nbody_version = 2");
        let venues = registered([("cow", &[1, 2][..]), ("uni", &[1][..])]);
        let err = admit_worker("keeper", &keeper, &venues).expect_err("refused");
        let msg = err.to_string();
        assert!(msg.contains("body version 2"), "{msg}");
        assert!(msg.contains("uni decodes {1}"), "{msg}");
    }

    /// A declaring keeper with no registered venue at all is refused.
    #[test]
    fn no_registered_venue_refuses_a_declaring_keeper() {
        let keeper = sections("[venue]\nbody_version = 1");
        let err = admit_worker("keeper", &keeper, &BTreeMap::new()).expect_err("refused");
        assert!(
            err.to_string().contains("no registered venue declares"),
            "{err}",
        );
    }

    /// A venue registering an empty set has opted out: it cannot satisfy a
    /// declaring keeper on its own.
    #[test]
    fn an_opted_out_venue_never_satisfies_a_declaring_keeper() {
        let keeper = sections("[venue]\nbody_version = 1");
        let venues = registered([("cow", &[][..])]);
        let err = admit_worker("keeper", &keeper, &venues).expect_err("refused");
        assert!(
            err.to_string().contains("no registered venue declares"),
            "{err}",
        );
    }

    /// An opted-out venue also refuses nothing: a declaring sibling alone
    /// decides the keeper.
    #[test]
    fn an_opted_out_venue_never_refuses_a_keeper() {
        let keeper = sections("[venue]\nbody_version = 2");
        let venues = registered([("cow", &[][..]), ("uni", &[2][..])]);
        admit_worker("keeper", &keeper, &venues).expect("the declaring venue decides");
    }

    /// A worker without a `[venue]` section is admitted.
    #[test]
    fn undeclared_sections_are_admitted() {
        admit_worker("keeper", &ExtensionSections::new(), &BTreeMap::new())
            .expect("worker admitted");
    }

    /// The venue-side spelling on the keeper face fails loudly.
    #[test]
    fn wrong_side_spelling_is_refused() {
        let keeper = sections("[venue]\nbody_versions = [1]");
        let err = admit_worker("keeper", &keeper, &BTreeMap::new())
            .expect_err("keeper with the venue key");
        assert!(err.to_string().contains("keeper [venue]"), "{err}");
    }
}
