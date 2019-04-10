#![crate_type = "lib"]
#![feature(futures_api, async_await, await_macro, arbitrary_self_types)]
#![feature(nll)]
#![feature(generators)]
#![feature(never_type)]
#![deny(trivial_numeric_casts, warnings)]
#![allow(intra_doc_link_resolution_failure)]
#![allow(
    clippy::too_many_arguments,
    clippy::implicit_hasher,
    clippy::module_inception,
    clippy::new_without_default
)]

#[macro_use]
extern crate log;

#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate common;

mod client_session;
mod index_client;
mod seq_friends;
mod seq_map;
mod single_client;
mod spawn;

#[cfg(test)]
mod tests;

pub use self::index_client::{IndexClientConfig, IndexClientConfigMutation, IndexClientError};
pub use self::spawn::{spawn_index_client, SpawnIndexClientError};
