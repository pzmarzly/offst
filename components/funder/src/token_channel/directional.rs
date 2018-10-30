#![warn(unused)]

use std::convert::TryFrom;
use byteorder::{BigEndian, WriteBytesExt};

use crypto::identity::{PublicKey, Signature, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use crypto::crypto_rand::{RandValue, RAND_VALUE_LEN};
use crypto::hash::sha_512_256;
use identity::IdentityClient;

use crate::consts::MAX_OPERATIONS_IN_BATCH;

use crate::token_channel::types::{TokenChannel, TcMutation};
use super::incoming::{ProcessOperationOutput, ProcessTransListError, 
    process_operations_list, IncomingMessage};
use super::outgoing::OutgoingTc;

use crate::types::{FriendMoveToken, 
    FriendMoveTokenRequest, ResetTerms, FriendTcOp};


// Prefix used for chain hashing of token channel funds.
// NEXT is used for hashing for the next move token funds.
// RESET is used for resetting the token channel.
// The prefix allows the receiver to distinguish between the two cases.
// const TOKEN_NEXT: &[u8] = b"NEXT";
const TOKEN_RESET: &[u8] = b"RESET";



/// Indicate the direction of the move token funds.
#[derive(Clone, Serialize, Deserialize)]
pub enum MoveTokenDirection {
    Incoming(FriendMoveToken),
    Outgoing(FriendMoveTokenRequest),
}

pub enum SetDirection {
    Incoming(FriendMoveToken), 
    Outgoing(FriendMoveToken),
}

#[allow(unused)]
pub enum DirectionalMutation {
    TcMutation(TcMutation),
    SetDirection(SetDirection),
    SetTokenWanted,
}


#[derive(Clone, Serialize, Deserialize)]
pub struct DirectionalTc {
    pub direction: MoveTokenDirection,
    pub token_channel: TokenChannel,
}

#[derive(Debug)]
pub enum ReceiveMoveTokenError {
    ChainInconsistency,
    InvalidTransaction(ProcessTransListError),
    InvalidSignature,
    InvalidStatedBalance,
    InvalidInconsistencyCounter,
    MoveTokenCounterOverflow,
    InvalidMoveTokenCounter,
}

pub struct MoveTokenReceived {
    pub incoming_messages: Vec<IncomingMessage>,
    pub mutations: Vec<DirectionalMutation>,
}


pub enum ReceiveMoveTokenOutput {
    Duplicate,
    RetransmitOutgoing(FriendMoveToken),
    Received(MoveTokenReceived),
    // In case of a reset, all the local pending requests will be canceled.
}



/// Calculate the token to be used for resetting the channel.
#[allow(unused)]
pub async fn calc_channel_reset_token(new_token: &Signature,
                      balance_for_reset: i128,
                      identity_client: IdentityClient) -> Signature {

    let mut sig_buffer = Vec::new();
    sig_buffer.extend_from_slice(&sha_512_256(TOKEN_RESET));
    sig_buffer.extend_from_slice(&new_token);
    sig_buffer.write_i128::<BigEndian>(balance_for_reset).unwrap();
    await!(identity_client.request_signature(sig_buffer)).unwrap()
}

/// Create a token from a public key
/// Currently this function puts the public key in the beginning of the signature buffer,
/// as the public key is shorter than a signature.
/// Possibly change this in the future (Maybe use a hash to spread the public key over all the
/// bytes of the signature)
///
/// Note that the output here is not a real signature. This function is used for the first
/// deterministic initialization of a token channel.
fn token_from_public_key(public_key: &PublicKey) -> Signature {
    let mut buff = [0; SIGNATURE_LEN];
    buff[0 .. PUBLIC_KEY_LEN].copy_from_slice(public_key);
    Signature::from(buff)
}

/// Generate a random nonce from public key.
/// Note that the result here is not really a random nonce. This function is used for the first
/// deterministic initialization of a token channel.
fn rand_nonce_from_public_key(public_key: &PublicKey) -> RandValue {
    let public_key_hash = sha_512_256(public_key);
    RandValue::try_from(&public_key_hash.as_ref()[.. RAND_VALUE_LEN]).unwrap()
}

impl DirectionalTc {

    #[allow(unused)]
    pub async fn new<'a>(local_public_key: &'a PublicKey, 
               remote_public_key: &'a PublicKey,
               identity_client: IdentityClient) -> DirectionalTc {

        let balance = 0;
        let token_channel = TokenChannel::new(&local_public_key, &remote_public_key, balance);
        let rand_nonce = rand_nonce_from_public_key(&remote_public_key);

        let move_token_counter = 0;
        let inconsistency_counter = 0;
        let first_move_token_lower = await!(FriendMoveToken::new(
            Vec::new(),
            token_from_public_key(&local_public_key),
            inconsistency_counter,
            move_token_counter,
            token_channel.state().balance.balance,
            token_channel.state().balance.local_pending_debt,
            token_channel.state().balance.remote_pending_debt,
            rand_nonce.clone(),
            identity_client));

        if sha_512_256(&local_public_key) < sha_512_256(&remote_public_key) {
            // We are the first sender
            let friend_move_token_request = FriendMoveTokenRequest {
                friend_move_token: first_move_token_lower.clone(),
                token_wanted: false,
            };
            DirectionalTc {
                direction: MoveTokenDirection::Outgoing(friend_move_token_request),
                token_channel,
            }
        } else {
            // We are the second sender
            DirectionalTc {
                direction: MoveTokenDirection::Incoming(first_move_token_lower),
                token_channel,
            }
        }
    }

    pub async fn create_friend_move_token(&self,
                                    operations: Vec<FriendTcOp>,
                                    rand_nonce: RandValue,
                                    identity_client: IdentityClient) -> Option<FriendMoveToken> {

        let friend_move_token = match &self.direction {
            MoveTokenDirection::Incoming(friend_move_token) => friend_move_token,
            MoveTokenDirection::Outgoing(_) => return None,
        };

        Some(await!(FriendMoveToken::new(
            operations,
            friend_move_token.new_token.clone(),
            friend_move_token.inconsistency_counter,
            friend_move_token.move_token_counter.wrapping_add(1),
            self.get_token_channel().state().balance.balance,
            self.get_token_channel().state().balance.local_pending_debt,
            self.get_token_channel().state().balance.remote_pending_debt,
            rand_nonce,
            identity_client)))
    }

    pub fn new_from_remote_reset(local_public_key: &PublicKey, 
                      remote_public_key: &PublicKey, 
                      reset_move_token: &FriendMoveToken,
                      balance: i128) -> DirectionalTc {

        DirectionalTc {
            direction: MoveTokenDirection::Incoming(reset_move_token.clone()),
            token_channel: TokenChannel::new(local_public_key, remote_public_key, balance),
        }
    }

    pub fn new_from_local_reset(local_public_key: &PublicKey, 
                      remote_public_key: &PublicKey, 
                      reset_move_token: &FriendMoveToken,
                      balance: i128) -> DirectionalTc {

        let friend_move_token_request = FriendMoveTokenRequest {
            friend_move_token: reset_move_token.clone(),
            token_wanted: false,
        };
        DirectionalTc {
            direction: MoveTokenDirection::Outgoing(friend_move_token_request),
            token_channel: TokenChannel::new(local_public_key, remote_public_key, balance),
        }
    }

    /// Get a reference to internal token_channel.
    pub fn get_token_channel(&self) -> &TokenChannel {
        &self.token_channel
    }

    #[allow(unused)]
    fn balance_for_reset(&self) -> i128 {
        self.get_token_channel().balance_for_reset()
    }

    pub fn remote_max_debt(&self) -> u128 {
        self.get_token_channel().state().balance.remote_max_debt
    }

    pub fn get_new_token(&self) -> &Signature {
        let friend_move_token = match &self.direction {
            MoveTokenDirection::Incoming(friend_move_token) => friend_move_token,
            MoveTokenDirection::Outgoing(friend_move_token_request) => 
                &friend_move_token_request.friend_move_token,
        };
        &friend_move_token.new_token
    }

    #[allow(unused)]
    async fn calc_channel_reset_token(&self, identity_client: IdentityClient) -> Signature {
        await!(calc_channel_reset_token(&self.get_new_token(),
                                 self.get_token_channel().balance_for_reset(),
                                 identity_client))
    }

    pub fn get_inconsistency_counter(&self) -> u64 {
        let friend_move_token = match &self.direction {
            MoveTokenDirection::Incoming(friend_move_token) => friend_move_token,
            MoveTokenDirection::Outgoing(friend_move_token_request) => 
                &friend_move_token_request.friend_move_token,
        };
        friend_move_token.inconsistency_counter
    }

    pub fn get_move_token_counter(&self) -> u128 {
        let friend_move_token = match &self.direction {
            MoveTokenDirection::Incoming(friend_move_token) => friend_move_token,
            MoveTokenDirection::Outgoing(friend_move_token_request) => 
                &friend_move_token_request.friend_move_token,
        };
        friend_move_token.move_token_counter
    }

    pub async fn get_reset_terms(&self, identity_client: IdentityClient) -> ResetTerms {
        // We add 2 for the new counter in case 
        // the remote side has already used the next counter.
        ResetTerms {
            reset_token: await!(self.calc_channel_reset_token(identity_client)),
            inconsistency_counter: self.get_inconsistency_counter().wrapping_add(1),
            balance_for_reset: self.balance_for_reset(),
        }
    }

    pub fn is_outgoing(&self) -> bool {
        match self.direction {
            MoveTokenDirection::Incoming(_) => false,
            MoveTokenDirection::Outgoing(_) => true,
        }
    }

    pub fn mutate(&mut self, d_mutation: &DirectionalMutation) {
        match d_mutation {
            DirectionalMutation::TcMutation(tc_mutation) => {
                self.token_channel.mutate(tc_mutation);
            },
            DirectionalMutation::SetDirection(ref set_direction) => {
                self.direction = match set_direction {
                    SetDirection::Incoming(new_token) => MoveTokenDirection::Incoming(new_token.clone()),
                    SetDirection::Outgoing(friend_move_token) => {
                        MoveTokenDirection::Outgoing(FriendMoveTokenRequest {
                            friend_move_token: friend_move_token.clone(),
                            token_wanted: false,
                        })
                    }
                };
            },
            DirectionalMutation::SetTokenWanted => {
                match self.direction {
                    MoveTokenDirection::Incoming(_) => unreachable!(),
                    MoveTokenDirection::Outgoing(ref mut friend_move_token_request) => {
                        friend_move_token_request.token_wanted = true;
                    },
                }
            },
        }
    }

    /// Deal with the case of incoming state turning into outgoing state.
    fn outgoing_to_incoming(&self, 
                            old_move_token: &FriendMoveToken, 
                            new_move_token: FriendMoveToken) 
        -> Result<ReceiveMoveTokenOutput, ReceiveMoveTokenError> {
    
        // Verify counters:
        if new_move_token.inconsistency_counter != old_move_token.inconsistency_counter {
            return Err(ReceiveMoveTokenError::InvalidInconsistencyCounter);
        }

        let expected_move_token_counter = old_move_token.move_token_counter
            .checked_add(1)
            .ok_or(ReceiveMoveTokenError::MoveTokenCounterOverflow)?;

        if new_move_token.move_token_counter != expected_move_token_counter {
            return Err(ReceiveMoveTokenError::InvalidMoveTokenCounter);
        }

        let mut token_channel = self.token_channel.clone();
        let res = process_operations_list(&mut token_channel,
            new_move_token.operations.clone());

        // Verify balance:
        if token_channel.state().balance.balance != new_move_token.balance ||
           token_channel.state().balance.local_pending_debt != new_move_token.local_pending_debt ||
           token_channel.state().balance.remote_pending_debt != new_move_token.remote_pending_debt {
            return Err(ReceiveMoveTokenError::InvalidStatedBalance);
        }

        match res {
            Ok(outputs) => {
                let mut move_token_received = MoveTokenReceived {
                    incoming_messages: Vec::new(),
                    mutations: Vec::new(),
                };

                for output in outputs {
                    let ProcessOperationOutput 
                        {incoming_message, tc_mutations} = output;

                    if let Some(funds) = incoming_message {
                        move_token_received.incoming_messages.push(funds);
                    }
                    for tc_mutation in tc_mutations {
                        move_token_received.mutations.push(
                            DirectionalMutation::TcMutation(tc_mutation));
                    }
                }
                move_token_received.mutations.push(
                    DirectionalMutation::SetDirection(SetDirection::Incoming(new_move_token)));
                Ok(ReceiveMoveTokenOutput::Received(move_token_received))
            },
            Err(e) => {
                Err(ReceiveMoveTokenError::InvalidTransaction(e))
            },
        }
    }


    #[allow(unused)]
    pub fn simulate_receive_move_token(&self, 
                              move_token_msg: FriendMoveToken)
        -> Result<ReceiveMoveTokenOutput, ReceiveMoveTokenError> {

        // Verify signature:
        let remote_public_key = &self.get_token_channel().state().idents.remote_public_key;
        if !move_token_msg.verify(remote_public_key) {
            return Err(ReceiveMoveTokenError::InvalidSignature);
        }

        match &self.direction {
            MoveTokenDirection::Incoming(friend_move_token) => {
                if friend_move_token == &move_token_msg {
                    // Duplicate
                    Ok(ReceiveMoveTokenOutput::Duplicate)
                } else {
                    // Inconsistency
                    Err(ReceiveMoveTokenError::ChainInconsistency)
                }
            },
            MoveTokenDirection::Outgoing(ref friend_move_token_request) => {
                let friend_move_token = &friend_move_token_request.friend_move_token;
                if &move_token_msg.old_token == self.get_new_token() {
                    self.outgoing_to_incoming(friend_move_token, move_token_msg)
                } else if friend_move_token.old_token == move_token_msg.new_token {
                    // We should retransmit our move token message to the remote side.
                    Ok(ReceiveMoveTokenOutput::RetransmitOutgoing(friend_move_token.clone()))
                } else {
                    Err(ReceiveMoveTokenError::ChainInconsistency)
                }
            },
        }
    }

    #[allow(unused)]
    pub fn begin_outgoing_move_token(&self) -> Option<OutgoingTc> {
        if let MoveTokenDirection::Outgoing(_) = self.direction {
            return None;
        }

        Some(OutgoingTc::new(&self.token_channel, MAX_OPERATIONS_IN_BATCH))
    }


    #[allow(unused)]
    pub fn get_outgoing_move_token(&self) -> Option<FriendMoveTokenRequest> {
        match self.direction {
            MoveTokenDirection::Incoming(_)=> None,
            MoveTokenDirection::Outgoing(ref friend_move_token_request) => {
                Some(friend_move_token_request.clone())
            }
        }
    }
}
