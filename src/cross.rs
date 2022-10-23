use crate::address::HierarchicalAddress;
use anyhow::anyhow;
use cid::Cid;
use fil_actors_runtime::BURNT_FUNDS_ACTOR_ADDR;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::Cbor;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::econ::TokenAmount;
use fvm_shared::MethodNum;
use fvm_shared::METHOD_SEND;
use primitives::{TAmt, TCid, TLink};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::checkpoint::CrossMsgMeta;
use crate::SubnetID;

/// StorableMsg stores all the relevant information required
/// to execute cross-messages.
///
/// We follow this approach because we can't directly store types.Message
/// as we did in the actor's Go counter-part. Instead we just persist the
/// information required to create the cross-messages and execute in the
/// corresponding node implementation.
#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct StorableMsg {
    pub from: HierarchicalAddress,
    pub to: HierarchicalAddress,
    pub method: MethodNum,
    pub params: RawBytes,
    // #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
    pub nonce: u64,
}
impl Cbor for StorableMsg {}

impl Default for StorableMsg {
    fn default() -> Self {
        Self {
            from: HierarchicalAddress::new_id(0),
            to: HierarchicalAddress::new_id(0),
            method: 0,
            params: RawBytes::default(),
            value: TokenAmount::default(),
            nonce: 0,
        }
    }
}

#[derive(PartialEq, Eq)]
pub enum HCMsgType {
    Unknown = 0,
    BottomUp,
    TopDown,
}

impl StorableMsg {
    pub fn new_release_msg(
        sub_id: &SubnetID,
        sig_addr: &Address,
        value: TokenAmount,
        nonce: u64,
    ) -> anyhow::Result<Self> {
        let to = HierarchicalAddress::new(
            &match sub_id.parent() {
                Some(s) => s,
                None => return Err(anyhow!("error getting parent for subnet addr")),
            },
            sig_addr,
        )?;
        let from = HierarchicalAddress::new(sub_id, &BURNT_FUNDS_ACTOR_ADDR)?;
        Ok(Self {
            from,
            to,
            method: METHOD_SEND,
            params: RawBytes::default(),
            value,
            nonce,
        })
    }

    pub fn new_fund_msg(
        sub_id: &SubnetID,
        sig_addr: &Address,
        value: TokenAmount,
    ) -> anyhow::Result<Self> {
        let from = HierarchicalAddress::new(
            &match sub_id.parent() {
                Some(s) => s,
                None => return Err(anyhow!("error getting parent for subnet addr")),
            },
            sig_addr,
        )?;
        let to = HierarchicalAddress::new(sub_id, sig_addr)?;
        // the nonce and the rest of message fields are set when the message is committed.
        Ok(Self {
            from,
            to,
            method: METHOD_SEND,
            value,
            ..Default::default()
        })
    }

    pub fn hc_type(&self) -> anyhow::Result<HCMsgType> {
        let sto = self.to.subnet()?;
        let sfrom = self.from.subnet()?;
        if is_bottomup(&sfrom, &sto) {
            return Ok(HCMsgType::BottomUp);
        }
        Ok(HCMsgType::TopDown)
    }

    pub fn apply_type(&self, curr: &SubnetID) -> anyhow::Result<HCMsgType> {
        let sto = self.to.subnet()?;
        let sfrom = self.from.subnet()?;
        if curr.common_parent(&sto) == sfrom.common_parent(&sto)
            && self.hc_type()? == HCMsgType::BottomUp
        {
            return Ok(HCMsgType::BottomUp);
        }
        Ok(HCMsgType::TopDown)
    }
}

pub fn is_bottomup(from: &SubnetID, to: &SubnetID) -> bool {
    let index = match from.common_parent(to) {
        Some((ind, _)) => ind,
        None => return false,
    };
    let a = from.to_string();
    Path::new(&a).components().count() - 1 > index
}

#[derive(PartialEq, Eq, Clone, Debug, Default, Serialize, Deserialize)]
pub struct CrossMsgs {
    pub msgs: Vec<StorableMsg>,
    pub metas: Vec<CrossMsgMeta>,
}
impl Cbor for CrossMsgs {}

#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct MetaTag {
    pub msgs_cid: TCid<TAmt<StorableMsg>>,
    pub meta_cid: TCid<TAmt<CrossMsgMeta>>,
}
impl Cbor for MetaTag {}

impl MetaTag {
    pub fn new<BS: Blockstore>(store: &BS) -> anyhow::Result<MetaTag> {
        Ok(Self {
            msgs_cid: TCid::new_amt(store)?,
            meta_cid: TCid::new_amt(store)?,
        })
    }
}

impl CrossMsgs {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn cid(&self) -> anyhow::Result<Cid> {
        let store = MemoryBlockstore::new();
        let mut meta = MetaTag::new(&store)?;

        meta.msgs_cid.update(&store, |msgs_array| {
            msgs_array
                .batch_set(self.msgs.clone())
                .map_err(|e| e.into())
        })?;

        meta.meta_cid.update(&store, |meta_array| {
            meta_array
                .batch_set(self.metas.clone())
                .map_err(|e| e.into())
        })?;

        let meta_cid: TCid<TLink<MetaTag>> = TCid::new_link(&store, &meta)?;

        Ok(meta_cid.cid())
    }

    pub(crate) fn add_metas(&mut self, metas: Vec<CrossMsgMeta>) -> anyhow::Result<()> {
        for m in metas.iter() {
            if self.metas.iter().any(|ms| ms == m) {
                continue;
            }
            self.metas.push(m.clone());
        }

        Ok(())
    }

    pub(crate) fn add_msg(&mut self, msg: &StorableMsg) -> anyhow::Result<()> {
        // TODO: Check if the message has already been added.
        self.msgs.push(msg.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::cross::*;
    use std::str::FromStr;

    #[test]
    fn test_is_bottomup() {
        bottom_up("/root/f01", "/root/f01/f02", false);
        bottom_up("/root/f01", "/root", true);
        bottom_up("/root/f01", "/root/f01/f02", false);
        bottom_up("/root/f01", "/root/f02/f02", true);
        bottom_up("/root/f01/f02", "/root/f01/f02", false);
        bottom_up("/root/f01/f02", "/root/f01/f02/f03", false);
    }
    fn bottom_up(a: &str, b: &str, res: bool) {
        assert_eq!(
            is_bottomup(
                &SubnetID::from_str(a).unwrap(),
                &SubnetID::from_str(b).unwrap()
            ),
            res
        );
    }
}
