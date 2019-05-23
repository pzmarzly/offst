use im::hashmap::HashMap as ImHashMap;
use im::vector::Vector as ImVec;

use common::canonical_serialize::CanonicalSerialize;
use crypto::hash_lock::PlainLock;
use crypto::identity::PublicKey;
use crypto::invoice_id::InvoiceId;
use crypto::payment_id::PaymentId;
use crypto::uid::Uid;

use proto::app_server::messages::NamedRelayAddress;
use proto::funder::messages::{AddFriend, Receipt};

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

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct NewTransactions {
    pub num_transactions: u64,
    pub invoice_id: InvoiceId,
    pub total_dest_payment: u128,
    pub dest_public_key: PublicKey,
}

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

/// A local invoice in progress
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct OpenInvoice {
    /// Total payment required to fulfill this invoice:
    pub total_dest_payment: u128,
    /// Destination plain locks for all requests related to a single open invoice that was
    /// originated for this node.
    /// Multiple requests are possible for a single invoice in case of a multi-route payment.
    pub dest_plain_locks: ImHashMap<Uid, PlainLock>,
}

impl OpenInvoice {
    pub fn new(total_dest_payment: u128) -> Self {
        OpenInvoice {
            total_dest_payment,
            dest_plain_locks: ImHashMap::new(),
        }
    }
}

/// A local request (Originated from this node) in progress
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct OpenTransaction {
    pub payment_id: PaymentId,
    /// The plain part of a hash lock for the generated transaction.
    pub src_plain_lock: PlainLock,
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
    AddDestPlainLock((InvoiceId, Uid, PlainLock)), // InvoiceId, RequestId, dest_plain_lock
    RemoveInvoice(InvoiceId),
    AddTransaction((Uid, PaymentId, PlainLock)), // (transaction_id, payment_id,src_plain_lock)
    RemoveTransaction(Uid),                      // transaction_id
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
            FunderMutation::AddDestPlainLock((invoice_id, request_id, plain_lock)) => {
                let open_invoice = self.open_invoices.get_mut(invoice_id).unwrap();
                open_invoice
                    .dest_plain_locks
                    .insert(request_id.clone(), plain_lock.clone());
            }
            FunderMutation::RemoveInvoice(invoice_id) => {
                let _ = self.open_invoices.remove(invoice_id);
            }
            FunderMutation::AddTransaction((transaction_id, payment_id, src_plain_lock)) => {
                let open_transaction = OpenTransaction {
                    payment_id: payment_id.clone(),
                    src_plain_lock: src_plain_lock.clone(),
                };
                let _ = self
                    .open_transactions
                    .insert(transaction_id.clone(), open_transaction);
            }
            FunderMutation::RemoveTransaction(transaction_id) => {
                let _ = self.open_transactions.remove(transaction_id);
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
