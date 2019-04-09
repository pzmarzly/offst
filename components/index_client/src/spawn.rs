use std::fmt::Debug;

use futures::channel::mpsc;
use futures::task::{Spawn, SpawnExt};
use futures::{Future, SinkExt, StreamExt};

use common::conn::{BoxFuture, ConnPairVec, FutTransform};
use database::DatabaseClient;
use identity::IdentityClient;
use timer::TimerClient;

use crypto::crypto_rand::CryptoRandom;
use crypto::identity::PublicKey;

use proto::index_client::messages::{
    AppServerToIndexClient, IndexClientState, IndexClientToAppServer,
};

use proto::index_server::messages::IndexServerAddress;
use proto::index_server::serialize::{
    deserialize_index_server_to_client, serialize_index_client_to_server,
};

use crate::client_session::IndexClientSession;
use crate::index_client::{
    index_client_loop, IndexClientConfig, IndexClientConfigMutation, IndexClientError,
};
use crate::seq_friends::create_seq_friends_service;
use crate::seq_map::SeqMap;
use crate::single_client::ServerConn;

#[derive(Clone)]
/// Connect to an index server
pub struct SerdeClientConnector<C, S> {
    net_connector: C,
    spawner: S,
}

impl<C, S> SerdeClientConnector<C, S> {
    pub fn new(net_connector: C, spawner: S) -> Self {
        SerdeClientConnector {
            net_connector,
            spawner,
        }
    }
}

impl<ISA, C, S> FutTransform for SerdeClientConnector<C, S>
where
    ISA: Send + 'static,
    C: FutTransform<Input = IndexServerAddress<ISA>, Output = Option<ConnPairVec>> + Clone + Send,
    S: Spawn + Send,
{
    type Input = IndexServerAddress<ISA>;
    type Output = Option<ServerConn>;

    fn transform(&mut self, index_server: Self::Input) -> BoxFuture<'_, Self::Output> {
        Box::pin(
            async move {
                // This line performs connection and then handshake:
                let (mut data_sender, mut data_receiver) =
                    await!(self.net_connector.transform(index_server))?;

                let (user_sender, mut local_receiver) = mpsc::channel(0);
                let (mut local_sender, user_receiver) = mpsc::channel(0);

                // Deserialize incoming data:
                let deser_fut = async move {
                    while let Some(data) = await!(data_receiver.next()) {
                        let message = match deserialize_index_server_to_client(&data) {
                            Ok(message) => message,
                            Err(_) => {
                                error!("deserialize index_server_to_client error");
                                return;
                            }
                        };
                        if let Err(e) = await!(local_sender.send(message)) {
                            error!("error sending to local_sender: {:?}", e);
                            return;
                        }
                    }
                };
                // If there is any error here, the user will find out when
                // he tries to read from `user_receiver`
                let _ = self.spawner.spawn(deser_fut);

                // Serialize outgoing data:
                let ser_fut = async move {
                    while let Some(message) = await!(local_receiver.next()) {
                        let data = serialize_index_client_to_server(&message);
                        if let Err(e) = await!(data_sender.send(data)) {
                            error!("error sending to data_sender: {:?}", e);
                            return;
                        }
                    }
                };
                // If there is any error here, the user will find out when
                // he tries to send through `user_sender`
                let _ = self.spawner.spawn(ser_fut);

                Some((user_sender, user_receiver))
            },
        )
    }
}

#[derive(Debug)]
pub enum SpawnIndexClientError {
    RequestTimerStreamError,
    SpawnError,
}

pub async fn spawn_index_client<ISA, C, R, S>(
    local_public_key: PublicKey,
    index_client_config: IndexClientConfig<ISA>,
    index_client_state: IndexClientState,
    identity_client: IdentityClient,
    mut timer_client: TimerClient,
    database_client: DatabaseClient<IndexClientConfigMutation<ISA>>,
    from_app_server: mpsc::Receiver<AppServerToIndexClient<ISA>>,
    to_app_server: mpsc::Sender<IndexClientToAppServer<ISA>>,
    max_open_index_client_requests: usize,
    keepalive_ticks: usize,
    backoff_ticks: usize,
    net_connector: C,
    rng: R,
    mut spawner: S,
) -> Result<impl Future<Output = Result<(), IndexClientError>>, SpawnIndexClientError>
where
    ISA: Debug + Eq + Clone + Send + 'static,
    C: FutTransform<Input = IndexServerAddress<ISA>, Output = Option<ConnPairVec>>
        + Clone
        + Send
        + Sync
        + 'static,
    R: CryptoRandom + Clone + Send + 'static,
    S: Spawn + Clone + Send + Sync + 'static,
{
    let timer_stream = await!(timer_client.request_timer_stream())
        .map_err(|_| SpawnIndexClientError::RequestTimerStreamError)?;

    let seq_friends = SeqMap::new(index_client_state.friends);
    let seq_friends_client = create_seq_friends_service(seq_friends, spawner.clone())
        .map_err(|_| SpawnIndexClientError::SpawnError)?;

    let serde_client_connector = SerdeClientConnector::new(net_connector, spawner.clone());

    let index_client_session = IndexClientSession::new(
        serde_client_connector,
        local_public_key,
        identity_client,
        rng,
        spawner.clone(),
    );

    let index_client_fut = index_client_loop(
        from_app_server,
        to_app_server,
        index_client_config,
        seq_friends_client,
        index_client_session,
        max_open_index_client_requests,
        keepalive_ticks,
        backoff_ticks,
        database_client,
        timer_stream,
        spawner.clone(),
    );

    spawner
        .spawn_with_handle(index_client_fut)
        .map_err(|_| SpawnIndexClientError::SpawnError)
}
