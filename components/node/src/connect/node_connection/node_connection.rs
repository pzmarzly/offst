use futures::channel::mpsc;
use futures::task::{Spawn, SpawnExt};
use futures::{FutureExt, SinkExt, StreamExt, TryFutureExt};

use proto::app_server::messages::{AppPermissions, AppServerToApp, AppToAppServer, NodeReport};

use crypto::crypto_rand::{CryptoRandom, OffstSystemRandom};

use common::conn::ConnPair;
use common::multi_consumer::{multi_consumer_service, MultiConsumerClient};
use common::mutable_state::BatchMutable;
use common::state_service::{state_service, StateClient};

use super::buyer::AppBuyer;
use super::config::AppConfig;
use super::report::AppReport;
use super::routes::AppRoutes;
use super::seller::AppSeller;

pub type NodeConnectionTuple = (
    AppPermissions,
    NodeReport,
    ConnPair<AppToAppServer, AppServerToApp>,
);

#[derive(Debug)]
pub enum NodeConnectionError {
    SpawnError,
}

// TODO: Do we need a way to close this connection?
// Is it closed on Drop?
#[derive(Clone)]
pub struct NodeConnection<R = OffstSystemRandom> {
    report: AppReport,
    opt_config: Option<AppConfig<R>>,
    opt_routes: Option<AppRoutes<R>>,
    opt_buyer: Option<AppBuyer<R>>,
    opt_seller: Option<AppSeller<R>>,
    rng: R,
}

impl<R> NodeConnection<R>
where
    R: CryptoRandom + Clone,
{
    pub fn new<S>(
        conn_tuple: NodeConnectionTuple,
        rng: R,
        spawner: &mut S,
    ) -> Result<Self, NodeConnectionError>
    where
        S: Spawn,
    {
        let (app_permissions, node_report, (sender, mut receiver)) = conn_tuple;

        let (mut incoming_mutations_sender, incoming_mutations) = mpsc::channel(0);
        let (requests_sender, incoming_requests) = mpsc::channel(0);
        let report_client = StateClient::new(requests_sender);
        let state_service_fut = state_service(
            incoming_requests,
            BatchMutable(node_report),
            incoming_mutations,
        )
        .map_err(|e| error!("state_service() error: {:?}", e))
        .map(|_| ());
        spawner
            .spawn(state_service_fut)
            .map_err(|_| NodeConnectionError::SpawnError)?;

        let (mut incoming_routes_sender, incoming_routes) = mpsc::channel(0);
        let (requests_sender, incoming_requests) = mpsc::channel(0);
        let routes_mc = MultiConsumerClient::new(requests_sender);
        let routes_fut = multi_consumer_service(incoming_routes, incoming_requests)
            .map_err(|e| error!("Routes multi_consumer_service() error: {:?}", e))
            .map(|_| ());
        spawner
            .spawn(routes_fut)
            .map_err(|_| NodeConnectionError::SpawnError)?;

        let (mut incoming_transaction_results_sender, incoming_transaction_results) =
            mpsc::channel(0);
        let (requests_sender, incoming_requests) = mpsc::channel(0);
        let transaction_results_mc = MultiConsumerClient::new(requests_sender);
        let transaction_results_fut =
            multi_consumer_service(incoming_transaction_results, incoming_requests)
                .map_err(|e| error!("Buyer multi_consumer_service() error: {:?}", e))
                .map(|_| ());
        spawner
            .spawn(transaction_results_fut)
            .map_err(|_| NodeConnectionError::SpawnError)?;

        let (mut incoming_response_close_payments_sender, incoming_response_close_payments) =
            mpsc::channel(0);
        let (requests_sender, incoming_requests) = mpsc::channel(0);
        let response_close_payments_mc = MultiConsumerClient::new(requests_sender);
        let response_close_payments_fut =
            multi_consumer_service(incoming_response_close_payments, incoming_requests)
                .map_err(|e| error!("Buyer multi_consumer_service() error: {:?}", e))
                .map(|_| ());
        spawner
            .spawn(response_close_payments_fut)
            .map_err(|_| NodeConnectionError::SpawnError)?;

        let (mut incoming_done_app_requests_sender, incoming_done_app_requests) = mpsc::channel(0);
        let (requests_sender, incoming_requests) = mpsc::channel(0);
        let done_app_requests_mc = MultiConsumerClient::new(requests_sender);
        let done_app_requests_fut =
            multi_consumer_service(incoming_done_app_requests, incoming_requests)
                .map_err(|e| error!("DoneAppRequests multi_consumer_service() error: {:?}", e))
                .map(|_| ());
        spawner
            .spawn(done_app_requests_fut)
            .map_err(|_| NodeConnectionError::SpawnError)?;

        spawner
            .spawn(async move {
                while let Some(message) = await!(receiver.next()) {
                    match message {
                        AppServerToApp::TransactionResult(transaction_result) => {
                            let _ = await!(
                                incoming_transaction_results_sender.send(transaction_result)
                            );
                        }
                        AppServerToApp::ResponseClosePayment(response_close_payment) => {
                            let _ = await!(incoming_response_close_payments_sender
                                .send(response_close_payment));
                        }
                        AppServerToApp::Report(_node_report) => {
                            // TODO: Maybe somehow redesign the type AppServerToApp
                            // so that we don't have this edge case?
                            error!("Received unexpected AppServerToApp::Report message. Aborting.");
                            return;
                        }
                        AppServerToApp::ReportMutations(node_report_mutations) => {
                            let _ = await!(
                                incoming_mutations_sender.send(node_report_mutations.mutations)
                            );
                            if let Some(app_request_id) = node_report_mutations.opt_app_request_id {
                                let _ =
                                    await!(incoming_done_app_requests_sender.send(app_request_id));
                            }
                        }
                        AppServerToApp::ResponseRoutes(client_response_routes) => {
                            let _ = await!(incoming_routes_sender.send(client_response_routes));
                        }
                    }
                }
            })
            .map_err(|_| NodeConnectionError::SpawnError)?;

        let opt_config = if app_permissions.config {
            Some(AppConfig::new(
                sender.clone(),
                done_app_requests_mc.clone(),
                rng.clone(),
            ))
        } else {
            None
        };

        let opt_routes = if app_permissions.routes {
            Some(AppRoutes::new(
                sender.clone(),
                routes_mc.clone(),
                rng.clone(),
            ))
        } else {
            None
        };

        let opt_buyer = if app_permissions.buyer {
            Some(AppBuyer::new(
                sender.clone(),
                transaction_results_mc.clone(),
                response_close_payments_mc.clone(),
                done_app_requests_mc.clone(),
                rng.clone(),
            ))
        } else {
            None
        };

        let opt_seller = if app_permissions.seller {
            Some(AppSeller::new(
                sender.clone(),
                done_app_requests_mc.clone(),
                rng.clone(),
            ))
        } else {
            None
        };

        Ok(NodeConnection {
            report: AppReport::new(report_client.clone()),
            opt_config,
            opt_routes,
            opt_buyer,
            opt_seller,
            rng,
        })
    }

    pub fn report(&mut self) -> &mut AppReport {
        &mut self.report
    }

    pub fn config(&mut self) -> Option<&mut AppConfig<R>> {
        self.opt_config.as_mut()
    }

    pub fn routes(&mut self) -> Option<&mut AppRoutes<R>> {
        self.opt_routes.as_mut()
    }

    pub fn buyer(&mut self) -> Option<&mut AppBuyer<R>> {
        self.opt_buyer.as_mut()
    }

    pub fn seller(&mut self) -> Option<&mut AppSeller<R>> {
        self.opt_seller.as_mut()
    }
}
