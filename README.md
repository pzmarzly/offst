# CSwitch

[![Build Status](https://travis-ci.com/kamyuentse/cswitch.svg?token=bxuBsFuxMyAChxHzJWNQ&branch=master)](https://travis-ci.com/kamyuentse/cswitch)
[![codecov](https://codecov.io/gh/kamyuentse/cswitch/branch/master/graph/badge.svg?token=8wnbKAjDFl)](https://codecov.io/gh/kamyuentse/cswitch)

A Credit Switching engine written in Rust.


## Setting up development environment

Theoretically CSwitch should work anywhere Rust works (Windows, Linux, MacOS).

- [Install Rust](https://www.rust-lang.org/install.html). We currently use
    nightly Rust.
- Install libsqlite3-dev. On ubuntu, run `sudo apt install libsqlite3-dev`.
- [Install capnproto](https://capnproto.org/install.html). On Ubuntu, run `sudo apt install capnproto`
- Install capnproto plugin for rust using cargo: `cargo install capnpc`.

After all is done, run 

```bash
cargo test
```

to make sure that all tests pass.
