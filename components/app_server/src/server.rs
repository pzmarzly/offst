use std::marker::Unpin;
use std::fmt::Debug;
use std::collections::HashMap;

use futures::{future, FutureExt, stream, Stream, StreamExt, Sink, SinkExt};
use futures::channel::mpsc;
use futures::task::{Spawn, SpawnExt};

use common::conn::ConnPair;
use crypto::identity::PublicKey;

use proto::funder::messages::{FunderOutgoingControl, FunderIncomingControl, 
    RemoveFriend, SetFriendStatus, FriendStatus,
    RequestsStatus, SetRequestsStatus};
use proto::app_server::messages::{AppServerToApp, AppToAppServer};
use proto::index_client::messages::{IndexClientToAppServer, AppServerToIndexClient};

use crate::config::AppPermissions;

type IncomingAppConnection<B,ISA> = (PublicKey, AppPermissions, ConnPair<AppServerToApp<B,ISA>, AppToAppServer<B,ISA>>);


pub enum AppServerError {
    FunderClosed,
    SpawnError,
    IndexClientClosed,
    SendToFunderError,
    SendToIndexClientError,
}

pub enum AppServerEvent<B: Clone,ISA> {
    IncomingConnection(IncomingAppConnection<B,ISA>),
    FromFunder(FunderOutgoingControl<Vec<B>>),
    FunderClosed,
    FromIndexClient(IndexClientToAppServer<ISA>),
    IndexClientClosed,
    FromApp((u128, Option<AppToAppServer<B,ISA>>)), // None means that app was closed
}

pub struct App<B: Clone,ISA> {
    public_key: PublicKey,
    permissions: AppPermissions,
    opt_sender: Option<mpsc::Sender<AppServerToApp<B,ISA>>>,
}

pub struct AppServer<B: Clone,ISA,TF,TIC,S> {
    to_funder: TF,
    to_index_client: TIC,
    from_app_sender: mpsc::Sender<(u128, Option<AppToAppServer<B,ISA>>)>,
    /// A long cyclic incrementing counter, 
    /// allows to give every connection a unique number.
    /// Required because an app (with one public key) might have multiple connections.
    app_counter: u128,
    apps: HashMap<u128, App<B,ISA>>,
    spawner: S,
}

/// Check if we should process an app_message from an app with certain permissions
fn check_permissions<B,ISA>(app_permissions: &AppPermissions, 
                     app_message: &AppToAppServer<B,ISA>) -> bool {

    match app_message {
        AppToAppServer::SetRelays(_) => app_permissions.config,
        AppToAppServer::RequestSendFunds(_) => app_permissions.send_funds,
        AppToAppServer::ReceiptAck(_) => app_permissions.send_funds,
        AppToAppServer::AddFriend(_) => app_permissions.config,
        AppToAppServer::SetFriendRelays(_) => app_permissions.config,
        AppToAppServer::SetFriendName(_) => app_permissions.config,
        AppToAppServer::RemoveFriend(_) => app_permissions.config,
        AppToAppServer::EnableFriend(_) => app_permissions.config,
        AppToAppServer::DisableFriend(_) => app_permissions.config,
        AppToAppServer::OpenFriend(_) => app_permissions.config,
        AppToAppServer::CloseFriend(_) => app_permissions.config,
        AppToAppServer::SetFriendRemoteMaxDebt(_) => app_permissions.config,
        AppToAppServer::ResetFriendChannel(_) => app_permissions.config,
        AppToAppServer::RequestRoutes(_) => app_permissions.routes,
        AppToAppServer::AddIndexServer(_) => app_permissions.config,
        AppToAppServer::RemoveIndexServer(_) => app_permissions.config,
    }
}

impl<B,ISA,TF,TIC,S> AppServer<B,ISA,TF,TIC,S> 
where
    B: Clone + Send + Debug + 'static,
    ISA: Send + Debug + 'static,
    TF: Sink<SinkItem=FunderIncomingControl<Vec<B>>> + Unpin + Sync + Send,
    TIC: Sink<SinkItem=AppServerToIndexClient<ISA>> + Unpin,
    S: Spawn,
{
    pub fn new(to_funder: TF, 
               to_index_client: TIC,
               from_app_sender: mpsc::Sender<(u128, Option<AppToAppServer<B,ISA>>)>,
               spawner: S) -> Self {

        AppServer {
            to_funder,
            to_index_client,
            from_app_sender,
            app_counter: 0,
            apps: HashMap::new(),
            spawner,
        }
    }

    /// Add an application connection
    pub fn handle_incoming_connection(&mut self, incoming_app_connection: IncomingAppConnection<B,ISA>) 
        -> Result<(), AppServerError> {

        let (public_key, permissions, (sender, receiver)) = incoming_app_connection;

        let app_counter = self.app_counter;
        let mut receiver = receiver
            .map(move |app_to_app_server| (app_counter.clone(), Some(app_to_app_server)));

        let mut from_app_sender = self.from_app_sender.clone();
        let send_all_fut = async move {
            // Forward all messages:
            let _ = await!(from_app_sender.send_all(&mut receiver));
            // Notify that the connection to the app was closed:
            let _ = await!(from_app_sender.send((app_counter, None)));
        };

        self.spawner.spawn(send_all_fut)
            .map_err(|_| AppServerError::SpawnError)?;

        let app = App {
            public_key,
            permissions,
            opt_sender: Some(sender),
        };

        self.apps.insert(self.app_counter, app);
        self.app_counter = self.app_counter.wrapping_add(1);
        Ok(())

    }

    pub fn handle_from_funder(&mut self, funder_message: FunderOutgoingControl<Vec<B>>)
        -> Result<(), AppServerError> {

        unimplemented!();
    }

    pub fn handle_from_index_client(&mut self, index_client_message: IndexClientToAppServer<ISA>) 
        -> Result<(), AppServerError> {

        unimplemented!();
    }

    async fn handle_app_message(&mut self, app_id: u128, app_message: AppToAppServer<B,ISA>)
        -> Result<(), AppServerError> {

        // Get the relevant application:
        let app = match self.apps.get_mut(&app_id) {
            Some(app) => app,
            None => {
                warn!("App {:?} does not exist!", app_id);
                return Ok(());
            },
        };

        // Make sure this message is allowed for this application:
        if !check_permissions(&app.permissions, &app_message) {
            warn!("App {:?} does not have permissions for {:?}", app_id, app_message);
            return Ok(());
        }

        match app_message {
            AppToAppServer::SetRelays(relays) =>
                await!(self.to_funder.send(FunderIncomingControl::SetAddress(relays)))
                    .map_err(|_| AppServerError::SendToFunderError),
            AppToAppServer::RequestSendFunds(user_request_send_funds) => 
                await!(self.to_funder.send(FunderIncomingControl::RequestSendFunds(user_request_send_funds)))
                    .map_err(|_| AppServerError::SendToFunderError),
            AppToAppServer::ReceiptAck(receipt_ack) =>
                await!(self.to_funder.send(FunderIncomingControl::ReceiptAck(receipt_ack)))
                    .map_err(|_| AppServerError::SendToFunderError),
            AppToAppServer::AddFriend(add_friend) =>
                await!(self.to_funder.send(FunderIncomingControl::AddFriend(add_friend)))
                    .map_err(|_| AppServerError::SendToFunderError),
            AppToAppServer::SetFriendRelays(set_friend_address) =>
                await!(self.to_funder.send(FunderIncomingControl::SetFriendAddress(set_friend_address)))
                    .map_err(|_| AppServerError::SendToFunderError),
            AppToAppServer::SetFriendName(set_friend_name) => 
                await!(self.to_funder.send(FunderIncomingControl::SetFriendName(set_friend_name)))
                    .map_err(|_| AppServerError::SendToFunderError),
            AppToAppServer::RemoveFriend(friend_public_key) => {
                let remove_friend = RemoveFriend { friend_public_key };
                await!(self.to_funder.send(FunderIncomingControl::RemoveFriend(remove_friend)))
                    .map_err(|_| AppServerError::SendToFunderError)
            },
            AppToAppServer::EnableFriend(friend_public_key) => {
                let set_friend_status = SetFriendStatus {
                    friend_public_key,
                    status: FriendStatus::Enabled,
                };
                await!(self.to_funder.send(FunderIncomingControl::SetFriendStatus(set_friend_status)))
                    .map_err(|_| AppServerError::SendToFunderError)
            },
            AppToAppServer::DisableFriend(friend_public_key) => {
                let set_friend_status = SetFriendStatus {
                    friend_public_key,
                    status: FriendStatus::Disabled,
                };
                await!(self.to_funder.send(FunderIncomingControl::SetFriendStatus(set_friend_status)))
                    .map_err(|_| AppServerError::SendToFunderError)
            },
            AppToAppServer::OpenFriend(friend_public_key) => {
                let set_requests_status = SetRequestsStatus {
                    friend_public_key,
                    status: RequestsStatus::Open,
                };
                await!(self.to_funder.send(FunderIncomingControl::SetRequestsStatus(set_requests_status))) 
                    .map_err(|_| AppServerError::SendToFunderError)
            },
            AppToAppServer::CloseFriend(friend_public_key) => {
                let set_requests_status = SetRequestsStatus {
                    friend_public_key,
                    status: RequestsStatus::Closed,
                };
                await!(self.to_funder.send(FunderIncomingControl::SetRequestsStatus(set_requests_status)))
                    .map_err(|_| AppServerError::SendToFunderError)
            },
            AppToAppServer::SetFriendRemoteMaxDebt(set_friend_remote_max_debt) =>
                await!(self.to_funder.send(FunderIncomingControl::SetFriendRemoteMaxDebt(set_friend_remote_max_debt)))
                    .map_err(|_| AppServerError::SendToFunderError),
            AppToAppServer::ResetFriendChannel(reset_friend_channel) =>
                await!(self.to_funder.send(FunderIncomingControl::ResetFriendChannel(reset_friend_channel)))
                    .map_err(|_| AppServerError::SendToFunderError),
            AppToAppServer::RequestRoutes(request_routes) =>
                await!(self.to_index_client.send(AppServerToIndexClient::RequestRoutes(request_routes)))
                    .map_err(|_| AppServerError::SendToIndexClientError),
            AppToAppServer::AddIndexServer(index_server_address) =>
                await!(self.to_index_client.send(AppServerToIndexClient::AddIndexServer(index_server_address)))
                    .map_err(|_| AppServerError::SendToIndexClientError),
            AppToAppServer::RemoveIndexServer(index_server_address) =>
                await!(self.to_index_client.send(AppServerToIndexClient::RemoveIndexServer(index_server_address)))
                    .map_err(|_| AppServerError::SendToIndexClientError),
        
        }
    }

    pub async fn handle_from_app(&mut self, app_id: u128, opt_app_message: Option<AppToAppServer<B,ISA>>)
        -> Result<(), AppServerError> {

        match opt_app_message {
            None => {
                // Remove the application. We assert that this application exists
                // in our apps map:
                self.apps.remove(&app_id).unwrap();
                Ok(())
            },
            Some(app_message) => await!(self.handle_app_message(app_id, app_message)),
        }
    }
}


pub async fn app_server_loop<B,ISA,FF,TF,FIC,TIC,IC,S>(from_funder: FF, 
                                                       to_funder: TF, 
                                                       from_index_client: FIC,
                                                       to_index_client: TIC,
                                                       incoming_connections: IC,
                                                       mut spawner: S) -> Result<(), AppServerError>
where
    B: Clone + Send + Debug + 'static,
    ISA: Send + Debug + 'static,
    FF: Stream<Item=FunderOutgoingControl<Vec<B>>> + Unpin,
    TF: Sink<SinkItem=FunderIncomingControl<Vec<B>>> + Unpin + Sync + Send,
    FIC: Stream<Item=IndexClientToAppServer<ISA>> + Unpin,
    TIC: Sink<SinkItem=AppServerToIndexClient<ISA>> + Unpin,
    IC: Stream<Item=IncomingAppConnection<B,ISA>> + Unpin,
    S: Spawn,
{

    let (from_app_sender, from_app_receiver) = mpsc::channel(0);
    let mut app_server = AppServer::new(to_funder, 
                                        to_index_client,
                                        from_app_sender,
                                        spawner);

    let from_funder = from_funder
        .map(|funder_outgoing_control| AppServerEvent::FromFunder(funder_outgoing_control))
        .chain(stream::once(future::ready(AppServerEvent::FunderClosed)));

    let from_index_client = from_index_client
        .map(|index_client_msg| AppServerEvent::FromIndexClient(index_client_msg))
        .chain(stream::once(future::ready(AppServerEvent::IndexClientClosed)));

    let from_app_receiver = from_app_receiver
        .map(|from_app: (u128, Option<AppToAppServer<B,ISA>>)| AppServerEvent::FromApp(from_app));

    let incoming_connections = incoming_connections
        .map(|incoming_connection| AppServerEvent::IncomingConnection(incoming_connection));

    let mut events = from_funder
                    .select(from_index_client)
                    .select(from_app_receiver)
                    .select(incoming_connections);

    while let Some(event) = await!(events.next()) {
        match event {
            AppServerEvent::IncomingConnection(incoming_app_connection) =>
                app_server.handle_incoming_connection(incoming_app_connection)?,
            AppServerEvent::FromFunder(funder_outgoing_control) => 
                app_server.handle_from_funder(funder_outgoing_control)?,
            AppServerEvent::FunderClosed => return Err(AppServerError::FunderClosed),
            AppServerEvent::FromIndexClient(from_index_client) => 
                app_server.handle_from_index_client(from_index_client)?,
            AppServerEvent::IndexClientClosed => return Err(AppServerError::IndexClientClosed),
            AppServerEvent::FromApp((app_id, opt_app_message)) => 
                await!(app_server.handle_from_app(app_id, opt_app_message))?,
        }
    }
    Ok(())
}
