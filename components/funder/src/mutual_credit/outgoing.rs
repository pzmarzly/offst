use crypto::identity::verify_signature;

use common::safe_arithmetic::SafeSignedArithmetic;

use proto::funder::messages::{
    CancelSendFundsOp, CollectSendFundsOp, FriendTcOp, RequestSendFundsOp, RequestsStatus,
    ResponseSendFundsOp, TransactionStage,
};
use proto::funder::signature_buff::create_response_signature_buffer;

use crate::types::create_pending_transaction;

use super::types::{McMutation, MutualCredit, MAX_FUNDER_DEBT};

/// Processes outgoing funds for a token channel.
/// Used to batch as many funds as possible.
pub struct OutgoingMc {
    mutual_credit: MutualCredit,
}

#[derive(Debug)]
pub enum QueueOperationError {
    RemoteMaxDebtTooLarge,
    InvalidRoute,
    PkPairNotInRoute,
    CreditsCalcOverflow,
    InsufficientTrust,
    RequestAlreadyExists,
    RequestDoesNotExist,
    InvalidResponseSignature,
    RemoteRequestsClosed,
    NotExpectingResponse,
    NotExpectingCollect,
    InvalidSrcPlainLock,
    InvalidDestPlainLock,
    DestPaymentExceedsTotal,
}

/// A wrapper over a token channel, accumulating funds to be sent as one transaction.
impl OutgoingMc {
    pub fn new(mutual_credit: &MutualCredit) -> OutgoingMc {
        OutgoingMc {
            mutual_credit: mutual_credit.clone(),
        }
    }

    pub fn queue_operation(
        &mut self,
        operation: &FriendTcOp,
    ) -> Result<Vec<McMutation>, QueueOperationError> {
        // TODO: Maybe remove clone from here later:
        match operation.clone() {
            FriendTcOp::EnableRequests => self.queue_enable_requests(),
            FriendTcOp::DisableRequests => self.queue_disable_requests(),
            FriendTcOp::SetRemoteMaxDebt(proposed_max_debt) => {
                self.queue_set_remote_max_debt(proposed_max_debt)
            }
            FriendTcOp::RequestSendFunds(request_send_funds) => {
                self.queue_request_send_funds(request_send_funds)
            }
            FriendTcOp::ResponseSendFunds(response_send_funds) => {
                self.queue_response_send_funds(response_send_funds)
            }
            FriendTcOp::CancelSendFunds(cancel_send_funds) => {
                self.queue_cancel_send_funds(cancel_send_funds)
            }
            FriendTcOp::CollectSendFunds(collect_send_funds) => {
                self.queue_collect_send_funds(collect_send_funds)
            }
        }
    }

    fn queue_enable_requests(&mut self) -> Result<Vec<McMutation>, QueueOperationError> {
        // TODO: Should we check first if local requests are already open?
        let mut mc_mutations = Vec::new();
        let mc_mutation = McMutation::SetLocalRequestsStatus(RequestsStatus::Open);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        Ok(mc_mutations)
    }

    fn queue_disable_requests(&mut self) -> Result<Vec<McMutation>, QueueOperationError> {
        let mut mc_mutations = Vec::new();
        let mc_mutation = McMutation::SetLocalRequestsStatus(RequestsStatus::Closed);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        Ok(mc_mutations)
    }

    fn queue_set_remote_max_debt(
        &mut self,
        proposed_max_debt: u128,
    ) -> Result<Vec<McMutation>, QueueOperationError> {
        if proposed_max_debt > MAX_FUNDER_DEBT {
            return Err(QueueOperationError::RemoteMaxDebtTooLarge);
        }

        let mut mc_mutations = Vec::new();
        let mc_mutation = McMutation::SetRemoteMaxDebt(proposed_max_debt);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);
        Ok(mc_mutations)
    }

    fn queue_request_send_funds(
        &mut self,
        request_send_funds: RequestSendFundsOp,
    ) -> Result<Vec<McMutation>, QueueOperationError> {
        if !request_send_funds.route.is_valid() {
            return Err(QueueOperationError::InvalidRoute);
        }

        if request_send_funds.dest_payment > request_send_funds.total_dest_payment {
            return Err(QueueOperationError::DestPaymentExceedsTotal);
        }

        // Find ourselves on the route. If we are not there, abort.
        let _local_index = request_send_funds
            .route
            .find_pk_pair(
                &self.mutual_credit.state().idents.local_public_key,
                &self.mutual_credit.state().idents.remote_public_key,
            )
            .ok_or(QueueOperationError::PkPairNotInRoute)?;

        // Make sure that remote side is open to requests:
        if !self.mutual_credit.state().requests_status.remote.is_open() {
            return Err(QueueOperationError::RemoteRequestsClosed);
        }

        // Calculate amount of credits to freeze
        let own_freeze_credits = request_send_funds
            .dest_payment
            .checked_add(request_send_funds.left_fees)
            .ok_or(QueueOperationError::CreditsCalcOverflow)?;

        let balance = &self.mutual_credit.state().balance;

        // Make sure we can freeze the credits
        let new_local_pending_debt = balance
            .local_pending_debt
            .checked_add(own_freeze_credits)
            .ok_or(QueueOperationError::CreditsCalcOverflow)?;

        // Check that local_pending_debt - balance <= local_max_debt:
        let sub = balance
            .balance
            .checked_sub_unsigned(new_local_pending_debt)
            .ok_or(QueueOperationError::CreditsCalcOverflow)?;

        if sub
            .checked_add_unsigned(balance.local_max_debt)
            .ok_or(QueueOperationError::CreditsCalcOverflow)?
            < 0
        {
            return Err(QueueOperationError::InsufficientTrust);
        }

        let p_local_requests = &self.mutual_credit.state().pending_transactions.local;

        // Make sure that we don't have this request as a pending request already:
        if p_local_requests.contains_key(&request_send_funds.request_id) {
            return Err(QueueOperationError::RequestAlreadyExists);
        }

        // Add pending transaction:
        let pending_transaction = create_pending_transaction(&request_send_funds);

        let mut mc_mutations = Vec::new();
        let mc_mutation = McMutation::InsertLocalPendingTransaction(pending_transaction);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        // If we are here, we can freeze the credits:
        let mc_mutation = McMutation::SetLocalPendingDebt(new_local_pending_debt);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        Ok(mc_mutations)
    }

    fn queue_response_send_funds(
        &mut self,
        response_send_funds: ResponseSendFundsOp,
    ) -> Result<Vec<McMutation>, QueueOperationError> {
        // Make sure that id exists in remote_pending hashmap,
        // and access saved request details.
        let remote_pending_transactions = &self.mutual_credit.state().pending_transactions.remote;

        // Obtain pending request:
        let pending_transaction = remote_pending_transactions
            .get(&response_send_funds.request_id)
            .ok_or(QueueOperationError::RequestDoesNotExist)?
            .clone();
        // TODO: Possibly get rid of clone() here for optimization later

        // verify signature:
        let response_signature_buffer =
            create_response_signature_buffer(&response_send_funds, &pending_transaction);
        // The response was signed by the destination node:
        let dest_public_key = pending_transaction.route.public_keys.last().unwrap();

        // Verify response funds signature:
        if !verify_signature(
            &response_signature_buffer,
            dest_public_key,
            &response_send_funds.signature,
        ) {
            return Err(QueueOperationError::InvalidResponseSignature);
        }

        // We expect that the current stage is Request:
        if let TransactionStage::Request = pending_transaction.stage {
        } else {
            return Err(QueueOperationError::NotExpectingResponse);
        }

        let mut mc_mutations = Vec::new();

        // Set the stage to Response, and remember dest_hashed_lock:
        let mc_mutation = McMutation::SetRemotePendingTransactionStage((
            response_send_funds.request_id,
            TransactionStage::Response(response_send_funds.dest_hashed_lock.clone()),
        ));
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        Ok(mc_mutations)
    }

    fn queue_cancel_send_funds(
        &mut self,
        cancel_send_funds: CancelSendFundsOp,
    ) -> Result<Vec<McMutation>, QueueOperationError> {
        // Make sure that id exists in remote_pending hashmap,
        // and access saved request details.
        let remote_pending_transactions = &self.mutual_credit.state().pending_transactions.remote;

        // Obtain pending request:
        let pending_transaction = remote_pending_transactions
            .get(&cancel_send_funds.request_id)
            .ok_or(QueueOperationError::RequestDoesNotExist)?;

        let freeze_credits = pending_transaction
            .dest_payment
            .checked_add(pending_transaction.left_fees)
            .unwrap();

        // Remove entry from remote hashmap:
        let mut mc_mutations = Vec::new();

        let mc_mutation = McMutation::RemoveRemotePendingTransaction(cancel_send_funds.request_id);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        // Decrease frozen credits:
        let new_remote_pending_debt = self
            .mutual_credit
            .state()
            .balance
            .remote_pending_debt
            .checked_sub(freeze_credits)
            .unwrap();

        let mc_mutation = McMutation::SetRemotePendingDebt(new_remote_pending_debt);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        Ok(mc_mutations)
    }

    fn queue_collect_send_funds(
        &mut self,
        collect_send_funds: CollectSendFundsOp,
    ) -> Result<Vec<McMutation>, QueueOperationError> {
        // Make sure that id exists in remote_pending hashmap,
        // and access saved request details.
        let remote_pending_transactions = &self.mutual_credit.state().pending_transactions.remote;

        // Obtain pending request:
        let pending_transaction = remote_pending_transactions
            .get(&collect_send_funds.request_id)
            .ok_or(QueueOperationError::RequestDoesNotExist)?
            .clone();
        // TODO: Possibly get rid of clone() here for optimization later

        let dest_hashed_lock = match &pending_transaction.stage {
            TransactionStage::Response(dest_hashed_lock) => dest_hashed_lock,
            _ => return Err(QueueOperationError::NotExpectingCollect),
        };

        // Verify src_plain_lock and dest_plain_lock:
        if collect_send_funds.src_plain_lock.hash() != pending_transaction.src_hashed_lock {
            return Err(QueueOperationError::InvalidSrcPlainLock);
        }

        if collect_send_funds.dest_plain_lock.hash() != *dest_hashed_lock {
            return Err(QueueOperationError::InvalidDestPlainLock);
        }

        // Calculate amount of credits that were frozen:
        let freeze_credits = pending_transaction
            .dest_payment
            .checked_add(pending_transaction.left_fees)
            .unwrap();

        // Remove entry from remote_pending hashmap:
        let mut mc_mutations = Vec::new();
        let mc_mutation = McMutation::RemoveRemotePendingTransaction(collect_send_funds.request_id);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        // Decrease frozen credits and increase balance:
        let new_remote_pending_debt = self
            .mutual_credit
            .state()
            .balance
            .remote_pending_debt
            .checked_sub(freeze_credits)
            .unwrap();
        // Above unwrap() should never fail. This was already checked when a request message was
        // received.

        let mc_mutation = McMutation::SetRemotePendingDebt(new_remote_pending_debt);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        let new_balance = self
            .mutual_credit
            .state()
            .balance
            .balance
            .checked_add_unsigned(freeze_credits)
            .unwrap();
        // Above unwrap() should never fail. This was already checked when a request message was
        // received.

        let mc_mutation = McMutation::SetBalance(new_balance);
        self.mutual_credit.mutate(&mc_mutation);
        mc_mutations.push(mc_mutation);

        Ok(mc_mutations)
    }
}
