use anyhow::anyhow;
use cid::multihash::Code;
use cid::multihash::MultihashDigest;
use cid::Cid;
use fvm_ipld_encoding::{serde_bytes, to_vec, Cbor};
use fvm_shared::clock::ChainEpoch;
use fvm_shared::econ::TokenAmount;
use primitives::{TCid, TLink};
use serde::{Deserialize, Serialize};

use crate::{CrossMsgs, SubnetID};

#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub data: CheckData,
    #[serde(with = "serde_bytes")]
    sig: Vec<u8>,
}

impl Cbor for Checkpoint {}

impl Checkpoint {
    pub fn new(id: SubnetID, epoch: ChainEpoch) -> Self {
        Self {
            data: CheckData::new(id, epoch),
            sig: Vec::new(),
        }
    }

    /// return cid for the checkpoint
    pub fn cid(&self) -> Cid {
        let mh_code = Code::Blake2b256;
        // we only use the data of the checkpoint to compute the cid, the signature
        // can change according to the source. We are only interested in the data.
        Cid::new_v1(
            fvm_ipld_encoding::DAG_CBOR,
            mh_code.digest(&to_vec(&self.data).unwrap()),
        )
    }

    /// return checkpoint epoch
    pub fn epoch(&self) -> ChainEpoch {
        self.data.epoch
    }

    /// return signature
    pub fn signature(&self) -> &Vec<u8> {
        &self.sig
    }

    /// set signature of checkpoint
    pub fn set_signature(&mut self, sig: Vec<u8>) {
        self.sig = sig;
    }

    /// return checkpoint source
    pub fn source(&self) -> &SubnetID {
        &self.data.source
    }

    /// return the cid of the previous checkpoint this checkpoint points to.
    pub fn prev_check(&self) -> &TCid<TLink<Checkpoint>> {
        &self.data.prev_check
    }

    /// return cross_msg metas included in the checkpoint.
    pub fn cross_msgs(&self) -> &Vec<CrossMsgMeta> {
        &self.data.cross_msgs
    }

    /// return specific crossmsg meta from and to the corresponding subnets.
    pub fn crossmsg_meta(&self, from: &SubnetID, to: &SubnetID) -> Option<&CrossMsgMeta> {
        self.data
            .cross_msgs
            .iter()
            .find(|m| from == &m.from && to == &m.to)
    }

    /// return the index in crossmsg_meta of the structure including metadata from
    /// and to the correponding subnets.
    pub fn crossmsg_meta_index(&self, from: &SubnetID, to: &SubnetID) -> Option<usize> {
        self.data
            .cross_msgs
            .iter()
            .position(|m| from == &m.from && to == &m.to)
    }

    /// append msgmeta to checkpoint
    pub fn append_msgmeta(&mut self, meta: CrossMsgMeta) -> anyhow::Result<()> {
        match self.crossmsg_meta(&meta.from, &meta.to) {
            Some(mm) => {
                if meta != *mm {
                    self.data.cross_msgs.push(meta)
                }
            }
            None => self.data.cross_msgs.push(meta),
        }
        Ok(())
    }

    /// Add the cid of a checkpoint from a child subnet for further propagation
    /// to the upper layerse of the hierarchy.
    pub fn add_child_check(&mut self, commit: &Checkpoint) -> anyhow::Result<()> {
        let cid = TCid::from(commit.cid());
        match self
            .data
            .children
            .iter_mut()
            .find(|m| commit.source() == &m.source)
        {
            // if there is already a structure for that child
            Some(ck) => {
                // check if the cid already exists
                if ck.checks.iter().any(|c| c == &cid) {
                    return Err(anyhow!(
                        "child checkpoint being committed already exists for source {}",
                        commit.source()
                    ));
                }
                // and if not append to list of child checkpoints.
                ck.checks.push(cid);
            }
            None => {
                // if none, new structure for source
                self.data.children.push(ChildCheck {
                    source: commit.data.source.clone(),
                    checks: vec![cid],
                });
            }
        };
        Ok(())
    }
}

#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct CheckData {
    pub source: SubnetID,
    #[serde(with = "serde_bytes")]
    pub tip_set: Vec<u8>,
    pub epoch: ChainEpoch,
    pub prev_check: TCid<TLink<Checkpoint>>,
    pub children: Vec<ChildCheck>,
    pub cross_msgs: Vec<CrossMsgMeta>,
}
impl CheckData {
    pub fn new(id: SubnetID, epoch: ChainEpoch) -> Self {
        Self {
            source: id,
            tip_set: Vec::new(),
            epoch,
            prev_check: TCid::default(),
            children: Vec::new(),
            cross_msgs: Vec::new(),
        }
    }
}
impl Cbor for CheckData {}

#[derive(PartialEq, Eq, Clone, Debug, Default, Serialize, Deserialize)]
pub struct CrossMsgMeta {
    pub from: SubnetID,
    pub to: SubnetID,
    pub msgs_cid: TCid<TLink<CrossMsgs>>,
    pub nonce: u64,
    pub value: TokenAmount,
}
impl Cbor for CrossMsgMeta {}

impl CrossMsgMeta {
    pub fn new(from: &SubnetID, to: &SubnetID) -> Self {
        Self {
            from: from.clone(),
            to: to.clone(),
            ..Default::default()
        }
    }

    pub fn set_nonce(&mut self, nonce: u64) {
        self.nonce = nonce;
    }
}

#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct ChildCheck {
    pub source: SubnetID,
    pub checks: Vec<TCid<TLink<Checkpoint>>>,
}
impl Cbor for ChildCheck {}

/// CheckpointEpoch returns the epoch of the next checkpoint
/// that needs to be signed
///
/// Return the template of the checkpoint template that has been
/// frozen and that is ready for signing and commitment in the
/// current window.
pub fn checkpoint_epoch(epoch: ChainEpoch, period: ChainEpoch) -> ChainEpoch {
    (epoch / period) * period
}

/// WindowEpoch returns the epoch of the active checkpoint window
///
/// Determines the epoch to which new checkpoints and xshard transactions need
/// to be assigned.
pub fn window_epoch(epoch: ChainEpoch, period: ChainEpoch) -> ChainEpoch {
    let ind = epoch / period;
    period * (ind + 1)
}
