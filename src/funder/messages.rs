use futures::sync::oneshot;
use bytes::Bytes;

use crypto::identity::PublicKey;
use crypto::uid::Uid;

use proto::funder::{ChannelToken, InvoiceId};
use proto::common::SendFundsReceipt;
use channeler::types::ChannelerNeighborInfo;

#[derive(Clone)]
pub enum FriendStatus {
    Enable = 1,
    Disable = 0,
}

#[derive(Clone)]
pub enum RequestsStatus {
    Open,
    Closed,
}

pub struct FriendUpdated {
    balance: i128,
    local_max_debt: u128,
    remote_max_debt: u128,
    local_pending_debt: u128,
    remote_pending_debt: u128,
    requests_status: RequestsStatus,
    status: FriendStatus,
}

pub struct FriendInconsistent {
    current_token: ChannelToken,
    balance_for_reset: i128,
}

pub enum FriendEvent {
    FriendUpdated(FriendUpdated),
    FriendRemoved,
    FriendInconsistent(FriendInconsistent),
}


pub enum ResponseSendFunds {
    Success(SendFundsReceipt),
    Failure(PublicKey), // Reporting public key.
}

pub struct FriendStateUpdate {
    friend_public_key: PublicKey,
    event: FriendEvent,
}

// TODO: Can we merge this with FriendInfoFromDB
pub struct FriendInfo {
    friend_public_key: PublicKey,
    wanted_remote_max_debt: u128,
    status: FriendStatus,
}

pub struct PendingFriendRequest {
    pub request_id: Uid,
    // pub route: FriendsRouteWithCapacity, // TODO: Fill in later
    pub mediator_payment_proposal: u64,
    pub invoice_id: InvoiceId,
    pub destination_payment: u128,
}

// ======== Internal interface ========

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FriendsRoute {
    pub route_links: Vec<PublicKey>,
}

pub struct FriendsRouteWithCapacity {
    route: FriendsRoute,
    capacity: u128,
}

pub struct RequestSendFunds {
    // Note that it is the sender's responsibility to randomly generate a request_id.
    // This is important to make sure send funds requests can be tracked by the sending
    // application, and can not be lost.
    //
    // TODO: Rename request_id -> payment_id ?
    pub request_id: Uid,
    pub route: FriendsRoute,
    pub invoice_id: InvoiceId,
    pub payment: u128,
    pub response_sender: oneshot::Sender<ResponseSendFunds>,
}



pub enum FunderToDatabase {
    // TODO
}


pub enum FunderToChanneler<A> {
    /// Request send message to remote.
    SendChannelMessage {
        friend_public_key: PublicKey,
        content: Bytes,
    },
    /// Request to add a new friend.
    AddFriend {
        info: ChannelerNeighborInfo<A>,
    },
    /// Request to remove a friend.
    RemoveFriend {
        friend_public_key: PublicKey
    },
}


pub struct SetFriendRemoteMaxDebt {
    pub friend_public_key: PublicKey,
    pub remote_max_debt: u128,
}

pub struct ResetFriendChannel {
    pub friend_public_key: PublicKey,
    pub current_token: ChannelToken,
}


pub struct SetFriendAddr<A> {
    pub friend_public_key: PublicKey,
    pub address: Option<A>,
}


pub struct AddFriend<A> {
    pub friend_public_key: PublicKey,
    pub address: Option<A>,
}

pub struct RemoveFriend {
    pub friend_public_key: PublicKey,
}

pub struct SetFriendStatus {
    pub friend_public_key: PublicKey,
    pub status: FriendStatus,
}

pub struct OpenFriend {
    pub friend_public_key: PublicKey,
}

pub struct CloseFriend {
    pub friend_public_key: PublicKey,
}

pub enum FunderCommand<A> {
    AddFriend(AddFriend<A>),
    RemoveFriend(RemoveFriend),
    OpenFriend(OpenFriend),
    CloseFriend(CloseFriend),
    SetFriendStatus(SetFriendStatus),
    SetFriendRemoteMaxDebt(SetFriendRemoteMaxDebt),
    SetFriendAddr(SetFriendAddr<A>),
    ResetFriendChannel(ResetFriendChannel),
    RequestSendFunds(RequestSendFunds),
}
