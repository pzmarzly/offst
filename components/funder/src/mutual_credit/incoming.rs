use crypto::identity::verify_signature;

use common::safe_arithmetic::SafeSignedArithmetic;

use proto::funder::messages::{
    CancelSendFundsOp, CollectSendFundsOp, FriendTcOp, PendingTransaction, RequestSendFundsOp,
    RequestsStatus, ResponseSendFundsOp, TransactionStage,
};
use proto::funder::signature_buff::create_response_signature_buffer;

use crate::types::create_pending_transaction;

use super::types::{McMutation, MutualCredit, MAX_FUNDER_DEBT};

#[derive(Debug)]
pub struct IncomingResponseSendFundsOp {
    pub pending_transaction: PendingTransaction,
    pub incoming_response: ResponseSendFundsOp,
}

#[derive(Debug)]
pub struct IncomingCancelSendFundsOp {
    pub pending_transaction: PendingTransaction,
    pub incoming_cancel: CancelSendFundsOp,
}

#[derive(Debug)]
pub struct IncomingCollectSendFundsOp {
    pub pending_transaction: PendingTransaction,
    pub incoming_collect: CollectSendFundsOp,
}

#[derive(Debug)]
pub enum IncomingMessage {
    Request(RequestSendFundsOp),
    Response(IncomingResponseSendFundsOp),
    Cancel(IncomingCancelSendFundsOp),
    Collect(IncomingCollectSendFundsOp),
}

/// Resulting tasks to perform after processing an incoming operation.
pub struct ProcessOperationOutput {
    pub incoming_message: Option<IncomingMessage>,
    pub mc_mutations: Vec<McMutation>,
}

#[derive(Debug)]
pub enum ProcessOperationError {
    RemoteMaxDebtTooLarge(u128),
    /// Trying to set the invoiceId, while already expecting another invoice id.
    PkPairNotInRoute,
    /// The Route contains some public key twice.
    InvalidRoute,
    RequestsAlreadyDisabled,
    InsufficientTrust,
    CreditsCalcOverflow,
    RequestAlreadyExists,
    RequestDoesNotExist,
    InvalidResponseSignature,
    LocalRequestsClosed,
    NotExpectingResponse,
    InvalidSrcPlainLock,
    InvalidDestPlainLock,
    NotExpectingCollect,
    DestPaymentExceedsTotal,
}

#[derive(Debug)]
pub struct ProcessTransListError {
    index: usize,
    process_trans_error: ProcessOperationError,
}

pub fn process_operations_list(
    mutual_credit: &mut MutualCredit,
    operations: Vec<FriendTcOp>,
) -> Result<Vec<ProcessOperationOutput>, ProcessTransListError> {
    let mut outputs = Vec::new();

    // We do not change the original MutualCredit.
    // Instead, we are operating over a clone:
    // This operation is not very expensive, because we are using immutable data structures
    // (specifically, HashMaps).

    for (index, funds) in operations.into_iter().enumerate() {
        match process_operation(mutual_credit, funds) {
            Err(e) => {
                return Err(ProcessTransListError {
                    index,
                    process_trans_error: e,
                })
            }
            Ok(trans_output) => outputs.push(trans_output),
        }
    }
    Ok(outputs)
}

pub fn process_operation(
    mutual_credit: &mut MutualCredit,
    friend_tc_op: FriendTcOp,
) -> Result<ProcessOperationOutput, ProcessOperationError> {
    match friend_tc_op {
        FriendTcOp::EnableRequests => process_enable_requests(mutual_credit),
        FriendTcOp::DisableRequests => process_disable_requests(mutual_credit),
        FriendTcOp::SetRemoteMaxDebt(proposed_max_debt) => {
            process_set_remote_max_debt(mutual_credit, proposed_max_debt)
        }
        FriendTcOp::RequestSendFunds(request_send_funds) => {
            process_request_send_funds(mutual_credit, request_send_funds)
        }
        FriendTcOp::ResponseSendFunds(response_send_funds) => {
            process_response_send_funds(mutual_credit, response_send_funds)
        }
        FriendTcOp::CancelSendFunds(cancel_send_funds) => {
            process_cancel_send_funds(mutual_credit, cancel_send_funds)
        }
        FriendTcOp::CollectSendFunds(collect_send_funds) => {
            process_collect_send_funds(mutual_credit, collect_send_funds)
        }
    }
}

fn process_enable_requests(
    mutual_credit: &mut MutualCredit,
) -> Result<ProcessOperationOutput, ProcessOperationError> {
    let mut op_output = ProcessOperationOutput {
        incoming_message: None,
        mc_mutations: Vec::new(),
    };
    let mc_mutation = McMutation::SetRemoteRequestsStatus(RequestsStatus::Open);
    mutual_credit.mutate(&mc_mutation);
    op_output.mc_mutations.push(mc_mutation);

    Ok(op_output)
}

fn process_disable_requests(
    mutual_credit: &mut MutualCredit,
) -> Result<ProcessOperationOutput, ProcessOperationError> {
    let mut op_output = ProcessOperationOutput {
        incoming_message: None,
        mc_mutations: Vec::new(),
    };

    match mutual_credit.state().requests_status.remote {
        RequestsStatus::Open => {
            let mc_mutation = McMutation::SetRemoteRequestsStatus(RequestsStatus::Closed);
            mutual_credit.mutate(&mc_mutation);
            op_output.mc_mutations.push(mc_mutation);
            Ok(op_output)
        }
        RequestsStatus::Closed => Err(ProcessOperationError::RequestsAlreadyDisabled),
    }
}

fn process_set_remote_max_debt(
    mutual_credit: &mut MutualCredit,
    proposed_max_debt: u128,
) -> Result<ProcessOperationOutput, ProcessOperationError> {
    let mut op_output = ProcessOperationOutput {
        incoming_message: None,
        mc_mutations: Vec::new(),
    };

    if proposed_max_debt > MAX_FUNDER_DEBT {
        Err(ProcessOperationError::RemoteMaxDebtTooLarge(
            proposed_max_debt,
        ))
    } else {
        let mc_mutation = McMutation::SetLocalMaxDebt(proposed_max_debt);
        mutual_credit.mutate(&mc_mutation);
        op_output.mc_mutations.push(mc_mutation);
        Ok(op_output)
    }
}

/// Process an incoming RequestSendFundsOp
fn process_request_send_funds(
    mutual_credit: &mut MutualCredit,
    request_send_funds: RequestSendFundsOp,
) -> Result<ProcessOperationOutput, ProcessOperationError> {
    if !request_send_funds.route.is_valid() {
        return Err(ProcessOperationError::InvalidRoute);
    }

    if request_send_funds.dest_payment > request_send_funds.total_dest_payment {
        return Err(ProcessOperationError::DestPaymentExceedsTotal);
    }

    // Find ourselves (And remote side) on the route. If we are not there, abort.
    let _remote_index = request_send_funds
        .route
        .find_pk_pair(
            &mutual_credit.state().idents.remote_public_key,
            &mutual_credit.state().idents.local_public_key,
        )
        .ok_or(ProcessOperationError::PkPairNotInRoute)?;

    // Make sure that we are open to requests:
    if !mutual_credit.state().requests_status.local.is_open() {
        return Err(ProcessOperationError::LocalRequestsClosed);
    }

    // Calculate amount of credits to freeze
    let own_freeze_credits = request_send_funds
        .dest_payment
        .checked_add(request_send_funds.left_fees)
        .ok_or(ProcessOperationError::CreditsCalcOverflow)?;

    // Make sure we can freeze the credits
    let balance = &mutual_credit.state().balance;

    let new_remote_pending_debt = balance
        .remote_pending_debt
        .checked_add(own_freeze_credits)
        .ok_or(ProcessOperationError::CreditsCalcOverflow)?;

    // Check that local_pending_debt - balance <= local_max_debt:
    let add = balance
        .balance
        .checked_add_unsigned(new_remote_pending_debt)
        .ok_or(ProcessOperationError::CreditsCalcOverflow)?;

    if add
        .checked_sub_unsigned(balance.remote_max_debt)
        .ok_or(ProcessOperationError::CreditsCalcOverflow)?
        > 0
    {
        return Err(ProcessOperationError::InsufficientTrust);
    }

    let p_remote_requests = &mutual_credit.state().pending_transactions.remote;
    // Make sure that we don't have this request as a pending request already:
    if p_remote_requests.contains_key(&request_send_funds.request_id) {
        return Err(ProcessOperationError::RequestAlreadyExists);
    }

    // Add pending transaction:
    let pending_transaction = create_pending_transaction(&request_send_funds);

    // let pending_friend_request = create_pending_transaction(&request_send_funds);

    let mut op_output = ProcessOperationOutput {
        incoming_message: Some(IncomingMessage::Request(request_send_funds)),
        mc_mutations: Vec::new(),
    };

    let mc_mutation = McMutation::InsertRemotePendingTransaction(pending_transaction);
    mutual_credit.mutate(&mc_mutation);
    op_output.mc_mutations.push(mc_mutation);

    // If we are here, we can freeze the credits:
    let mc_mutation = McMutation::SetRemotePendingDebt(new_remote_pending_debt);
    mutual_credit.mutate(&mc_mutation);
    op_output.mc_mutations.push(mc_mutation);

    Ok(op_output)
}

fn process_response_send_funds(
    mutual_credit: &mut MutualCredit,
    response_send_funds: ResponseSendFundsOp,
) -> Result<ProcessOperationOutput, ProcessOperationError> {
    // Make sure that id exists in local_pending hashmap,
    // and access saved request details.
    let local_pending_transactions = &mutual_credit.state().pending_transactions.local;

    // Obtain pending request:
    // TODO: Possibly get rid of clone() here for optimization later
    let pending_transaction = local_pending_transactions
        .get(&response_send_funds.request_id)
        .ok_or(ProcessOperationError::RequestDoesNotExist)?
        .clone();

    let dest_public_key = pending_transaction.route.public_keys.last().unwrap();

    let response_signature_buffer =
        create_response_signature_buffer(&response_send_funds, &pending_transaction);

    // Verify response funds signature:
    if !verify_signature(
        &response_signature_buffer,
        dest_public_key,
        &response_send_funds.signature,
    ) {
        return Err(ProcessOperationError::InvalidResponseSignature);
    }

    // We expect that the current stage is Request:
    if let TransactionStage::Request = pending_transaction.stage {
    } else {
        return Err(ProcessOperationError::NotExpectingResponse);
    }

    let mut mc_mutations = Vec::new();

    // Set the stage to Response, and remember dest_hashed_lock:
    let mc_mutation = McMutation::SetLocalPendingTransactionStage((
        response_send_funds.request_id,
        TransactionStage::Response(response_send_funds.dest_hashed_lock.clone()),
    ));
    mutual_credit.mutate(&mc_mutation);
    mc_mutations.push(mc_mutation);

    let incoming_message = Some(IncomingMessage::Response(IncomingResponseSendFundsOp {
        pending_transaction,
        incoming_response: response_send_funds,
    }));

    Ok(ProcessOperationOutput {
        incoming_message,
        mc_mutations,
    })
}

fn process_cancel_send_funds(
    mutual_credit: &mut MutualCredit,
    cancel_send_funds: CancelSendFundsOp,
) -> Result<ProcessOperationOutput, ProcessOperationError> {
    // Make sure that id exists in local_pending hashmap,
    // and access saved request details.
    let local_pending_transactions = &mutual_credit.state().pending_transactions.local;

    // Obtain pending request:
    let pending_transaction = local_pending_transactions
        .get(&cancel_send_funds.request_id)
        .ok_or(ProcessOperationError::RequestDoesNotExist)?
        .clone();
    // TODO: Possibly get rid of clone() here for optimization later

    let mut mc_mutations = Vec::new();

    // Remove entry from local_pending hashmap:
    let mc_mutation = McMutation::RemoveLocalPendingTransaction(cancel_send_funds.request_id);
    mutual_credit.mutate(&mc_mutation);
    mc_mutations.push(mc_mutation);

    let freeze_credits = pending_transaction
        .dest_payment
        .checked_add(pending_transaction.left_fees)
        .unwrap();

    // Decrease frozen credits:
    let new_local_pending_debt = mutual_credit
        .state()
        .balance
        .local_pending_debt
        .checked_sub(freeze_credits)
        .unwrap();

    let mc_mutation = McMutation::SetLocalPendingDebt(new_local_pending_debt);
    mutual_credit.mutate(&mc_mutation);
    mc_mutations.push(mc_mutation);

    // Return cancel_send_funds:
    let incoming_message = Some(IncomingMessage::Cancel(IncomingCancelSendFundsOp {
        pending_transaction,
        incoming_cancel: cancel_send_funds,
    }));

    Ok(ProcessOperationOutput {
        incoming_message,
        mc_mutations,
    })
}

fn process_collect_send_funds(
    mutual_credit: &mut MutualCredit,
    collect_send_funds: CollectSendFundsOp,
) -> Result<ProcessOperationOutput, ProcessOperationError> {
    // Make sure that id exists in local_pending hashmap,
    // and access saved request details.
    let local_pending_transactions = &mutual_credit.state().pending_transactions.local;

    // Obtain pending request:
    // TODO: Possibly get rid of clone() here for optimization later
    let pending_transaction = local_pending_transactions
        .get(&collect_send_funds.request_id)
        .ok_or(ProcessOperationError::RequestDoesNotExist)?
        .clone();

    let dest_hashed_lock = match &pending_transaction.stage {
        TransactionStage::Response(dest_hashed_lock) => dest_hashed_lock,
        _ => return Err(ProcessOperationError::NotExpectingCollect),
    };

    // Verify src_plain_lock and dest_plain_lock:
    if collect_send_funds.src_plain_lock.hash() != pending_transaction.src_hashed_lock {
        return Err(ProcessOperationError::InvalidSrcPlainLock);
    }

    if collect_send_funds.dest_plain_lock.hash() != *dest_hashed_lock {
        return Err(ProcessOperationError::InvalidDestPlainLock);
    }

    // Calculate amount of credits that were frozen:
    let freeze_credits = pending_transaction
        .dest_payment
        .checked_add(pending_transaction.left_fees)
        .unwrap();
    // Note: The unwrap() above should never fail, because this was already checked during the
    // request message processing.

    let mut mc_mutations = Vec::new();

    // Remove entry from local_pending hashmap:
    let mc_mutation = McMutation::RemoveLocalPendingTransaction(collect_send_funds.request_id);
    mutual_credit.mutate(&mc_mutation);
    mc_mutations.push(mc_mutation);

    // Decrease frozen credits and decrease balance:
    let new_local_pending_debt = mutual_credit
        .state()
        .balance
        .local_pending_debt
        .checked_sub(freeze_credits)
        .unwrap();

    let mc_mutation = McMutation::SetLocalPendingDebt(new_local_pending_debt);
    mutual_credit.mutate(&mc_mutation);
    mc_mutations.push(mc_mutation);

    let new_balance = mutual_credit
        .state()
        .balance
        .balance
        .checked_sub_unsigned(freeze_credits)
        .unwrap();

    let mc_mutation = McMutation::SetBalance(new_balance);
    mutual_credit.mutate(&mc_mutation);
    mc_mutations.push(mc_mutation);

    let incoming_message = Some(IncomingMessage::Collect(IncomingCollectSendFundsOp {
        pending_transaction,
        incoming_collect: collect_send_funds,
    }));

    Ok(ProcessOperationOutput {
        incoming_message,
        mc_mutations,
    })
}
