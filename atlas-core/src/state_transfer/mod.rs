use std::sync::Arc;
use atlas_execution::serialize::SharedData;
use atlas_common::error::*;
use atlas_common::globals::ReadOnly;
use atlas_common::ordering::{Orderable, SeqNo};
use atlas_communication::message::StoredMessage;
use atlas_communication::Node;
use crate::messages::{Protocol, StateTransfer};
use crate::ordering_protocol::{OrderingProtocol, OrderingProtocolArgs, View};
use crate::serialize::{NetworkView, OrderingProtocolMessage, ServiceMsg, StatefulOrderProtocolMessage, StateTransferMessage};
use crate::timeouts::{RqTimeout, Timeouts};
#[cfg(feature = "serialize_serde")]
use serde::{Serialize, Deserialize};
use atlas_common::crypto::hash::Digest;
use atlas_execution::ExecutorHandle;
use crate::request_pre_processing::BatchOutput;


/// Represents a local checkpoint.
///
/// Contains the last application state, as well as the sequence number
/// which decided the last batch of requests executed before the checkpoint.
#[cfg_attr(feature = "serialize_serde", derive(Serialize, Deserialize))]
#[derive(Clone)]
pub struct Checkpoint<S> {
    seq: SeqNo,
    app_state: S,
    digest: Digest,
}

impl<S> Orderable for Checkpoint<S> {
    /// Returns the sequence number of the batch of client requests
    /// decided before the local checkpoint.
    fn sequence_number(&self) -> SeqNo {
        self.seq
    }
}

impl<S> Checkpoint<S> {
    pub fn new(seq: SeqNo, app_state: S, digest: Digest) -> Arc<ReadOnly<Self>> {
        Arc::new(ReadOnly::new(Self {
            seq,
            app_state,
            digest,
        }))
    }

    /// The last sequence no represented in this checkpoint
    pub fn last_seq(&self) -> &SeqNo {
        &self.seq
    }

    /// Returns a reference to the state of the application before
    /// the local checkpoint.
    pub fn state(&self) -> &S {
        &self.app_state
    }

    pub fn digest(&self) -> &Digest { &self.digest }

    /// Returns the inner values within this local checkpoint.
    pub fn into_inner(self) -> (SeqNo, S, Digest) {
        (self.seq, self.app_state, self.digest)
    }
}

/// The result of processing a message in the state transfer protocol
pub enum STResult<D: SharedData> {
    RunCst,
    CstNotNeeded,
    CstRunning,
    CstFinished(D::State, Vec<D::Request>),
}

pub enum STTimeoutResult {
    RunCst,
    CstNotNeeded,
}

pub type CstM<M: StateTransferMessage> = <M as StateTransferMessage>::StateTransferMessage;

/// A trait for the implementation of the state transfer protocol
pub trait StateTransferProtocol<D, OP, NT> where
    D: SharedData + 'static,
    OP: StatefulOrderProtocol<D, NT> + 'static {
    type Serialization: StateTransferMessage + 'static;

    type Config;

    /// Initialize the state transferring protocol with the given configuration, timeouts and communication layer
    fn initialize(config: Self::Config, timeouts: Timeouts, node: Arc<NT>) -> Result<Self>
        where Self: Sized;

    /// Request the latest state from the rest of replicas
    fn request_latest_state(&mut self,
                            order_protocol: &mut OP) -> Result<()>
        where NT: Node<ServiceMsg<D, OP::Serialization, Self::Serialization>>;

    /// Handle a state transfer protocol message that was received while executing the ordering protocol
    fn handle_off_ctx_message(&mut self,
                              order_protocol: &mut OP,
                              message: StoredMessage<StateTransfer<CstM<Self::Serialization>>>)
                              -> Result<()>
        where NT: Node<ServiceMsg<D, OP::Serialization, Self::Serialization>>;

    /// Process a state transfer protocol message
    fn process_message(&mut self,
                       order_protocol: &mut OP,
                       message: StoredMessage<StateTransfer<CstM<Self::Serialization>>>)
                       -> Result<STResult<D>>
        where NT: Node<ServiceMsg<D, OP::Serialization, Self::Serialization>>;

    /// Handle having received a state from the application
    fn handle_state_received_from_app(&mut self,
                                      order_protocol: &mut OP,
                                      state: Arc<ReadOnly<Checkpoint<D::State>>>) -> Result<()>
        where NT: Node<ServiceMsg<D, OP::Serialization, Self::Serialization>>;

    /// Handle a timeout being received from the timeout layer
    fn handle_timeout(&mut self, order_protocol: &mut OP, timeout: Vec<RqTimeout>) -> Result<STTimeoutResult>
        where NT: Node<ServiceMsg<D, OP::Serialization, Self::Serialization>>;
}

pub type DecLog<OP> = <OP as StatefulOrderProtocolMessage>::DecLog;
pub type SerProof<OP> = <OP as StatefulOrderProtocolMessage>::Proof;

/// An order protocol that uses the state transfer protocol to manage its state.
pub trait StatefulOrderProtocol<D: SharedData + 'static, NT>: OrderingProtocol<D, NT> {
    /// The serialization abstraction for
    type StateSerialization: StatefulOrderProtocolMessage + 'static;

    fn initialize_with_initial_state(config: Self::Config, args: OrderingProtocolArgs<D, NT>, initial_state: Arc<ReadOnly<Checkpoint<D::State>>>) -> Result<Self> where
        Self: Sized;

    /// Get the current sequence number of the protocol, combined with a proof of it so we can send it to other replicas
    fn sequence_number_with_proof(&self) -> Result<Option<(SeqNo, SerProof<Self::StateSerialization>)>>;

    /// Verify the sequence number sent by another replica. This doesn't pass a mutable reference since we don't want to
    /// make any changes to the state of the protocol here (or allow the implementer to do so). Instead, we want to
    /// just verify this sequence number
    fn verify_sequence_number(&self, seq_no: SeqNo, proof: &SerProof<Self::StateSerialization>) -> Result<bool>;

    /// Install a state received from other replicas in the system
    /// Should only alter the necessary things within its own state and
    /// then should return the state and a list of all requests that should
    /// then be executed by the application.
    fn install_state(&mut self, state: Arc<ReadOnly<Checkpoint<D::State>>>,
                     view_info: View<Self::Serialization>,
                     dec_log: DecLog<Self::StateSerialization>) -> Result<(D::State, Vec<D::Request>)>;

    /// Install a given sequence number
    fn install_seq_no(&mut self, seq_no: SeqNo) -> Result<()>;

    /// Snapshot the current log of the replica
    fn snapshot_log(&mut self) -> Result<(Arc<ReadOnly<Checkpoint<D::State>>>,
                                          View<Self::Serialization>,
                                          DecLog<Self::StateSerialization>)>;

    /// Finalize the checkpoint of the replica
    fn finalize_checkpoint(&mut self, state: Arc<ReadOnly<Checkpoint<D::State>>>) -> Result<()>;
}