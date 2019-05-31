use std::collections::HashMap;
use std::fmt::Debug;
use std::marker::Unpin;

use futures::channel::mpsc;
use futures::task::{Spawn, SpawnExt};
use futures::{future, stream, Sink, SinkExt, Stream, StreamExt};

use common::conn::ConnPair;
use common::select_streams::{select_streams, BoxStream};
// use common::mutable_state::MutableState;
use crypto::payment_id::PaymentId;
use crypto::uid::Uid;

use proto::funder::messages::{
    FriendStatus, FunderControl, FunderIncomingControl, FunderOutgoingControl, RemoveFriend,
    RequestsStatus, SetFriendStatus, SetRequestsStatus,
};
use proto::report::convert::funder_report_mutation_to_index_mutation;

use proto::app_server::messages::{
    AppPermissions, AppRequest, AppServerToApp, AppToAppServer, NodeReport, NodeReportMutation,
    ReportMutations,
};
use proto::index_client::messages::{
    AppServerToIndexClient, IndexClientRequest, IndexClientToAppServer,
};

pub type IncomingAppConnection<B> = (
    AppPermissions,
    ConnPair<AppServerToApp<B>, AppToAppServer<B>>,
);

#[derive(Debug)]
pub enum AppServerError {
    FunderClosed,
    SpawnError,
    IndexClientClosed,
    SendToFunderError,
    SendToIndexClientError,
    AllAppsClosed,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum AppServerEvent<B: Clone> {
    IncomingConnection(IncomingAppConnection<B>),
    IncomingConnectionsClosed,
    FromFunder(FunderOutgoingControl<B>),
    FunderClosed,
    FromIndexClient(IndexClientToAppServer<B>),
    IndexClientClosed,
    FromApp((u128, Option<AppToAppServer<B>>)), // None means that app was closed
}

pub struct App<B: Clone> {
    permissions: AppPermissions,
    opt_sender: Option<mpsc::Sender<AppServerToApp<B>>>,
}

impl<B> App<B>
where
    B: Clone,
{
    pub fn new(permissions: AppPermissions, sender: mpsc::Sender<AppServerToApp<B>>) -> Self {
        App {
            permissions,
            opt_sender: Some(sender),
        }
    }

    pub async fn send(&mut self, message: AppServerToApp<B>) {
        if let Some(mut sender) = self.opt_sender.take() {
            if let Ok(()) = await!(sender.send(message)) {
                self.opt_sender = Some(sender);
            }
        }
    }
}

pub struct AppServer<B: Clone, TF, TIC, S> {
    to_funder: TF,
    to_index_client: TIC,
    from_app_sender: mpsc::Sender<(u128, Option<AppToAppServer<B>>)>,
    node_report: NodeReport<B>,
    incoming_connections_closed: bool,
    /// A long cyclic incrementing counter,
    /// allows to give every connection a unique number.
    /// Required because an app (with one public key) might have multiple connections.
    app_counter: u128,
    apps: HashMap<u128, App<B>>,
    /// Data structures to track ongoing requests.
    /// This allows us to multiplex requests/responses to multiple apps:
    route_requests: HashMap<Uid, u128>,
    close_payment_requests: HashMap<PaymentId, u128>,
    transactions: HashMap<Uid, u128>,
    spawner: S,
}

/// Check if we should process an app_request from an app with certain permissions
fn check_request_permissions<B>(
    app_permissions: &AppPermissions,
    app_request: &AppRequest<B>,
) -> bool {
    match app_request {
        AppRequest::AddRelay(_) => app_permissions.config,
        AppRequest::RemoveRelay(_) => app_permissions.config,
        AppRequest::CreatePayment(_) => app_permissions.buyer,
        AppRequest::CreateTransaction(_) => app_permissions.buyer,
        AppRequest::RequestClosePayment(_) => app_permissions.buyer,
        AppRequest::AckClosePayment(_) => app_permissions.buyer,

        AppRequest::AddInvoice(_) => app_permissions.seller,
        AppRequest::CancelInvoice(_) => app_permissions.seller,
        AppRequest::CommitInvoice(_) => app_permissions.seller,

        AppRequest::AddFriend(_) => app_permissions.config,
        AppRequest::SetFriendRelays(_) => app_permissions.config,
        AppRequest::SetFriendName(_) => app_permissions.config,
        AppRequest::RemoveFriend(_) => app_permissions.config,
        AppRequest::EnableFriend(_) => app_permissions.config,
        AppRequest::DisableFriend(_) => app_permissions.config,
        AppRequest::OpenFriend(_) => app_permissions.config,
        AppRequest::CloseFriend(_) => app_permissions.config,
        AppRequest::SetFriendRemoteMaxDebt(_) => app_permissions.config,
        AppRequest::SetFriendRate(_) => app_permissions.config,
        AppRequest::ResetFriendChannel(_) => app_permissions.config,
        AppRequest::RequestRoutes(_) => app_permissions.routes,
        AppRequest::AddIndexServer(_) => app_permissions.config,
        AppRequest::RemoveIndexServer(_) => app_permissions.config,
    }
}

impl<B, TF, TIC, S> AppServer<B, TF, TIC, S>
where
    B: Clone + PartialEq + Eq + Debug + Send + Sync + 'static,
    TF: Sink<FunderIncomingControl<B>> + Unpin + Sync + Send,
    TIC: Sink<AppServerToIndexClient<B>> + Unpin,
    S: Spawn,
{
    pub fn new(
        to_funder: TF,
        to_index_client: TIC,
        from_app_sender: mpsc::Sender<(u128, Option<AppToAppServer<B>>)>,
        node_report: NodeReport<B>,
        spawner: S,
    ) -> Self {
        AppServer {
            to_funder,
            to_index_client,
            from_app_sender,
            node_report,
            incoming_connections_closed: false,
            app_counter: 0,
            apps: HashMap::new(),
            route_requests: HashMap::new(),
            close_payment_requests: HashMap::new(),
            transactions: HashMap::new(),
            spawner,
        }
    }

    /// Add an application connection
    pub async fn handle_incoming_connection(
        &mut self,
        incoming_app_connection: IncomingAppConnection<B>,
    ) -> Result<(), AppServerError> {
        let (permissions, (sender, receiver)) = incoming_app_connection;

        let app_counter = self.app_counter;
        let mut receiver =
            receiver.map(move |app_to_app_server| (app_counter, Some(app_to_app_server)));

        let mut from_app_sender = self.from_app_sender.clone();
        let send_all_fut = async move {
            // Forward all messages:
            let _ = await!(from_app_sender.send_all(&mut receiver));
            // Notify that the connection to the app was closed:
            let _ = await!(from_app_sender.send((app_counter, None)));
        };

        self.spawner
            .spawn(send_all_fut)
            .map_err(|_| AppServerError::SpawnError)?;

        let mut app = App::new(permissions, sender);
        // Send the initial node report:
        await!(app.send(AppServerToApp::Report(self.node_report.clone())));

        self.apps.insert(self.app_counter, app);
        self.app_counter = self.app_counter.wrapping_add(1);

        Ok(())
    }

    /// The channel carrying new connections was closed.
    /// This means we will not receive any new connections
    pub async fn handle_incoming_connections_closed(&mut self) -> Result<(), AppServerError> {
        self.incoming_connections_closed = true;
        if self.apps.is_empty() {
            return Err(AppServerError::AllAppsClosed);
        }
        Ok(())
    }

    /// Send node report mutations to all connected apps
    pub async fn broadcast_node_report_mutations(&mut self, report_mutations: ReportMutations<B>) {
        // Send node report mutations to all connected apps
        for app in &mut self.apps.values_mut() {
            await!(app.send(AppServerToApp::ReportMutations(report_mutations.clone())));
        }
    }

    pub async fn handle_from_funder(
        &mut self,
        funder_message: FunderOutgoingControl<B>,
    ) -> Result<(), AppServerError> {
        match funder_message {
            FunderOutgoingControl::TransactionResult(transaction_result) => {
                // Find the app that issued the request, and forward the response to this app:
                let app_id = if let Some(app_id) =
                    self.transactions.remove(&transaction_result.request_id)
                {
                    app_id
                } else {
                    warn!("TransactionResult: Could not find app that initiated CreateTransaction");
                    return Ok(());
                };
                if let Some(app) = self.apps.get_mut(&app_id) {
                    await!(app.send(AppServerToApp::TransactionResult(
                        transaction_result.clone()
                    )));
                }
            }
            FunderOutgoingControl::ResponseClosePayment(response_close_payment) => {
                // Find the app that issued the request, and forward the response to this app:
                let app_id = if let Some(app_id) = self
                    .close_payment_requests
                    .remove(&response_close_payment.payment_id)
                {
                    app_id
                } else {
                    warn!("ResponseClosePayment: Could not find app that initiated RequestClosePayment");
                    return Ok(());
                };
                if let Some(app) = self.apps.get_mut(&app_id) {
                    await!(app.send(AppServerToApp::ResponseClosePayment(
                        response_close_payment.clone()
                    )));
                }
            }
            FunderOutgoingControl::ReportMutations(funder_report_mutations) => {
                let mut index_mutations = Vec::new();
                for funder_report_mutation in &funder_report_mutations.mutations {
                    // Transform the funder report mutation to index mutations
                    // and send it to IndexClient
                    let opt_index_mutation = funder_report_mutation_to_index_mutation(
                        &self.node_report.funder_report,
                        funder_report_mutation,
                    );

                    if let Some(index_mutation) = opt_index_mutation {
                        index_mutations.push(index_mutation);
                    }
                }

                // Send index mutations:
                if !index_mutations.is_empty() {
                    await!(self
                        .to_index_client
                        .send(AppServerToIndexClient::ApplyMutations(index_mutations)))
                    .map_err(|_| AppServerError::SendToIndexClientError)?;
                }

                let mut report_mutations = ReportMutations {
                    opt_app_request_id: funder_report_mutations.opt_app_request_id,
                    mutations: Vec::new(),
                };
                for funder_report_mutation in funder_report_mutations.mutations {
                    let mutation = NodeReportMutation::Funder(funder_report_mutation);
                    // Mutate our node report:
                    self.node_report.mutate(&mutation).unwrap();
                    report_mutations.mutations.push(mutation);
                }

                await!(self.broadcast_node_report_mutations(report_mutations));
            }
        }
        Ok(())
    }

    pub async fn handle_from_index_client(
        &mut self,
        index_client_message: IndexClientToAppServer<B>,
    ) -> Result<(), AppServerError> {
        match index_client_message {
            IndexClientToAppServer::ReportMutations(index_client_report_mutations) => {
                let mut report_mutations = ReportMutations {
                    opt_app_request_id: index_client_report_mutations.opt_app_request_id,
                    mutations: Vec::new(),
                };
                for index_client_report_mutation in index_client_report_mutations.mutations {
                    let mutation = NodeReportMutation::IndexClient(index_client_report_mutation);
                    // Mutate our node report:
                    self.node_report.mutate(&mutation).unwrap();
                    report_mutations.mutations.push(mutation);
                }

                await!(self.broadcast_node_report_mutations(report_mutations));
            }
            IndexClientToAppServer::ResponseRoutes(client_response_routes) => {
                // We search for the app that issued the request, and send it the response.
                let app_id = if let Some(app_id) = self
                    .route_requests
                    .remove(&client_response_routes.request_id)
                {
                    app_id
                } else {
                    warn!(
                        "ResponseRoutes: Could not find the app that issued RequestRoutes request"
                    );
                    return Ok(());
                };

                if let Some(app) = self.apps.get_mut(&app_id) {
                    await!(app.send(AppServerToApp::ResponseRoutes(
                        client_response_routes.clone()
                    )));
                }
            }
        };
        Ok(())
    }

    fn check_app_permissions(&self, app_id: u128, app_message: &AppToAppServer<B>) -> bool {
        // Get the relevant application:
        let app = match self.apps.get(&app_id) {
            Some(app) => app,
            None => {
                warn!("App {:?} does not exist!", app_id);
                return false;
            }
        };

        // Make sure this message is allowed for this application:
        if !check_request_permissions(&app.permissions, &app_message.app_request) {
            warn!(
                "App {:?} does not have permissions for {:?}",
                app_id, app_message
            );
            return false;
        }

        true
    }

    async fn handle_app_message(
        &mut self,
        app_id: u128,
        app_message: AppToAppServer<B>,
    ) -> Result<(), AppServerError> {
        if !self.check_app_permissions(app_id, &app_message) {
            return Ok(());
        }

        let app_request_id = app_message.app_request_id;
        match app_message.app_request {
            AppRequest::AddRelay(named_relay_address) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::AddRelay(named_relay_address)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::RemoveRelay(public_key) => await!(self.to_funder.send(
                FunderIncomingControl::new(app_request_id, FunderControl::RemoveRelay(public_key))
            ))
            .map_err(|_| AppServerError::SendToFunderError),
            AppRequest::CreatePayment(create_payment) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::CreatePayment(create_payment)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::CreateTransaction(create_transaction) => {
                // Keep track of which application issued this request:
                self.transactions
                    .insert(create_transaction.request_id, app_id);
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::CreateTransaction(create_transaction)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::RequestClosePayment(request_close_payment) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::RequestClosePayment(request_close_payment)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::AckClosePayment(ack_close_payment) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::AckClosePayment(ack_close_payment)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::AddInvoice(add_invoice) => await!(self.to_funder.send(
                FunderIncomingControl::new(app_request_id, FunderControl::AddInvoice(add_invoice))
            ))
            .map_err(|_| AppServerError::SendToFunderError),
            AppRequest::CancelInvoice(invoice_id) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::CancelInvoice(invoice_id)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::CommitInvoice(multi_commit) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::CommitInvoice(multi_commit)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::AddFriend(add_friend) => await!(self.to_funder.send(
                FunderIncomingControl::new(app_request_id, FunderControl::AddFriend(add_friend))
            ))
            .map_err(|_| AppServerError::SendToFunderError),
            AppRequest::SetFriendRelays(set_friend_address) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::SetFriendRelays(set_friend_address)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::SetFriendName(set_friend_name) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::SetFriendName(set_friend_name)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::RemoveFriend(friend_public_key) => {
                let remove_friend = RemoveFriend { friend_public_key };
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::RemoveFriend(remove_friend)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::EnableFriend(friend_public_key) => {
                let set_friend_status = SetFriendStatus {
                    friend_public_key,
                    status: FriendStatus::Enabled,
                };
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::SetFriendStatus(set_friend_status)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::DisableFriend(friend_public_key) => {
                let set_friend_status = SetFriendStatus {
                    friend_public_key,
                    status: FriendStatus::Disabled,
                };
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::SetFriendStatus(set_friend_status)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::OpenFriend(friend_public_key) => {
                let set_requests_status = SetRequestsStatus {
                    friend_public_key,
                    status: RequestsStatus::Open,
                };
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::SetRequestsStatus(set_requests_status)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::CloseFriend(friend_public_key) => {
                let set_requests_status = SetRequestsStatus {
                    friend_public_key,
                    status: RequestsStatus::Closed,
                };
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::SetRequestsStatus(set_requests_status)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::SetFriendRemoteMaxDebt(set_friend_remote_max_debt) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::SetFriendRemoteMaxDebt(set_friend_remote_max_debt)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::SetFriendRate(set_friend_rate) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::SetFriendRate(set_friend_rate)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::ResetFriendChannel(reset_friend_channel) => {
                await!(self.to_funder.send(FunderIncomingControl::new(
                    app_request_id,
                    FunderControl::ResetFriendChannel(reset_friend_channel)
                )))
                .map_err(|_| AppServerError::SendToFunderError)
            }
            AppRequest::RequestRoutes(request_routes) => {
                // Keep track of which application issued this request:
                if self
                    .route_requests
                    .insert(request_routes.request_id, app_id)
                    .is_some()
                {
                    warn!("RequestRoutes: request_id clash.");
                }
                await!(self
                    .to_index_client
                    .send(AppServerToIndexClient::AppRequest((
                        app_request_id,
                        IndexClientRequest::RequestRoutes(request_routes)
                    ))))
                .map_err(|_| AppServerError::SendToIndexClientError)
            }
            AppRequest::AddIndexServer(named_index_server_address) => await!(self
                .to_index_client
                .send(AppServerToIndexClient::AppRequest((
                    app_request_id,
                    IndexClientRequest::AddIndexServer(named_index_server_address)
                ))))
            .map_err(|_| AppServerError::SendToIndexClientError),
            AppRequest::RemoveIndexServer(index_server_address) => await!(self
                .to_index_client
                .send(AppServerToIndexClient::AppRequest((
                    app_request_id,
                    IndexClientRequest::RemoveIndexServer(index_server_address)
                ))))
            .map_err(|_| AppServerError::SendToIndexClientError),
        }
    }

    pub async fn handle_from_app(
        &mut self,
        app_id: u128,
        opt_app_message: Option<AppToAppServer<B>>,
    ) -> Result<(), AppServerError> {
        match opt_app_message {
            None => {
                // Remove the application. We assert that this application exists
                // in our apps map:
                self.apps.remove(&app_id).unwrap();
                if self.apps.is_empty() && self.incoming_connections_closed {
                    return Err(AppServerError::AllAppsClosed);
                }
                Ok(())
            }
            Some(app_message) => await!(self.handle_app_message(app_id, app_message)),
        }
    }
}

#[allow(unused)]
pub async fn app_server_loop<B, FF, TF, FIC, TIC, IC, S>(
    from_funder: FF,
    to_funder: TF,
    from_index_client: FIC,
    to_index_client: TIC,
    incoming_connections: IC,
    initial_node_report: NodeReport<B>,
    mut spawner: S,
) -> Result<(), AppServerError>
where
    B: Clone + PartialEq + Eq + Debug + Send + Sync + 'static,
    FF: Stream<Item = FunderOutgoingControl<B>> + Unpin + Send,
    TF: Sink<FunderIncomingControl<B>> + Unpin + Sync + Send,
    FIC: Stream<Item = IndexClientToAppServer<B>> + Unpin + Send,
    TIC: Sink<AppServerToIndexClient<B>> + Unpin,
    IC: Stream<Item = IncomingAppConnection<B>> + Unpin + Send,
    S: Spawn,
{
    let (from_app_sender, from_app_receiver) = mpsc::channel(0);
    let mut app_server = AppServer::new(
        to_funder,
        to_index_client,
        from_app_sender,
        initial_node_report,
        spawner,
    );

    let from_funder = from_funder
        .map(AppServerEvent::FromFunder)
        .chain(stream::once(future::ready(AppServerEvent::FunderClosed)));

    let from_index_client = from_index_client
        .map(AppServerEvent::FromIndexClient)
        .chain(stream::once(future::ready(
            AppServerEvent::IndexClientClosed,
        )));

    let from_app_receiver = from_app_receiver.map(AppServerEvent::FromApp);

    let incoming_connections = incoming_connections
        .map(AppServerEvent::IncomingConnection)
        .chain(stream::once(future::ready(
            AppServerEvent::IncomingConnectionsClosed,
        )));

    let mut events = select_streams![
        from_funder,
        from_index_client,
        from_app_receiver,
        incoming_connections
    ];

    while let Some(event) = await!(events.next()) {
        match event {
            AppServerEvent::IncomingConnection(incoming_app_connection) => {
                await!(app_server.handle_incoming_connection(incoming_app_connection))?
            }
            AppServerEvent::IncomingConnectionsClosed => {
                await!(app_server.handle_incoming_connections_closed())?
            }
            AppServerEvent::FromFunder(funder_outgoing_control) => {
                await!(app_server.handle_from_funder(funder_outgoing_control))?
            }
            AppServerEvent::FunderClosed => return Err(AppServerError::FunderClosed),
            AppServerEvent::FromIndexClient(from_index_client) => {
                await!(app_server.handle_from_index_client(from_index_client))?
            }
            AppServerEvent::IndexClientClosed => return Err(AppServerError::IndexClientClosed),
            AppServerEvent::FromApp((app_id, opt_app_message)) => {
                await!(app_server.handle_from_app(app_id, opt_app_message))?
            }
        }
    }
    Ok(())
}
