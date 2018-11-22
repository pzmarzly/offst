#![feature(try_from)]
#![feature(nll)]

extern crate capnp;
extern crate byteorder;

#[macro_use]
extern crate offst_utils as utils;
extern crate offst_crypto as crypto;

extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate bytes;

#[macro_use]
pub mod macros;
pub mod capnp_common;
pub mod relay;
pub mod secure_channel;
pub mod funder;



include_schema!(channeler_capnp, "channeler_capnp");
include_schema!(common_capnp, "common_capnp");
include_schema!(dh_capnp, "dh_capnp");
include_schema!(relay_capnp, "relay_capnp");
include_schema!(funder_capnp, "funder_capnp");
