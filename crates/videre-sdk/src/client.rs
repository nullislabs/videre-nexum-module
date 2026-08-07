//! The typed venue client: [`VenueClient`] binds one [`Venue`] over the
//! byte-level [`VenueTransport`] seam, encoding each call through
//! [`IntentBody`] so keeper code never handles wire bytes. [`HostVenues`]
//! binds the seam to the module's `videre:venue/client` import; tests
//! implement [`VenueTransport`] directly. Transport methods are native
//! AFIT, so dispatch is static.

use std::borrow::Cow;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use nexum_sdk::keeper::RetryAction;
use strum::IntoStaticStr;

use crate::bindings::videre::venue::client as shims;
use crate::{BodyError, IntentBody, IntentStatus, Quotation, SubmitOutcome, VenueFault};

/// Venue identifier: the id an adapter registers under and every client
/// call routes to. Opaque beyond equality and never empty or
/// whitespace-padded: every constructor validates, at const evaluation
/// for [`VenueId::from_static`].
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VenueId(Cow<'static, str>);

/// A candidate venue id failed validation at a boundary.
#[derive(Debug, thiserror::Error)]
#[error("venue id must not be empty or whitespace-padded (got {0:?})")]
pub struct InvalidVenueId(String);

impl VenueId {
    /// Wrap a static id without allocating. An empty or padded id
    /// panics: at const evaluation where the call site is a const, so a
    /// [`Venue::ID`] never compiles, and at the call otherwise.
    #[must_use]
    pub const fn from_static(id: &'static str) -> Self {
        assert!(
            !padded(id),
            "venue id must not be empty or whitespace-padded"
        );
        Self(Cow::Borrowed(id))
    }

    /// Validating constructor: rejects empty input and surrounding
    /// whitespace. Padded ids are rejected, never trimmed: a trim would
    /// collapse two spellings into one id.
    pub fn new(id: impl Into<Cow<'static, str>>) -> Result<Self, InvalidVenueId> {
        let id = id.into();
        if padded(&id) {
            return Err(InvalidVenueId(id.into_owned()));
        }
        Ok(Self(id))
    }

    /// The id at its wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// True when `id` is empty or opens or closes on Unicode whitespace.
/// Const for [`VenueId::from_static`], so the boundary chars are decoded
/// by hand: `str::chars` is not const.
const fn padded(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() {
        return true;
    }
    let mut last = bytes.len() - 1;
    while bytes[last] & 0xC0 == 0x80 {
        last -= 1;
    }
    is_whitespace(code_point_at(bytes, 0)) || is_whitespace(code_point_at(bytes, last))
}

/// Decode the code point starting at `at`; `str` guarantees well-formed
/// UTF-8.
const fn code_point_at(bytes: &[u8], at: usize) -> u32 {
    let b0 = bytes[at] as u32;
    match b0 {
        0x00..=0x7F => b0,
        0xC0..=0xDF => ((b0 & 0x1F) << 6) | (bytes[at + 1] as u32 & 0x3F),
        0xE0..=0xEF => {
            ((b0 & 0x0F) << 12)
                | ((bytes[at + 1] as u32 & 0x3F) << 6)
                | (bytes[at + 2] as u32 & 0x3F)
        }
        _ => {
            ((b0 & 0x07) << 18)
                | ((bytes[at + 1] as u32 & 0x3F) << 12)
                | ((bytes[at + 2] as u32 & 0x3F) << 6)
                | (bytes[at + 3] as u32 & 0x3F)
        }
    }
}

/// `char::is_whitespace` over a raw code point.
const fn is_whitespace(cp: u32) -> bool {
    match char::from_u32(cp) {
        Some(c) => c.is_whitespace(),
        None => false,
    }
}

impl std::str::FromStr for VenueId {
    type Err = InvalidVenueId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.to_owned())
    }
}

impl TryFrom<String> for VenueId {
    type Error = InvalidVenueId;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        Self::new(id)
    }
}

impl TryFrom<&str> for VenueId {
    type Error = InvalidVenueId;

    fn try_from(id: &str) -> Result<Self, Self::Error> {
        id.parse()
    }
}

impl AsRef<str> for VenueId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One venue as a keeper types it. Implement on a unit marker
/// (`struct CowVenue;`) and drive it through [`VenueClient`].
pub trait Venue {
    /// The id the venue's adapter registers under.
    const ID: VenueId;

    /// The versioned body schema the venue decodes.
    type Body: IntentBody;

    /// Classify a `denied` detail into the retry action the generic
    /// keeper path folds it to. Defaults to the coarse mapping: every
    /// denial drops. Static so the keeper carries it as a
    /// [`DenialClassifier`](crate::keeper::DenialClassifier).
    ///
    /// Only a drop-class action retires a stranded reservation: classify
    /// a denial as [`RetryAction::TryNextBlock`] and [`reconcile`] holds
    /// the marker `RESERVED` and re-POSTs the refused body every tick.
    ///
    /// [`reconcile`]: crate::keeper::reconcile
    fn classify_denied(detail: &str) -> RetryAction {
        (crate::keeper::DROP_DENIED)(detail)
    }
}

/// Sealing markers: a transport opts into [`VenueTransport`], and an
/// adapter into [`VenueReconcile`], by also implementing the respective
/// marker.
#[doc(hidden)]
pub mod sealed {
    pub trait SealedTransport {}
    pub trait SealedReconcile {}
}

/// The byte-level seam under the typed client: `videre:venue/client`
/// with the venue named per call. Native AFIT, so a [`VenueClient`]
/// over any transport dispatches statically. Sealed: a transport opts
/// in by also implementing the sealing marker.
pub trait VenueTransport: sealed::SealedTransport {
    /// Price an opaque intent body at the named venue. Staleness
    /// enforcement is host-side, not the adapter's: the host records the
    /// quote and refuses a later submit of the same bytes once the
    /// quotation's `valid-until-ms` elapses (`denied` with the
    /// `stale-quote:` prefix). Best-effort, never a guarantee: a
    /// quotation valid far past the host's recording horizon is not
    /// recorded, nor is one quoted while the host's ledger is full.
    fn quote(
        &self,
        venue: &VenueId,
        body: Vec<u8>,
    ) -> impl Future<Output = Result<Quotation, VenueFault>>;

    /// Submit an opaque intent body to the named venue.
    fn submit(
        &self,
        venue: &VenueId,
        body: Vec<u8>,
    ) -> impl Future<Output = Result<SubmitOutcome, VenueFault>>;

    /// Put an externally-obtained receipt under the host's status watch;
    /// an accepted submit is watched implicitly. Defaults to
    /// `unsupported`.
    fn observe(
        &self,
        venue: &VenueId,
        receipt: &[u8],
    ) -> impl Future<Output = Result<(), VenueFault>> {
        let _ = (venue, receipt);
        async { Err(VenueFault::Unsupported) }
    }

    /// Report where a previously submitted intent is in its life.
    fn status(
        &self,
        venue: &VenueId,
        receipt: &[u8],
    ) -> impl Future<Output = Result<IntentStatus, VenueFault>>;

    /// Ask the venue to withdraw an intent. Success means the venue
    /// accepted the cancellation, not that an in-flight settlement can
    /// no longer win the race.
    fn cancel(
        &self,
        venue: &VenueId,
        receipt: &[u8],
    ) -> impl Future<Output = Result<(), VenueFault>>;
}

/// Marker naming the reconcile guarantees an adapter's
/// [`submit`](crate::VenueAdapter::submit) and
/// [`status`](crate::VenueAdapter::status) give, so a keeper recovers a
/// stranded reservation without double-placing. An adapter opts in by
/// implementing it (and the sealing marker), and proves it with
/// `videre_test::venue_reconcile_compliance!`.
///
/// # Contract
///
/// 1. Re-POST idempotency (mandatory): a
///    [`submit`](crate::VenueAdapter::submit) of a body the venue already
///    holds resolves to the SAME outcome as the first, never to a
///    terminal [`VenueFault`]. This is what makes a reconcile resubmit
///    safe.
/// 2. Status fast-path (optional): an adapter MAY derive a receipt from
///    the body, so a reconcile can [`status`](crate::VenueAdapter::status)
///    first and commit without a redundant POST. The reconcile primitive
///    is `submit`, not [`observe`](VenueTransport::observe).
///
/// Reconcile trusts venue-side order validity and does not re-poll the
/// watch source, so adapters must carry self-describing order validity.
pub trait VenueReconcile: crate::VenueAdapter + sealed::SealedReconcile {}

/// Poll a future once and return its state. A [`VenueTransport`] over the
/// host import resolves on the first poll; [`Poll::Pending`] means a
/// foreign impl suspended, which the keeper macro folds to
/// `Fault::Internal`.
pub fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    future.as_mut().poll(&mut cx)
}

/// The module's `videre:venue/client` import behind the
/// [`VenueTransport`] seam; the default transport for a [`VenueClient`].
#[derive(Clone, Copy, Debug, Default)]
pub struct HostVenues;

impl sealed::SealedTransport for HostVenues {}

impl VenueTransport for HostVenues {
    async fn quote(&self, venue: &VenueId, body: Vec<u8>) -> Result<Quotation, VenueFault> {
        shims::quote(venue.as_str(), &body).map_err(VenueFault::from)
    }

    async fn submit(&self, venue: &VenueId, body: Vec<u8>) -> Result<SubmitOutcome, VenueFault> {
        shims::submit(venue.as_str(), &body).map_err(VenueFault::from)
    }

    async fn observe(&self, venue: &VenueId, receipt: &[u8]) -> Result<(), VenueFault> {
        shims::observe(venue.as_str(), receipt).map_err(VenueFault::from)
    }

    async fn status(&self, venue: &VenueId, receipt: &[u8]) -> Result<IntentStatus, VenueFault> {
        shims::status(venue.as_str(), receipt).map_err(VenueFault::from)
    }

    async fn cancel(&self, venue: &VenueId, receipt: &[u8]) -> Result<(), VenueFault> {
        shims::cancel(venue.as_str(), receipt).map_err(VenueFault::from)
    }
}

/// A typed client bound to one [`Venue`]: encodes the venue's
/// [`IntentBody`] and forwards through the [`VenueTransport`] seam under
/// [`Venue::ID`]. Zero-sized over the default [`HostVenues`] transport.
pub struct VenueClient<V: Venue, T: VenueTransport = HostVenues> {
    transport: T,
    venue: PhantomData<V>,
}

impl<V: Venue> VenueClient<V> {
    /// Bind the venue over the module's own `videre:venue/client`
    /// import.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            transport: HostVenues,
            venue: PhantomData,
        }
    }
}

impl<V: Venue> Default for VenueClient<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Venue, T: VenueTransport> VenueClient<V, T> {
    /// Bind the venue over a caller-supplied transport.
    pub const fn with_transport(transport: T) -> Self {
        Self {
            transport,
            venue: PhantomData,
        }
    }

    /// The venue id every call on this client routes to.
    #[must_use]
    pub fn venue(&self) -> VenueId {
        V::ID
    }

    /// The bound transport, so a reconcile pass resubmits reserved bodies
    /// through the same seam this client submits on.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Encode the typed body and price it at the bound venue. The returned
    /// [`Quoted`] carries the encoded bytes, so `submit` sends exactly the
    /// body the venue priced.
    pub async fn quote(&self, body: &V::Body) -> Result<Quoted<'_, V, T>, ClientError> {
        let bytes = body.to_bytes()?;
        let quotation = self.transport.quote(&V::ID, bytes.clone()).await?;
        Ok(Quoted {
            client: self,
            bytes,
            quotation,
        })
    }

    /// Encode the typed body and submit it to the bound venue.
    pub async fn submit(&self, body: &V::Body) -> Result<SubmitOutcome, ClientError> {
        let bytes = body.to_bytes()?;
        Ok(self.transport.submit(&V::ID, bytes).await?)
    }

    /// Put an externally-obtained receipt under the host's status
    /// watch at the bound venue.
    pub async fn observe(&self, receipt: &[u8]) -> Result<(), ClientError> {
        Ok(self.transport.observe(&V::ID, receipt).await?)
    }

    /// Report where a previously submitted intent is in its life.
    /// Rejects an empty receipt as `invalid-receipt` before the wire.
    pub async fn status(&self, receipt: &[u8]) -> Result<IntentStatus, ClientError> {
        crate::adapter::guard_receipt(receipt).map_err(VenueFault::from)?;
        Ok(self.transport.status(&V::ID, receipt).await?)
    }

    /// Ask the bound venue to withdraw an intent. Rejects an empty
    /// receipt as `invalid-receipt` before the wire.
    pub async fn cancel(&self, receipt: &[u8]) -> Result<(), ClientError> {
        crate::adapter::guard_receipt(receipt).map_err(VenueFault::from)?;
        Ok(self.transport.cancel(&V::ID, receipt).await?)
    }
}

impl<V: Venue, T: VenueTransport + Clone> Clone for VenueClient<V, T> {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            venue: PhantomData,
        }
    }
}

impl<V: Venue, T: VenueTransport> fmt::Debug for VenueClient<V, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VenueClient")
            .field("venue", &V::ID)
            .finish_non_exhaustive()
    }
}

/// A priced intent: the quotation plus the exact bytes it prices, bound
/// to the client that fetched it. [`submit`](Self::submit) is the only
/// way from a quote to a submission, so the submitted body is always the
/// quoted one.
pub struct Quoted<'a, V: Venue, T: VenueTransport> {
    client: &'a VenueClient<V, T>,
    bytes: Vec<u8>,
    quotation: Quotation,
}

impl<V: Venue, T: VenueTransport> Quoted<'_, V, T> {
    /// The venue's indicative quotation for the body.
    #[must_use]
    pub fn quotation(&self) -> &Quotation {
        &self.quotation
    }

    /// Submit the quoted body to the venue that priced it. The host
    /// refuses the bytes once the quotation's `valid-until-ms` has
    /// elapsed, best-effort as [`VenueTransport::quote`] describes;
    /// re-quote to re-arm.
    pub async fn submit(self) -> Result<SubmitOutcome, ClientError> {
        Ok(self.client.transport.submit(&V::ID, self.bytes).await?)
    }
}

impl<V: Venue, T: VenueTransport> fmt::Debug for Quoted<'_, V, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Quoted")
            .field("venue", &V::ID)
            .field("quotation", &self.quotation)
            .finish_non_exhaustive()
    }
}

/// Why a typed client call failed: before the wire (the body failed to
/// encode) or beyond it (the registry or venue refused). `IntoStaticStr`
/// yields a snake_case label per case.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ClientError {
    /// The typed body failed to encode; nothing crossed the wire.
    #[error(transparent)]
    Body(#[from] BodyError),
    /// The registry or the venue behind it refused the call.
    #[error(transparent)]
    Venue(#[from] VenueFault),
}

#[cfg(test)]
mod tests {
    use std::task::Poll;

    use super::{VenueId, padded, poll_once};

    #[test]
    fn blank_venue_id_is_rejected_at_construction() {
        assert!("".parse::<VenueId>().is_err());
        assert!("  ".parse::<VenueId>().is_err());
        assert!(VenueId::new("\t\n").is_err());
        assert_eq!("demo".parse::<VenueId>().expect("parses").as_str(), "demo");
        // `:` stays legal for the future chain-qualified shape.
        assert_eq!(
            "demo:11155111".parse::<VenueId>().expect("parses").as_str(),
            "demo:11155111"
        );
    }

    #[test]
    fn padded_venue_id_is_rejected_at_construction() {
        assert!("demo ".parse::<VenueId>().is_err());
        assert!(" demo".parse::<VenueId>().is_err());
        assert!(VenueId::try_from("demo\n").is_err());
        assert!(VenueId::try_from("\tdemo".to_owned()).is_err());
        assert!(VenueId::new("demo\u{a0}").is_err());
        assert!(VenueId::new("\u{2028}demo".to_owned()).is_err());
        // Interior whitespace is not surrounding: reject-not-trim keeps
        // the spelling exact and never rewrites it.
        assert!(VenueId::new("demo venue").is_ok());
    }

    #[test]
    fn valid_ids_construct_through_every_path() {
        assert_eq!(VenueId::from_static("demo").as_str(), "demo");
        assert_eq!(
            VenueId::new("demo".to_owned())
                .expect("constructs")
                .as_str(),
            "demo"
        );
        assert_eq!(
            VenueId::try_from("demo").expect("converts").to_string(),
            "demo"
        );
        assert_eq!(AsRef::<str>::as_ref(&VenueId::from_static("demo")), "demo");
    }

    #[test]
    fn padded_walks_every_utf8_width_at_both_ends() {
        // One boundary char per encoded width, whitespace and not:
        // U+0020/U+0041 (1), U+00A0/U+00C0 (2), U+2028/U+4E00 (3),
        // U+1F600 (4, never whitespace).
        for ws in ["\u{20}", "\u{a0}", "\u{2028}", "\u{3000}"] {
            assert!(padded(&format!("{ws}demo")), "{ws:?} leads");
            assert!(padded(&format!("demo{ws}")), "{ws:?} trails");
        }
        for ok in ["A", "\u{c0}", "\u{4e00}", "\u{1f600}"] {
            assert!(!padded(&format!("{ok}demo{ok}")), "{ok:?} wraps");
        }
    }

    /// `padded` is a hand-rolled const decoder; `str::trim` is the
    /// oracle the host and macros copies spell it with. Exhaustive over
    /// every code point at both ends, so a decoder slip cannot land.
    #[test]
    fn padded_agrees_with_str_trim_over_every_code_point() {
        for cp in 0..=0x10FFFF {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            for s in [format!("{c}x"), format!("x{c}")] {
                assert_eq!(padded(&s), s.trim().len() != s.len(), "U+{cp:04X}");
            }
        }
    }

    #[test]
    fn ready_chain_completes_in_one_poll() {
        async fn two() -> u8 {
            let one = async { 1u8 }.await;
            one + async { 1u8 }.await
        }
        assert_eq!(poll_once(two()), Poll::Ready(2));
    }

    #[test]
    fn suspending_future_reports_pending() {
        assert_eq!(poll_once(std::future::pending::<()>()), Poll::Pending);
    }
}
