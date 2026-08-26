//! Typed wrappers over the adapter world's scoped transport imports
//! (chain RPC and outbound wasi:http), adapting the bindgen shims to the
//! SDK-neutral `nexum_sdk::host` vocabulary.
//!
//! The wrappers only translate; scoping is the host's: chain to its
//! read-only surface and HTTP to `http_allow`, each refusal surfacing as
//! a typed `denied`.

use core::time::Duration;

use nexum_sdk::host::{ChainError, ChainHost, RpcError};
use nexum_sdk::http::{Fetch, FetchError, FetchOptions};

use crate::bindings::nexum::host::chain;
use crate::faults::fault_into_sdk;

/// Outbound HTTP for adapters: the SDK's wasi:http surface re-exported.
/// An off-allowlist request fails as [`FetchError::Denied`], which
/// converts into [`VenueError`](crate::VenueError) via `?`.
pub use nexum_sdk::http;

/// Clamps every wasi:http phase timeout of the inner [`Fetch`] (connect,
/// first byte, between bytes) to at most `bound`, so a hung endpoint
/// errors rather than stalling the export call. A caller may ask for
/// less, never more.
#[derive(Clone, Copy, Debug)]
pub struct BoundedFetch<F> {
    inner: F,
    bound: Duration,
}

impl<F> BoundedFetch<F> {
    /// Bound every phase (connect, first byte, between bytes) of every
    /// request to at most `bound`.
    pub const fn new(inner: F, bound: Duration) -> Self {
        Self { inner, bound }
    }
}

impl<F: Fetch> Fetch for BoundedFetch<F> {
    fn fetch_with(
        &self,
        request: ::http::Request<Vec<u8>>,
        options: FetchOptions,
    ) -> Result<::http::Response<Vec<u8>>, FetchError> {
        self.inner.fetch_with(
            request,
            FetchOptions {
                connect_timeout: options.connect_timeout.min(self.bound),
                first_byte_timeout: options.first_byte_timeout.min(self.bound),
                between_bytes_timeout: options.between_bytes_timeout.min(self.bound),
            },
        )
    }
}

/// The adapter's `nexum:host/chain` import behind the SDK's [`ChainHost`]
/// seam.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostChain;

impl ChainHost for HostChain {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError> {
        chain::request(chain_id, method, params).map_err(chain_error_into_sdk)
    }
}

/// Lift the wire chain error into the SDK-neutral [`ChainError`].
fn chain_error_into_sdk(err: chain::ChainError) -> ChainError {
    match err {
        chain::ChainError::Fault(fault) => ChainError::Fault(fault_into_sdk(fault)),
        chain::ChainError::Rpc(rpc) => ChainError::Rpc(RpcError {
            code: rpc.code,
            message: rpc.message,
            data: rpc.data.map(Into::into),
        }),
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use core::time::Duration;

    use nexum_sdk::http::{Fetch, FetchError, FetchOptions};

    use super::BoundedFetch;

    struct Spy {
        seen: Cell<Option<FetchOptions>>,
    }

    impl Fetch for Spy {
        fn fetch_with(
            &self,
            _request: http::Request<Vec<u8>>,
            options: FetchOptions,
        ) -> Result<http::Response<Vec<u8>>, FetchError> {
            self.seen.set(Some(options));
            Ok(http::Response::new(Vec::new()))
        }
    }

    fn request() -> http::Request<Vec<u8>> {
        http::Request::get("https://api.cow.fi/")
            .body(Vec::new())
            .expect("test request builds")
    }

    #[test]
    fn plain_fetch_is_bounded() {
        let bound = Duration::from_secs(5);
        let timed = BoundedFetch::new(
            Spy {
                seen: Cell::new(None),
            },
            bound,
        );
        timed.fetch(request()).expect("spy accepts");
        let seen = timed.inner.seen.get().expect("options recorded");
        assert_eq!(seen.connect_timeout, bound);
        assert_eq!(seen.first_byte_timeout, bound);
        assert_eq!(seen.between_bytes_timeout, bound);
    }

    #[test]
    fn caller_options_clamp_to_the_bound_but_tighter_ones_pass() {
        let timed = BoundedFetch::new(
            Spy {
                seen: Cell::new(None),
            },
            Duration::from_secs(5),
        );
        timed
            .fetch_with(
                request(),
                FetchOptions {
                    connect_timeout: Duration::from_secs(60),
                    first_byte_timeout: Duration::from_secs(1),
                    between_bytes_timeout: Duration::from_secs(60),
                },
            )
            .expect("spy accepts");
        let seen = timed.inner.seen.get().expect("options recorded");
        assert_eq!(seen.connect_timeout, Duration::from_secs(5));
        assert_eq!(seen.first_byte_timeout, Duration::from_secs(1));
        assert_eq!(seen.between_bytes_timeout, Duration::from_secs(5));
    }
}
