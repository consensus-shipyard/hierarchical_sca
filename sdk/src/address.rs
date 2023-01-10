use crate::error::Error;
use crate::subnet_id::SubnetID;
use fil_actors_runtime::cbor;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

const IPC_SEPARATOR_ADDR: &str = ":";

#[derive(Clone, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
pub struct IPCAddress {
    subnet_id: SubnetID,
    raw_address: Address,
}

impl IPCAddress {
    /// Generates new IPC address
    pub fn new(sn: &SubnetID, addr: &Address) -> Result<Self, Error> {
        Ok(Self {
            subnet_id: sn.clone(),
            raw_address: *addr,
        })
    }

    /// Returns subnets of a IPC address
    pub fn subnet(&self) -> Result<SubnetID, Error> {
        Ok(self.subnet_id.clone())
    }

    /// Returns the raw address of a IPC address (without subnet context)
    pub fn raw_addr(&self) -> Result<Address, Error> {
        Ok(self.raw_address)
    }

    /// Returns encoded bytes of Address
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        Ok(cbor::serialize(self, "ipc-address")?.to_vec())
    }

    pub fn from_bytes(bz: &[u8]) -> Result<Self, Error> {
        let i: Self = cbor::deserialize(&RawBytes::new(bz.to_vec()), "ipc-address")?;
        Ok(i)
    }

    pub fn to_string(&self) -> Result<String, Error> {
        Ok(format!(
            "{}{}{}",
            self.subnet_id, IPC_SEPARATOR_ADDR, self.raw_address
        ))
    }
}

impl FromStr for IPCAddress {
    type Err = Error;

    fn from_str(addr: &str) -> Result<Self, Error> {
        let r: Vec<&str> = addr.split(IPC_SEPARATOR_ADDR).collect();
        if r.len() != 2 {
            Err(Error::InvalidIPCAddr)
        } else {
            Ok(Self {
                raw_address: Address::from_str(r[1])?,
                subnet_id: SubnetID::from_str(r[0])?,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::address::IPCAddress;
    use crate::subnet_id::{SubnetID, ROOTNET_ID};
    use fvm_shared::address::Address;
    use std::str::FromStr;

    #[test]
    fn test_ipc_address() {
        let act = Address::new_id(1001);
        let sub_id = SubnetID::new(&ROOTNET_ID.clone(), act);
        let bls = Address::from_str("f3vvmn62lofvhjd2ugzca6sof2j2ubwok6cj4xxbfzz4yuxfkgobpihhd2thlanmsh3w2ptld2gqkn2jvlss4a").unwrap();
        let haddr = IPCAddress::new(&sub_id, &bls).unwrap();

        let str = haddr.to_string().unwrap();

        let blss = IPCAddress::from_str(&str).unwrap();
        assert_eq!(haddr.raw_addr().unwrap(), bls);
        assert_eq!(haddr.subnet().unwrap(), sub_id);
        assert_eq!(haddr, blss);
    }

    #[test]
    fn test_ipc_from_str() {
        let sub_id = SubnetID::new(&ROOTNET_ID.clone(), Address::new_id(100));
        let addr = IPCAddress::new(&sub_id, &Address::new_id(101)).unwrap();
        let st = addr.to_string().unwrap();
        let addr_out = IPCAddress::from_str(&st).unwrap();
        assert_eq!(addr, addr_out);
    }

    #[test]
    fn test_ipc_serialization() {
        let sub_id = SubnetID::new(&ROOTNET_ID.clone(), Address::new_id(100));
        let addr = IPCAddress::new(&sub_id, &Address::new_id(101)).unwrap();
        let st = addr.to_bytes().unwrap();
        let addr_out = IPCAddress::from_bytes(&st).unwrap();
        assert_eq!(addr, addr_out);
    }
}
