use crypto::hash::HashResult;

// TODO: impl Receipt

/// A SendFundsReceipt is received if a RequestSendFunds is successful.
/// It can be used a proof of payment for a specific invoice_id.
struct SendFundsReceipt {
    response_hash: HashResult,
    // = sha512/256(requestId ||
    //       sha512/256(nodeIdPath) ||
    //       mediatorPaymentProposal)
    invoice_id: InvoiceId,
    payment: u128,
    rand_nonce: RandValue,
    signature: Signature,
    // Signature{key=recipientKey}(
    //   "FUND_SUCCESS" ||
    //   sha512/256(requestId || sha512/256(nodeIdPath) || mediatorPaymentProposal) ||
    //   invoiceId ||
    //   payment ||
    //   randNonce)
}