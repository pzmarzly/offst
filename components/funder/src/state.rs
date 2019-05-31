use im::hashmap::HashMap as ImHashMap;
use im::vector::Vector as ImVec;

use common::canonical_serialize::CanonicalSerialize;
use crypto::hash_lock::{HashedLock, PlainLock};
use crypto::identity::PublicKey;
use crypto::invoice_id::InvoiceId;
use crypto::payment_id::PaymentId;
use crypto::uid::Uid;

use proto::app_server::messages::NamedRelayAddress;
use proto::funder::messages::{AddFriend, Receipt, ResponseSendFundsOp};

use crate::friend::{FriendMutation, FriendState};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct FunderState<B: Clone> {
    /// Public key of this node
    pub local_public_key: PublicKey,
    /// Addresses of relays we are going to connect to.
    pub relays: ImVec<NamedRelayAddress<B>>,
    /// All configured friends and their state
    pub friends: ImHashMap<PublicKey, FriendState<B>>,
    /// Locally issued invoices in progress (For which this node is the seller)
    pub open_invoices: ImHashMap<InvoiceId, OpenInvoice>,
    /// Locally created transaction in progress. (For which this node is the buyer).
    pub open_transactions: ImHashMap<Uid, OpenTransaction>,
    /// Ongoing payments (For which this node is the buyer):
    pub payments: ImHashMap<PaymentId, Payment>,
}

/// A state of a Payment where new transactions may still be added.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct NewTransactions {
    pub num_transactions: u64,
    pub invoice_id: InvoiceId,
    pub total_dest_payment: u128,
    pub dest_public_key: PublicKey,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum Payment {
    /// User can add new transactions
    // TODO: Think about a better name for this?
    NewTransactions(NewTransactions),
    /// User can no longer add new transactions (user sent a RequestClosePayment)
    InProgress(u64), // num_transactions
    /// A receipt was received:
    Success((u64, Receipt, Uid)), // (num_transactions, Receipt, ack_uid)
    /// The payment will not complete, because all transactions were canceled:
    Canceled(Uid), // ack_uid
    /// User already acked, We now wait for the remaining transactions to finish.
    AfterSuccessAck(u64), // num_transactions
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct IncomingTransaction {
    pub request_id: Uid,
    /// The lock we used on our ResponseSendFundsOp message.
    /// We have to keep it, otherwise we will not be able to send a valid CollectSendFundsOp later.
    pub dest_plain_lock: PlainLock,
}

/// A local invoice in progress
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct OpenInvoice {
    /// Total payment required to fulfill this invoice:
    pub total_dest_payment: u128,
    /// Multiple transactions are possible for a single invoice in case of a multi-route payment.
    pub incoming_transactions: ImHashMap<HashedLock, IncomingTransaction>,
}

impl OpenInvoice {
    pub fn new(total_dest_payment: u128) -> Self {
        OpenInvoice {
            total_dest_payment,
            incoming_transactions: ImHashMap::new(),
        }
    }
}

/// A local request (Originated from this node) in progress
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct OpenTransaction {
    pub payment_id: PaymentId,
    /// The plain part of a hash lock for the generated transaction.
    pub src_plain_lock: PlainLock,
    /// A response (if we got one):
    pub opt_response: Option<ResponseSendFundsOp>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FunderMutation<B: Clone> {
    FriendMutation((PublicKey, FriendMutation<B>)),
    AddRelay(NamedRelayAddress<B>),
    RemoveRelay(PublicKey),
    AddFriend(AddFriend<B>),
    RemoveFriend(PublicKey),
    AddInvoice((InvoiceId, u128)), // (InvoiceId, total_dest_payment)
    AddIncomingTransaction((InvoiceId, Uid, PlainLock)), // (invoice_id, request_id, dest_plain_lock)
    RemoveInvoice(InvoiceId),
    AddTransaction((Uid, PaymentId, PlainLock)), // (request_id, payment_id,src_plain_lock)
    SetTransactionResponse(ResponseSendFundsOp), // (request_id, response_send_funds)
    RemoveTransaction(Uid),                      // request_id
    UpdatePayment((PaymentId, Payment)),
    RemovePayment(PaymentId),
}

impl<B> FunderState<B>
where
    B: Clone + CanonicalSerialize,
{
    pub fn new(local_public_key: PublicKey, relays: Vec<NamedRelayAddress<B>>) -> Self {
        // Convert relays into a map:
        let relays = relays.into_iter().collect();

        FunderState {
            local_public_key,
            relays,
            friends: ImHashMap::new(),
            open_invoices: ImHashMap::new(),
            open_transactions: ImHashMap::new(),
            payments: ImHashMap::new(),
        }
    }

    // TODO: Use MutableState trait instead:
    pub fn mutate(&mut self, funder_mutation: &FunderMutation<B>) {
        match funder_mutation {
            FunderMutation::FriendMutation((public_key, friend_mutation)) => {
                let friend = self.friends.get_mut(&public_key).unwrap();
                friend.mutate(friend_mutation);
            }
            FunderMutation::AddRelay(named_relay_address) => {
                // Check for duplicates:
                self.relays.retain(|cur_named_relay_address| {
                    cur_named_relay_address.public_key != named_relay_address.public_key
                });
                self.relays.push_back(named_relay_address.clone());
                // TODO: Should check here if we have more than a constant amount of relays
            }
            FunderMutation::RemoveRelay(public_key) => {
                self.relays.retain(|cur_named_relay_address| {
                    &cur_named_relay_address.public_key != public_key
                });
            }
            FunderMutation::AddFriend(add_friend) => {
                let friend = FriendState::new(
                    &self.local_public_key,
                    &add_friend.friend_public_key,
                    add_friend.relays.clone(),
                    add_friend.name.clone(),
                    add_friend.balance,
                );
                // Insert friend, but also make sure that we didn't override an existing friend
                // with the same public key:
                let res = self
                    .friends
                    .insert(add_friend.friend_public_key.clone(), friend);
                assert!(res.is_none());
            }
            FunderMutation::RemoveFriend(public_key) => {
                let _ = self.friends.remove(&public_key);
            }
            FunderMutation::AddInvoice((invoice_id, total_dest_payment)) => {
                self.open_invoices
                    .insert(invoice_id.clone(), OpenInvoice::new(*total_dest_payment));
            }
            FunderMutation::AddIncomingTransaction((invoice_id, request_id, dest_plain_lock)) => {
                let open_invoice = self.open_invoices.get_mut(invoice_id).unwrap();
                let incoming_transaction = IncomingTransaction {
                    request_id: *request_id,
                    dest_plain_lock: dest_plain_lock.clone(),
                };
                open_invoice
                    .incoming_transactions
                    .insert(dest_plain_lock.hash().clone(), incoming_transaction);
            }
            FunderMutation::RemoveInvoice(invoice_id) => {
                let _ = self.open_invoices.remove(invoice_id);
            }
            FunderMutation::AddTransaction((request_id, payment_id, src_plain_lock)) => {
                let open_transaction = OpenTransaction {
                    payment_id: *payment_id,
                    src_plain_lock: src_plain_lock.clone(),
                    opt_response: None,
                };
                let _ = self
                    .open_transactions
                    .insert(request_id.clone(), open_transaction);
            }
            FunderMutation::SetTransactionResponse(response_send_funds) => {
                let open_transaction = self
                    .open_transactions
                    .get_mut(&response_send_funds.request_id)
                    .unwrap();
                // We assert that no response was received so far:
                assert!(open_transaction.opt_response.take().is_none());
                open_transaction.opt_response = Some(response_send_funds.clone());
            }
            FunderMutation::RemoveTransaction(request_id) => {
                let _ = self.open_transactions.remove(request_id);
            }
            FunderMutation::UpdatePayment((payment_id, payment)) => {
                let _ = self.payments.insert(payment_id.clone(), payment.clone());
            }
            FunderMutation::RemovePayment(payment_id) => {
                let _ = self.payments.remove(payment_id);
            }
        }
    }
}
