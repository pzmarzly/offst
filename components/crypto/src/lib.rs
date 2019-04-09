#![deny(trivial_numeric_casts, warnings)]
#![allow(intra_doc_link_resolution_failure)]
#![allow(
    clippy::too_many_arguments,
    clippy::implicit_hasher,
    clippy::module_inception
)]
// TODO: disallow clippy::too_many_arguments

extern crate bytes;
extern crate ring;
#[macro_use]
extern crate common;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate base64;
extern crate byteorder;
extern crate rand;
#[macro_use]
extern crate lazy_static;

pub mod crypto_rand;
pub mod dh;
pub mod hash;
pub mod identity;
pub mod nonce_window;
pub mod sym_encrypt;
pub mod uid;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CryptoError;

impl From<::ring::error::Unspecified> for CryptoError {
    fn from(_: ::ring::error::Unspecified) -> CryptoError {
        CryptoError
    }
}

impl From<::ring::error::KeyRejected> for CryptoError {
    fn from(_: ::ring::error::KeyRejected) -> CryptoError {
        CryptoError
    }
}

impl ::std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        f.write_str("crypto error")
    }
}

impl ::std::error::Error for CryptoError {
    #[inline]
    fn description(&self) -> &str {
        "crypto error"
    }

    #[inline]
    fn cause(&self) -> Option<&::std::error::Error> {
        None
    }
}

/// Increase the bytes represented number by 1.
///
/// Reference: `libsodium/sodium/utils.c#L241`
#[inline]
pub fn increase_nonce(nonce: &mut [u8]) {
    let mut c: u16 = 1;
    for i in nonce {
        c += u16::from(*i);
        *i = c as u8;
        c >>= 8;
    }
}
