#![crate_type = "lib"]
#![feature(futures_api, async_await, await_macro, arbitrary_self_types)]
#![feature(nll)]
#![feature(generators)]
#![feature(never_type)]
#![type_length_limit = "2097152"]
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
extern crate common;

mod client;
mod server;

pub use self::client::client_connector::ClientConnector;
pub use self::client::client_listener::ClientListener;
pub use self::server::net_server::{net_relay_server, NetRelayServerError};
