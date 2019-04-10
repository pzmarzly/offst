#![feature(futures_api, async_await, await_macro, arbitrary_self_types)]
#![feature(nll)]
#![feature(generators)]
#![feature(never_type)]
#![deny(trivial_numeric_casts, warnings)]
#![allow(intra_doc_link_resolution_failure)]
#![allow(
    clippy::too_many_arguments,
    clippy::implicit_hasher,
    clippy::module_inception
)]
// TODO: disallow clippy::too_many_arguments

#[macro_use]
extern crate log;

use std::convert::TryInto;
use std::path::{Path, PathBuf};

use structopt::StructOpt;

use crypto::crypto_rand::system_random;
use crypto::identity::{generate_pkcs8_key_pair, Identity};

use proto::app_server::messages::{AppPermissions, RelayAddress};
use proto::index_server::messages::IndexServerAddress;
use proto::net::messages::{NetAddress, NetAddressError};
use proto::node::types::NodeAddress;

use database::file_db::FileDb;
use node::NodeState;

use proto::file::app::{store_trusted_app_to_file, TrustedApp};
use proto::file::identity::{load_identity_from_file, store_identity_to_file};
use proto::file::index_server::store_index_server_to_file;
use proto::file::node::store_node_to_file;
use proto::file::relay::store_relay_to_file;

#[derive(Debug)]
enum InitNodeDbError {
    OutputAlreadyExists,
    LoadIdentityError,
    FileDbError,
}

#[derive(Debug, StructOpt)]
struct InitNodeDbCmd {
    /// StCtrl app identity file path
    #[structopt(parse(from_os_str), short = "I", long = "idfile")]
    idfile: PathBuf,
    /// Database output file path
    #[structopt(parse(from_os_str), short = "I", long = "output")]
    output: PathBuf,
}

#[derive(Debug, StructOpt)]
struct GenIdentCmd {
    /// Identity file output file path
    #[structopt(parse(from_os_str), short = "I", long = "output")]
    output: PathBuf,
}

#[derive(Debug, StructOpt)]
struct AppTicketCmd {
    /// StCtrl app identity file path
    #[structopt(parse(from_os_str), short = "I", long = "idfile")]
    idfile: PathBuf,
    /// Application ticket output file path
    #[structopt(parse(from_os_str), short = "I", long = "output")]
    output: PathBuf,
    /// Permission to request reports
    #[structopt(long = "proutes")]
    proutes: bool,
    /// Permission to send funds
    #[structopt(long = "pfunds")]
    pfunds: bool,
    /// Permission to change configuration
    #[structopt(long = "pconfig")]
    pconfig: bool,
}

#[derive(Debug, StructOpt)]
struct RelayTicketCmd {
    /// StCtrl app identity file path
    #[structopt(parse(from_os_str), short = "I", long = "idfile")]
    idfile: PathBuf,
    /// Relay ticket output file path
    #[structopt(parse(from_os_str), short = "I", long = "output")]
    output: PathBuf,
    /// Public address of the relay
    #[structopt(long = "address")]
    address: SocketAddr,
}

#[derive(Debug, StructOpt)]
struct IndexTicketCmd {
    /// StCtrl app identity file path
    #[structopt(parse(from_os_str), short = "I", long = "idfile")]
    idfile: PathBuf,
    /// Index server ticket output file path
    #[structopt(parse(from_os_str), short = "I", long = "output")]
    output: PathBuf,
    /// Public address of the index server
    #[structopt(long = "address")]
    address: SocketAddr,
}

#[derive(Debug, StructOpt)]
struct NodeTicketCmd {
    /// StCtrl app identity file path
    #[structopt(parse(from_os_str), short = "I", long = "idfile")]
    idfile: PathBuf,
    /// Node server ticket output file path
    #[structopt(parse(from_os_str), short = "I", long = "output")]
    output: PathBuf,
    /// Public address of the node server
    #[structopt(long = "address")]
    address: SocketAddr,
}

// TODO: Add version (0.1.0)
// TODO: Add author
// TODO: Add description - Performs Offst related management operations
/// stmgr: offST ManaGeR
#[derive(Debug, StructOpt)]
enum StMgrCmd {
    /// Initialize a new (empty) node database
    #[structopt(name="init-node-db")]
    InitNodeDb(InitNodeDbCmd),
    /// Randomly generate a new identity file
    #[structopt(name="gen-ident")]
    GenIdent(GenIdentCmd),
    /// Create an application ticket
    #[structopt(name="app-ticket")]
    AppTicket(AppTicketCmd),
     /// Create a relay ticket
    #[structopt(name="relay-ticket")]
    RelayTicket(RelayTicketCmd),
    /// Create an index server ticket
    #[structopt(name="index-ticket")]
    IndexTicket(IndexTicketCmd),
    /// Create a node server ticket
    #[structopt(name="node-ticket")]
    NodeTicket(NodeTicketCmd),
}

fn init_node_db(InitNodeDbCmd{idfile, output}: InitNodeDbCmd) -> Result<(), InitNodeDbError> {
    // Make sure that output_path does not exist.
    // This program should never override any file! (Otherwise users might erase their database by
    // accident).
    let output_path = PathBuf::from(output);
    if output_path.exists() {
        return Err(InitNodeDbError::OutputAlreadyExists);
    }

    // Parse identity file:
    let identity = load_identity_from_file(Path::new(&idfile))
        .map_err(|_| InitNodeDbError::LoadIdentityError)?;
    let local_public_key = identity.get_public_key();

    // Create a new database file:
    let initial_state = NodeState::<NetAddress>::new(local_public_key);
    let _ = FileDb::create(output_path.to_path_buf(), initial_state)
        .map_err(|_| InitNodeDbError::FileDbError)?;

    Ok(())
}

#[derive(Debug)]
enum GenIdentityError {
    StoreToFileError,
}

/// Randomly generate an identity file (private-public key pair)
fn gen_identity(GenIdentCmd{output}: GenIdentCmd) -> Result<(), GenIdentityError> {
    // Generate a new random keypair:
    let rng = system_random();
    let pkcs8 = generate_pkcs8_key_pair(&rng);

    store_identity_to_file(pkcs8, output).map_err(|_| GenIdentityError::StoreToFileError)
}

#[derive(Debug)]
enum AppTicketError {
    OutputAlreadyExists,
    LoadIdentityError,
    StoreAppFileError,
}

/// Create an app ticket.
/// The ticket contains:
/// - public key
/// - application permissions
///
/// The app ticket is used to authorize an application to
/// connect to a running node.
fn app_ticket(AppTicketCmd {idfile, output, proutes, pfunds, pconfig}: AppTicketCmd) -> Result<(), AppTicketError> {
    // Obtain app's public key:
    let identity = load_identity_from_file(Path::new(&idfile))
        .map_err(|_| AppTicketError::LoadIdentityError)?;
    let public_key = identity.get_public_key();

    // Get app's permissions:
    let permissions = AppPermissions {
        routes,
        send_funds,
        config,
    };

    // Store app ticket to file:
    let trusted_app = TrustedApp {
        public_key,
        permissions,
    };
    store_trusted_app_to_file(&trusted_app, &output)
        .map_err(|_| AppTicketError::StoreAppFileError)
}

#[derive(Debug)]
enum RelayTicketError {
    OutputAlreadyExists,
    LoadIdentityError,
    StoreRelayFileError,
    NetAddressError(NetAddressError),
}

impl From<NetAddressError> for RelayTicketError {
    fn from(e: NetAddressError) -> Self {
        RelayTicketError::NetAddressError(e)
    }
}

/// Create a public relay ticket
/// The ticket can be fed into a running offst node
fn relay_ticket(input: RelayTicketCmd) -> Result<(), RelayTicketError> {
    let idfile = matches.value_of("idfile").unwrap();
    let output = matches.value_of("output").unwrap();

    // Make sure that output_path does not exist.
    let output_path = Path::new(output);
    if output_path.exists() {
        return Err(RelayTicketError::OutputAlreadyExists);
    }

    // Parse identity file:
    let identity = load_identity_from_file(Path::new(&idfile))
        .map_err(|_| RelayTicketError::LoadIdentityError)?;
    let public_key = identity.get_public_key();

    let address_str = matches.value_of("address").unwrap();

    let relay_address = RelayAddress {
        public_key,
        address: address_str.to_owned().try_into()?,
    };

    store_relay_to_file(&relay_address, &output_path)
        .map_err(|_| RelayTicketError::StoreRelayFileError)
}

#[derive(Debug)]
enum IndexTicketError {
    OutputAlreadyExists,
    LoadIdentityError,
    StoreIndexFileError,
    NetAddressError(NetAddressError),
}

impl From<NetAddressError> for IndexTicketError {
    fn from(e: NetAddressError) -> Self {
        IndexTicketError::NetAddressError(e)
    }
}

/// Create a public index ticket
/// The ticket can be fed into a running offst node
fn index_ticket(input: IndexTicketCmd) -> Result<(), IndexTicketError> {
    let idfile = matches.value_of("idfile").unwrap();
    let output = matches.value_of("output").unwrap();

    // Make sure that output_path does not exist.
    let output_path = Path::new(output);
    if output_path.exists() {
        return Err(IndexTicketError::OutputAlreadyExists);
    }

    // Parse identity file:
    let identity = load_identity_from_file(Path::new(&idfile))
        .map_err(|_| IndexTicketError::LoadIdentityError)?;
    let public_key = identity.get_public_key();

    let address_str = matches.value_of("address").unwrap();

    let index_address = IndexServerAddress {
        public_key,
        address: address_str.to_owned().try_into()?,
    };

    store_index_server_to_file(&index_address, &output_path)
        .map_err(|_| IndexTicketError::StoreIndexFileError)
}

#[derive(Debug)]
enum NodeTicketError {
    OutputAlreadyExists,
    LoadIdentityError,
    StoreNodeFileError,
    NetAddressError(NetAddressError),
}

impl From<NetAddressError> for NodeTicketError {
    fn from(e: NetAddressError) -> Self {
        NodeTicketError::NetAddressError(e)
    }
}

/// Create a node ticket
/// The ticket can be fed into a node application
fn node_ticket(input: NodeTicketCmd) -> Result<(), NodeTicketError> {
    let idfile = matches.value_of("idfile").unwrap();
    let output = matches.value_of("output").unwrap();

    // Make sure that output_path does not exist.
    let output_path = Path::new(output);
    if output_path.exists() {
        return Err(NodeTicketError::OutputAlreadyExists);
    }

    // Parse identity file:
    let identity = load_identity_from_file(Path::new(&idfile))
        .map_err(|_| NodeTicketError::LoadIdentityError)?;
    let public_key = identity.get_public_key();

    let address_str = matches.value_of("address").unwrap();

    let node_address = NodeAddress {
        public_key,
        address: address_str.to_owned().try_into()?,
    };

    store_node_to_file(&node_address, &output_path).map_err(|_| NodeTicketError::StoreNodeFileError)
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
enum StmError {
    InitNodeDbError(InitNodeDbError),
    GenIdentityError(GenIdentityError),
    AppTicketError(AppTicketError),
    RelayTicketError(RelayTicketError),
    IndexTicketError(IndexTicketError),
    NodeTicketError(NodeTicketError),
}

impl From<InitNodeDbError> for StmError {
    fn from(e: InitNodeDbError) -> Self {
        StmError::InitNodeDbError(e)
    }
}

impl From<GenIdentityError> for StmError {
    fn from(e: GenIdentityError) -> Self {
        StmError::GenIdentityError(e)
    }
}

impl From<AppTicketError> for StmError {
    fn from(e: AppTicketError) -> Self {
        StmError::AppTicketError(e)
    }
}

impl From<RelayTicketError> for StmError {
    fn from(e: RelayTicketError) -> Self {
        StmError::RelayTicketError(e)
    }
}

impl From<IndexTicketError> for StmError {
    fn from(e: IndexTicketError) -> Self {
        StmError::IndexTicketError(e)
    }
}

impl From<NodeTicketError> for StmError {
    fn from(e: NodeTicketError) -> Self {
        StmError::NodeTicketError(e)
    }
}

fn run() -> Result<(), StmError> {
    env_logger::init();

    match matches.subcommand() {
        ("init-node-db", Some(matches)) => init_node_db(matches)?,
        ("gen-ident", Some(matches)) => gen_identity(matches)?,
        ("app-ticket", Some(matches)) => app_ticket(matches)?,
        ("relay-ticket", Some(matches)) => relay_ticket(matches)?,
        ("index-ticket", Some(matches)) => index_ticket(matches)?,
        ("node-ticket", Some(matches)) => node_ticket(matches)?,
        _ => unreachable!(),
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        error!("run() error: {:?}", e);
    }
}
