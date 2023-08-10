// Copyright: ConsensusLab
//
use cid::multihash::Blake2b256;
use cid::multihash::Hasher;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_ipld_hamt::BytesKey;
use fvm_shared::address::Address;
use fvm_shared::MethodNum;
use ipc_gateway::IPCAddress;
use primitives::{TCid, THamt};
use serde_tuple::{Deserialize_tuple, Serialize_tuple};
use std::collections::HashMap;

use crate::types::AtomicExecID;
use crate::ConstructorParams;

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct State {
    pub registry: RegistryCid, // H(exec_id, actors) -> pre-commitments
    pub ipc_gateway_address: Address,
}

type RegistryCid = TCid<THamt<RegistryKey, RegistryEntry>>;
type RegistryKey = BytesKey;
type RegistryEntry = HashMap<IPCAddrString, MethodNum>;
type IPCAddrString = String;

impl State {
    pub fn new<BS: Blockstore>(store: &BS, params: ConstructorParams) -> anyhow::Result<State> {
        Ok(State {
            registry: TCid::new_hamt(store)?,
            ipc_gateway_address: params.ipc_gateway_address,
        })
    }

    /// Modifies the atomic execution entry associated with the atomic
    /// execution ID and the actors.
    pub fn modify_atomic_exec<BS: Blockstore, R>(
        &mut self,
        store: &BS,
        exec_id: &AtomicExecID,
        actors: &Vec<IPCAddress>,
        f: impl FnOnce(&mut RegistryEntry) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        let k = Self::registry_key(exec_id, actors);
        self.registry.modify(store, |registry| {
            let mut entry = registry
                .get(&k)?
                .map_or_else(HashMap::new, |e| e.to_owned());
            let res = f(&mut entry)?;
            registry.set(k, entry)?;
            Ok(res)
        })
    }

    /// Removes the atomic execution entry associated with the atomic
    /// execution ID and the actors.
    pub fn rm_atomic_exec<BS: Blockstore>(
        &mut self,
        store: &BS,
        exec_id: &AtomicExecID,
        actors: &Vec<IPCAddress>,
    ) -> anyhow::Result<()> {
        let k = Self::registry_key(exec_id, actors);
        self.registry.update(store, |registry| {
            registry.delete(&k)?;
            Ok(())
        })?;
        Ok(())
    }

    fn registry_key(exec_id: &AtomicExecID, actors: &Vec<IPCAddress>) -> RegistryKey {
        let mut h = Blake2b256::default();
        h.update(exec_id);
        h.update(&RawBytes::serialize(actors).unwrap());
        RegistryKey::from(h.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fvm_ipld_blockstore::MemoryBlockstore;
    use ipc_gateway::{IPCAddress, SubnetID};

    lazy_static::lazy_static! {
        static ref ROOTNET_ID: SubnetID = SubnetID::new(123, vec![]);
    }

    #[test]
    fn state_works() {
        let store = MemoryBlockstore::new();
        let mut state = State::new(
            &store,
            ConstructorParams {
                ipc_gateway_address: Address::new_id(64),
            },
        )
        .unwrap();

        let exec_id = AtomicExecID::from(Vec::from("exec_id"));
        let actors = vec![
            IPCAddress::new(
                &SubnetID::new_from_parent(&ROOTNET_ID, Address::new_id('A' as u64)),
                &Address::new_id(1),
            )
            .unwrap(),
            IPCAddress::new(
                &SubnetID::new_from_parent(&ROOTNET_ID, Address::new_id('B' as u64)),
                &Address::new_id(1),
            )
            .unwrap(),
        ];
        state
            .modify_atomic_exec(&store, &exec_id, &actors, |entry| {
                entry.insert(actors[0].to_string().unwrap(), 2);
                entry.insert(actors[1].to_string().unwrap(), 3);
                Ok(())
            })
            .unwrap();

        let entry = state
            .modify_atomic_exec(&store, &exec_id, &actors, |entry| Ok(entry.clone()))
            .unwrap();
        assert_eq!(entry[&actors[0].to_string().unwrap()], 2);
        assert_eq!(entry[&actors[1].to_string().unwrap()], 3);

        state.rm_atomic_exec(&store, &exec_id, &actors).unwrap();
        let entry = state
            .modify_atomic_exec(&store, &exec_id, &actors, |entry| Ok(entry.clone()))
            .unwrap();
        assert_eq!(entry.keys().len(), 0);
    }
}
