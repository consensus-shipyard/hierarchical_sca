use cid::Cid;
use fil_actors_runtime::runtime::Runtime;
use fil_actors_runtime::test_utils::MockRuntime;
use fil_actors_runtime::BURNT_FUNDS_ACTOR_ADDR;
use fvm_ipld_encoding::RawBytes;
use fvm_ipld_hamt::BytesKey;
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::clock::ChainEpoch;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::METHOD_SEND;
use ipc_gateway::checkpoint::BatchCrossMsgs;
use ipc_gateway::Status::{Active, Inactive};
use ipc_gateway::{
    get_topdown_msg, Checkpoint, CronCheckpoint, CrossMsg, IPCAddress, PostBoxItem, State,
    StorableMsg, CROSS_MSG_FEE, DEFAULT_CHECKPOINT_PERIOD, SUBNET_ACTOR_REWARD_METHOD,
};
use ipc_sdk::subnet_id::SubnetID;
use ipc_sdk::vote::{EpochVoteSubmissions, UniqueVote};
use ipc_sdk::{Validator, ValidatorSet};
use primitives::TCid;
use std::collections::BTreeSet;
use std::ops::Mul;
use std::str::FromStr;

use crate::harness::*;
mod harness;

#[test]
fn construct() {
    let mut rt = new_runtime();
    let h = new_harness(ROOTNET_ID.clone());
    h.construct_and_verify(&mut rt);
    h.check_state();
}

#[test]
fn register_subnet() {
    let (h, mut rt) = setup_root();

    // Register a subnet with 1FIL collateral
    let mut value = TokenAmount::from_atto(10_u64.pow(18));
    h.register(&mut rt, &SUBNET_ONE, &value, ExitCode::OK)
        .unwrap();

    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 1);
    let shid = SubnetID::new_from_parent(&h.net_name, *SUBNET_ONE);
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.id, shid);
    assert_eq!(subnet.stake, value);
    assert_eq!(subnet.circ_supply, TokenAmount::zero());
    assert_eq!(subnet.status, Active);
    h.check_state();

    // Registering an already existing subnet should fail
    h.register(&mut rt, &SUBNET_ONE, &value, ExitCode::USR_ILLEGAL_ARGUMENT)
        .unwrap();
    h.check_state();
    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 1);

    // Registering without enough collateral.
    value = TokenAmount::from_atto(10_u64.pow(17));
    h.register(&mut rt, &SUBNET_ONE, &value, ExitCode::USR_ILLEGAL_ARGUMENT)
        .unwrap();
    h.check_state();
    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 1);

    // Register additional subnet
    value = TokenAmount::from_atto(12_i128.pow(18));
    h.register(&mut rt, &SUBNET_TWO, &value, ExitCode::OK)
        .unwrap();

    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 2);
    let shid = SubnetID::new_from_parent(&h.net_name, *SUBNET_TWO);
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.id, shid);
    assert_eq!(subnet.stake, value);
    assert_eq!(subnet.circ_supply, TokenAmount::zero());
    assert_eq!(subnet.status, Active);
    h.check_state();
}

#[test]
fn add_stake() {
    let (h, mut rt) = setup_root();

    // Register a subnet with 1FIL collateral
    let value = TokenAmount::from_atto(10_u64.pow(18));
    h.register(&mut rt, &SUBNET_ONE, &value, ExitCode::OK)
        .unwrap();

    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 1);
    let shid = SubnetID::new_from_parent(&h.net_name, *SUBNET_ONE);
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.id, shid);
    assert_eq!(subnet.stake, value);
    assert_eq!(subnet.circ_supply, TokenAmount::zero());
    assert_eq!(subnet.status, Active);
    h.check_state();

    // Add some stake
    h.add_stake(&mut rt, &shid, &value, ExitCode::OK).unwrap();
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.stake, value.clone().mul(2));

    // Add to unregistered subnet
    h.add_stake(
        &mut rt,
        &SubnetID::new_from_parent(&h.net_name, *SUBNET_TWO),
        &value,
        ExitCode::USR_ILLEGAL_ARGUMENT,
    )
    .unwrap();

    // Add some more stake
    h.add_stake(&mut rt, &shid, &value, ExitCode::OK).unwrap();
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.stake, value.clone().mul(3));

    // Add with zero value
    h.add_stake(
        &mut rt,
        &shid,
        &TokenAmount::zero(),
        ExitCode::USR_ILLEGAL_ARGUMENT,
    )
    .unwrap();
}

#[test]
fn release_stake() {
    let (h, mut rt) = setup_root();

    // Register a subnet with 1FIL collateral
    let value = TokenAmount::from_atto(10_u64.pow(18));
    h.register(&mut rt, &SUBNET_ONE, &value, ExitCode::OK)
        .unwrap();

    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 1);
    let shid = SubnetID::new_from_parent(&h.net_name, *SUBNET_ONE);
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.id, shid);
    assert_eq!(subnet.stake, value);
    assert_eq!(subnet.circ_supply, TokenAmount::zero());
    assert_eq!(subnet.status, Active);
    h.check_state();

    // Add some stake
    h.add_stake(&mut rt, &shid, &value, ExitCode::OK).unwrap();
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.stake, value.clone().mul(2));

    // Release some stake
    h.release_stake(&mut rt, &shid, &value, ExitCode::OK)
        .unwrap();
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.stake, value.clone());
    assert_eq!(subnet.status, Active);

    // Release from unregistered subnet
    h.release_stake(
        &mut rt,
        &SubnetID::new_from_parent(&h.net_name, *SUBNET_TWO),
        &value,
        ExitCode::USR_ILLEGAL_ARGUMENT,
    )
    .unwrap();

    // Release with zero value
    h.release_stake(
        &mut rt,
        &shid,
        &TokenAmount::zero(),
        ExitCode::USR_ILLEGAL_ARGUMENT,
    )
    .unwrap();

    // Release enough to inactivate
    rt.set_balance(value.clone().mul(2));
    h.release_stake(
        &mut rt,
        &shid,
        &TokenAmount::from_atto(5u64.pow(17)),
        ExitCode::OK,
    )
    .unwrap();
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.stake, &value - TokenAmount::from_atto(5u64.pow(17)));
    assert_eq!(subnet.status, Inactive);

    // Not enough funds to release
    h.release_stake(&mut rt, &shid, &value, ExitCode::USR_ILLEGAL_STATE)
        .unwrap();

    // Balance is not enough to release
    //, ExitCode::OK).unwrap();
    rt.set_balance(TokenAmount::zero());
    h.release_stake(
        &mut rt,
        &shid,
        &TokenAmount::from_atto(5u64.pow(17)),
        ExitCode::USR_ILLEGAL_STATE,
    )
    .unwrap();
}

#[test]
fn test_kill() {
    let (h, mut rt) = setup_root();

    // Register a subnet with 1FIL collateral
    let value = TokenAmount::from_atto(10_u64.pow(18));
    h.register(&mut rt, &SUBNET_ONE, &value, ExitCode::OK)
        .unwrap();

    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 1);
    let shid = SubnetID::new_from_parent(&h.net_name, *SUBNET_ONE);
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.id, shid);
    assert_eq!(subnet.stake, value);
    assert_eq!(subnet.circ_supply, TokenAmount::zero());
    assert_eq!(subnet.status, Active);
    h.check_state();

    // Add some stake
    h.kill(&mut rt, &shid, &value, ExitCode::OK).unwrap();
    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 0);
    assert!(h.get_subnet(&rt, &shid).is_none());
}

#[test]
fn checkpoint_commit() {
    let (h, mut rt) = setup_root();

    // Register a subnet with 1FIL collateral
    let value = TokenAmount::from_atto(10_u64.pow(18));
    h.register(&mut rt, &SUBNET_ONE, &value, ExitCode::OK)
        .unwrap();

    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 1);
    let shid = SubnetID::new_from_parent(&h.net_name, *SUBNET_ONE);
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.id, shid);
    assert_eq!(subnet.stake, value);
    assert_eq!(subnet.circ_supply, TokenAmount::zero());
    assert_eq!(subnet.status, Active);
    h.check_state();

    // Commit first checkpoint for first window in first subnet
    let epoch: ChainEpoch = 10;
    rt.set_epoch(epoch);
    let ch = Checkpoint::new(shid.clone(), epoch + 9);

    h.commit_child_check(&mut rt, &shid, &ch, ExitCode::OK)
        .unwrap();
    let st: State = rt.get_state();
    let commit = st.get_window_checkpoint(rt.store(), epoch).unwrap();
    assert_eq!(commit.epoch(), DEFAULT_CHECKPOINT_PERIOD);
    let child_check = has_childcheck_source(&commit.data.children, &shid).unwrap();
    assert_eq!(&child_check.checks.len(), &1);
    assert_eq!(has_cid(&child_check.checks, &ch.cid()), true);

    // Commit a checkpoint for subnet twice
    h.commit_child_check(&mut rt, &shid, &ch, ExitCode::USR_ILLEGAL_ARGUMENT)
        .unwrap();
    let prev_cid = ch.cid();

    // Append a new checkpoint for the same subnet
    let mut ch = Checkpoint::new(shid.clone(), epoch + 11);
    ch.data.prev_check = TCid::from(prev_cid);
    h.commit_child_check(&mut rt, &shid, &ch, ExitCode::OK)
        .unwrap();
    let st: State = rt.get_state();
    let commit = st.get_window_checkpoint(rt.store(), epoch).unwrap();
    assert_eq!(commit.epoch(), DEFAULT_CHECKPOINT_PERIOD);
    let child_check = has_childcheck_source(&commit.data.children, &shid).unwrap();
    assert_eq!(&child_check.checks.len(), &2);
    assert_eq!(has_cid(&child_check.checks, &ch.cid()), true);

    // Register second subnet
    h.register(&mut rt, &SUBNET_TWO, &value, ExitCode::OK)
        .unwrap();

    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 2);
    let shid_two = SubnetID::new_from_parent(&h.net_name, *SUBNET_TWO);
    let subnet = h.get_subnet(&rt, &shid_two).unwrap();
    assert_eq!(subnet.id, shid_two);
    h.check_state();

    // Trying to commit from the wrong subnet
    let ch = Checkpoint::new(shid.clone(), epoch + 9);
    h.commit_child_check(&mut rt, &shid_two, &ch, ExitCode::USR_ILLEGAL_ARGUMENT)
        .unwrap();

    // Commit first checkpoint for first window in second subnet
    let epoch: ChainEpoch = 10;
    rt.set_epoch(epoch);
    let ch = Checkpoint::new(shid_two.clone(), epoch + 9);

    h.commit_child_check(&mut rt, &shid_two, &ch, ExitCode::OK)
        .unwrap();
    let st: State = rt.get_state();
    let commit = st.get_window_checkpoint(rt.store(), epoch).unwrap();
    assert_eq!(commit.epoch(), DEFAULT_CHECKPOINT_PERIOD);
    let child_check = has_childcheck_source(&commit.data.children, &shid_two).unwrap();
    assert_eq!(&child_check.checks.len(), &1);
    assert_eq!(has_cid(&child_check.checks, &ch.cid()), true);
}

#[test]
fn checkpoint_crossmsgs() {
    let (h, mut rt) = setup_root();

    // Register a subnet with 1FIL collateral
    let value = TokenAmount::from_atto(10_u64.pow(18));
    h.register(&mut rt, &SUBNET_ONE, &value, ExitCode::OK)
        .unwrap();

    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 1);
    let shid = SubnetID::new_from_parent(&h.net_name, *SUBNET_ONE);
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.id, shid);
    assert_eq!(subnet.stake, value);
    assert_eq!(subnet.circ_supply, TokenAmount::zero());
    assert_eq!(subnet.status, Active);
    h.check_state();

    // found some to the subnet
    let funder = Address::new_id(1001);
    let amount = TokenAmount::from_atto(10_u64.pow(18));
    h.fund(
        &mut rt,
        &funder,
        &shid,
        ExitCode::OK,
        amount.clone(),
        1,
        &amount,
    )
    .unwrap();

    // Commit first checkpoint for first window in first subnet
    let epoch: ChainEpoch = 10;
    rt.set_epoch(epoch);
    let mut ch = Checkpoint::new(shid.clone(), epoch + 9);
    // and include some fees.
    let fee = TokenAmount::from_atto(5);
    ch.data.cross_msgs = BatchCrossMsgs {
        cross_msgs: None,
        fee: fee.clone(),
    };

    rt.expect_send(
        shid.subnet_actor(),
        SUBNET_ACTOR_REWARD_METHOD,
        None,
        fee,
        None,
        ExitCode::OK,
    );
    h.commit_child_check(&mut rt, &shid, &ch, ExitCode::OK)
        .unwrap();
    let st: State = rt.get_state();
    let commit = st.get_window_checkpoint(rt.store(), epoch).unwrap();
    assert_eq!(commit.epoch(), DEFAULT_CHECKPOINT_PERIOD);
    let child_check = has_childcheck_source(&commit.data.children, &shid).unwrap();
    assert_eq!(&child_check.checks.len(), &1);
    let prev_cid = ch.cid();
    assert_eq!(has_cid(&child_check.checks, &prev_cid), true);

    // TODO: More extensive tests?
}

#[test]
fn test_fund() {
    let (h, mut rt) = setup_root();

    // Register a subnet with 1FIL collateral
    let value = TokenAmount::from_atto(10_u64.pow(18));
    h.register(&mut rt, &SUBNET_ONE, &value, ExitCode::OK)
        .unwrap();

    let st: State = rt.get_state();
    assert_eq!(st.total_subnets, 1);
    let shid = SubnetID::new_from_parent(&h.net_name, *SUBNET_ONE);
    let subnet = h.get_subnet(&rt, &shid).unwrap();
    assert_eq!(subnet.id, shid);
    assert_eq!(subnet.stake, value);
    assert_eq!(subnet.circ_supply, TokenAmount::zero());
    assert_eq!(subnet.status, Active);
    h.check_state();

    let funder = Address::new_id(1001);
    let amount = TokenAmount::from_atto(10_u64.pow(18));
    h.fund(
        &mut rt,
        &funder,
        &shid,
        ExitCode::OK,
        amount.clone(),
        1,
        &amount,
    )
    .unwrap();
    let funder = Address::new_id(1002);
    let mut exp_cs = amount.clone() * 2;
    h.fund(
        &mut rt,
        &funder,
        &shid,
        ExitCode::OK,
        amount.clone(),
        2,
        &exp_cs,
    )
    .unwrap();
    exp_cs += amount.clone();
    h.fund(
        &mut rt,
        &funder,
        &shid,
        ExitCode::OK,
        amount.clone(),
        3,
        &exp_cs,
    )
    .unwrap();
    // No funds sent
    h.fund(
        &mut rt,
        &funder,
        &shid,
        ExitCode::USR_ILLEGAL_ARGUMENT,
        TokenAmount::zero(),
        3,
        &exp_cs,
    )
    .unwrap();

    // Subnet doesn't exist
    h.fund(
        &mut rt,
        &funder,
        &SubnetID::new_from_parent(&h.net_name, *SUBNET_TWO),
        ExitCode::USR_ILLEGAL_ARGUMENT,
        TokenAmount::zero(),
        3,
        &exp_cs,
    )
    .unwrap();
}

#[test]
fn test_release() {
    let shid = SubnetID::new_from_parent(&ROOTNET_ID, *SUBNET_ONE);
    let (h, mut rt) = setup(shid.clone());

    let releaser = Address::new_id(1001);
    // Release funds
    let r_amount = TokenAmount::from_atto(5_u64.pow(18));
    rt.set_balance(2 * r_amount.clone());
    h.release(&mut rt, &releaser, ExitCode::OK, r_amount.clone(), 0)
        .unwrap();
    h.release(&mut rt, &releaser, ExitCode::OK, r_amount, 1)
        .unwrap();
}

#[test]
fn test_send_cross() {
    let shid = SubnetID::new_from_parent(&ROOTNET_ID, *SUBNET_ONE);
    let (h, mut rt) = setup(shid.clone());

    let from = Address::new_id(1001);
    let to = Address::new_id(1002);

    let value = TokenAmount::from_atto(10_u64.pow(18));

    // register subnet
    let reg_value = TokenAmount::from_atto(10_u64.pow(18));
    h.register(&mut rt, &SUBNET_ONE, &reg_value, ExitCode::OK)
        .unwrap();

    // top-down
    let sub = SubnetID::from_str("/root/t0101/t0101").unwrap();
    h.send_cross(
        &mut rt,
        &from,
        &shid,
        &to,
        sub,
        ExitCode::OK,
        value.clone(),
        1,
        &value,
    )
    .unwrap();
    let sub = SubnetID::from_str("/root/t0101/t0101").unwrap();
    let circ_sup = 2 * &value;
    h.send_cross(
        &mut rt,
        &from,
        &shid,
        &to,
        sub,
        ExitCode::OK,
        value.clone(),
        2,
        &circ_sup,
    )
    .unwrap();
    let sub = SubnetID::from_str("/root/t0101/t0101/t01002").unwrap();
    let circ_sup = circ_sup.clone() + &value;
    h.send_cross(
        &mut rt,
        &from,
        &shid,
        &to,
        sub,
        ExitCode::OK,
        value.clone(),
        3,
        &circ_sup,
    )
    .unwrap();

    // bottom-up
    rt.set_balance(3 * &value);
    let sub = SubnetID::from_str("/root/t0102/t0101").unwrap();
    let zero = TokenAmount::zero();
    h.send_cross(
        &mut rt,
        &from,
        &shid,
        &to,
        sub,
        ExitCode::OK,
        value.clone(),
        0,
        &zero,
    )
    .unwrap();
    let sub = SubnetID::from_str("/root/t0102/t0101").unwrap();
    h.send_cross(
        &mut rt,
        &from,
        &shid,
        &to,
        sub,
        ExitCode::OK,
        value.clone(),
        1,
        &zero,
    )
    .unwrap();
    let sub = SubnetID::from_str("/root").unwrap();
    h.send_cross(
        &mut rt,
        &from,
        &shid,
        &to,
        sub,
        ExitCode::OK,
        value.clone(),
        2,
        &zero,
    )
    .unwrap();
}

/// This test covers the case where a bottom up cross_msg's target subnet is the SAME as that of
/// the gateway. It should directly commit the message and will not save in postbox.
#[test]
fn test_commit_child_check_bu_target_subnet() {
    // ============== Register subnet ==============
    let shid = SubnetID::new_from_parent(&ROOTNET_ID, *SUBNET_ONE);
    let (h, mut rt) = setup(ROOTNET_ID.clone());

    h.register(
        &mut rt,
        &SUBNET_ONE,
        &TokenAmount::from_atto(10_u64.pow(18)),
        ExitCode::OK,
    )
    .unwrap();
    h.fund(
        &mut rt,
        &Address::new_id(1001),
        &shid,
        ExitCode::OK,
        TokenAmount::from_atto(10_u64.pow(18)),
        1,
        &TokenAmount::from_atto(10_u64.pow(18)),
    )
    .unwrap();

    let from = Address::new_bls(&[3; fvm_shared::address::BLS_PUB_LEN]).unwrap();
    let to = Address::new_bls(&[4; fvm_shared::address::BLS_PUB_LEN]).unwrap();

    let sub = shid.clone();

    // ================ Setup ===============
    let value = TokenAmount::from_atto(10_u64.pow(17));

    // ================= Bottom-Up ===============
    let ff = IPCAddress::new(&sub, &to).unwrap();
    let tt = IPCAddress::new(&ROOTNET_ID, &from).unwrap();
    let msg_nonce = 0;

    // Only system code is allowed to this method
    let msg = StorableMsg {
        to: tt.clone(),
        from: ff.clone(),
        method: METHOD_SEND,
        value: value.clone(),
        params: RawBytes::default(),
        nonce: msg_nonce,
    };

    let epoch: ChainEpoch = 10;
    rt.set_epoch(epoch);
    let mut ch = Checkpoint::new(shid.clone(), epoch + 9);
    // and include some fees.
    let fee = TokenAmount::from_atto(5);
    ch.data.cross_msgs = BatchCrossMsgs {
        cross_msgs: Some(vec![CrossMsg {
            msg: msg.clone(),
            wrapped: false,
        }]),
        fee: fee.clone(),
    };

    // execute bottom up
    rt.expect_send(
        msg.to.raw_addr().unwrap(),
        msg.method,
        None,
        msg.value,
        None,
        ExitCode::OK,
    );
    // distribute fee
    rt.expect_send(
        shid.subnet_actor(),
        SUBNET_ACTOR_REWARD_METHOD,
        None,
        fee,
        None,
        ExitCode::OK,
    );
    h.commit_child_check(&mut rt, &shid, &ch, ExitCode::OK)
        .unwrap();
}

/// This test covers the case where a bottom up cross_msg's target subnet is NOT the same as that of
/// the gateway. It will save it in the postbox.
#[test]
fn test_commit_child_check_bu_not_target_subnet() {
    // ============== Register subnet ==============
    let parent = SubnetID::new_from_parent(&ROOTNET_ID, *SUBNET_ONE);
    let shid = SubnetID::new_from_parent(&parent, *SUBNET_TWO);
    let (h, mut rt) = setup(parent);

    h.register(
        &mut rt,
        &shid.subnet_actor(),
        &TokenAmount::from_atto(10_u64.pow(18)),
        ExitCode::OK,
    )
    .unwrap();
    h.fund(
        &mut rt,
        &Address::new_id(1001),
        &shid,
        ExitCode::OK,
        TokenAmount::from_atto(10_u64.pow(18)),
        1,
        &TokenAmount::from_atto(10_u64.pow(18)),
    )
    .unwrap();

    let from = Address::new_bls(&[3; fvm_shared::address::BLS_PUB_LEN]).unwrap();
    let to = Address::new_bls(&[4; fvm_shared::address::BLS_PUB_LEN]).unwrap();

    // ================ Setup ===============
    let value = TokenAmount::from_atto(10_u64.pow(17));

    // ================= Bottom-Up ===============
    let ff = IPCAddress::new(&shid.clone(), &to).unwrap();
    let tt = IPCAddress::new(&ROOTNET_ID, &from).unwrap();
    let msg_nonce = 0;

    // Only system code is allowed to this method
    let msg = StorableMsg {
        to: tt.clone(),
        from: ff.clone(),
        method: METHOD_SEND,
        value: value.clone(),
        params: RawBytes::default(),
        nonce: msg_nonce,
    };

    let epoch: ChainEpoch = 10;
    rt.set_epoch(epoch);
    let mut ch = Checkpoint::new(shid.clone(), epoch + 9);
    // and include some fees.
    let fee = TokenAmount::from_atto(5);
    ch.data.cross_msgs = BatchCrossMsgs {
        cross_msgs: Some(vec![CrossMsg {
            msg: msg.clone(),
            wrapped: false,
        }]),
        fee: fee.clone(),
    };

    // distribute fee
    rt.expect_send(
        shid.subnet_actor(),
        SUBNET_ACTOR_REWARD_METHOD,
        None,
        fee,
        None,
        ExitCode::OK,
    );
    h.commit_child_check(&mut rt, &shid, &ch, ExitCode::OK)
        .unwrap();

    // Part 1: test the message is stored in postbox
    let st: State = rt.get_state();
    assert_ne!(tt.subnet().unwrap(), st.network_name);

    // Check 1: `tt` is in `parent`, which is not in that of `runtime` of gateway, will store in postbox
    let postbox = st.postbox.load(rt.store()).unwrap();
    let mut cid = None;
    postbox
        .for_each(|k, v| {
            let item = PostBoxItem::deserialize(v.clone()).unwrap();
            assert_eq!(item.owners, Some(vec![ff.clone().raw_addr().unwrap()]));
            let msg = item.cross_msg.msg;
            assert_eq!(msg.to, tt);
            // the nonce should not have changed at all
            assert_eq!(msg.nonce, msg_nonce);
            assert_eq!(msg.value, value);

            cid = Some(Cid::try_from(k.clone().to_vec()).unwrap());
            Ok(())
        })
        .unwrap();

    // Part 2: Now we propagate from postbox
    // get the original subnet nonce first
    let caller = ff.clone().raw_addr().unwrap();
    // propagating a bottom-up message triggers the
    // funds included in the message to be burnt.
    rt.expect_send(
        BURNT_FUNDS_ACTOR_ADDR,
        METHOD_SEND,
        None,
        msg.clone().value,
        None,
        ExitCode::OK,
    );
    h.propagate(
        &mut rt,
        caller,
        cid.unwrap().clone(),
        &msg.value,
        TokenAmount::zero(),
    )
    .unwrap();

    // state should be updated, load again
    let new_state: State = rt.get_state();

    // cid should be removed from postbox
    let r = new_state.load_from_postbox(rt.store(), cid.unwrap());
    assert_eq!(r.is_err(), true);
    let err = r.unwrap_err();
    assert_eq!(err.to_string(), "cid not found in postbox");
}

/// This test covers the case where the amount send in the propagate
/// message exceeds the required fee and the remainder is returned
/// to the caller.
#[test]
fn test_propagate_with_remainder() {
    // ============== Register subnet ==============
    let parent = SubnetID::new_from_parent(&ROOTNET_ID, *SUBNET_ONE);
    let shid = SubnetID::new_from_parent(&parent, *SUBNET_TWO);

    let (h, mut rt) = setup(parent);
    h.register(
        &mut rt,
        &shid.subnet_actor(),
        &TokenAmount::from_atto(10_u64.pow(18)),
        ExitCode::OK,
    )
    .unwrap();
    h.fund(
        &mut rt,
        &Address::new_id(1001),
        &shid,
        ExitCode::OK,
        TokenAmount::from_atto(10_u64.pow(18)),
        1,
        &TokenAmount::from_atto(10_u64.pow(18)),
    )
    .unwrap();

    let from = Address::new_bls(&[3; fvm_shared::address::BLS_PUB_LEN]).unwrap();
    let to = Address::new_bls(&[4; fvm_shared::address::BLS_PUB_LEN]).unwrap();

    let sub = shid.clone();

    // ================ Setup ===============
    let value = TokenAmount::from_atto(10_u64.pow(17));

    // ================= Bottom-Up ===============
    let ff = IPCAddress::new(&sub, &to).unwrap();
    let tt = IPCAddress::new(&ROOTNET_ID, &from).unwrap();
    let msg_nonce = 0;

    // Only system code is allowed to this method
    let params = StorableMsg {
        to: tt.clone(),
        from: ff.clone(),
        method: METHOD_SEND,
        value: value.clone(),
        params: RawBytes::default(),
        nonce: msg_nonce,
    };

    let epoch: ChainEpoch = 10;
    rt.set_epoch(epoch);
    let mut ch = Checkpoint::new(shid.clone(), epoch + 9);
    // and include some fees.
    let fee = TokenAmount::from_atto(5);
    ch.data.cross_msgs = BatchCrossMsgs {
        cross_msgs: Some(vec![CrossMsg {
            msg: params.clone(),
            wrapped: false,
        }]),
        fee: fee.clone(),
    };

    // distribute fee
    rt.expect_send(
        shid.subnet_actor(),
        SUBNET_ACTOR_REWARD_METHOD,
        None,
        fee,
        None,
        ExitCode::OK,
    );
    h.commit_child_check(&mut rt, &shid, &ch, ExitCode::OK)
        .unwrap();

    // Part 1: test the message is stored in postbox
    let st: State = rt.get_state();
    assert_ne!(tt.subnet().unwrap(), st.network_name);

    // Check 1: `tt` is in `parent`, which is not in that of `runtime` of gateway, will store in postbox
    let postbox = st.postbox.load(rt.store()).unwrap();
    let mut cid = None;
    postbox
        .for_each(|k, v| {
            let item = PostBoxItem::deserialize(v.clone()).unwrap();
            assert_eq!(item.owners, Some(vec![ff.clone().raw_addr().unwrap()]));
            let msg = item.cross_msg.msg;
            assert_eq!(msg.to, tt);
            // the nonce should not have changed at all
            assert_eq!(msg.nonce, msg_nonce);
            assert_eq!(msg.value, value);

            cid = Some(Cid::try_from(k.clone().to_vec()).unwrap());
            Ok(())
        })
        .unwrap();

    // Part 2: Now we propagate from postbox
    // get the original subnet nonce first with an
    // excess to check that there is a remainder
    // to be returned
    let caller = ff.clone().raw_addr().unwrap();
    // propagating a bottom-up message triggers the
    // funds included in the message to be burnt.
    rt.expect_send(
        BURNT_FUNDS_ACTOR_ADDR,
        METHOD_SEND,
        None,
        params.clone().value,
        None,
        ExitCode::OK,
    );
    h.propagate(
        &mut rt,
        caller,
        cid.clone().unwrap(),
        &params.value,
        value.clone(),
    )
    .unwrap();

    // state should be updated, load again
    let new_state: State = rt.get_state();

    // cid should be removed from postbox
    let r = new_state.load_from_postbox(rt.store(), cid.unwrap());
    assert_eq!(r.is_err(), true);
    let err = r.unwrap_err();
    assert_eq!(err.to_string(), "cid not found in postbox");
}

/// This test covers the case where a bottom up cross_msg's target subnet is NOT the same as that of
/// the gateway. It would save in postbox. Also, the gateway is the nearest parent, a switch to
/// top down cross msg should occur.
#[test]
fn test_commit_child_check_bu_switch_td() {
    // ============== Register subnet ==============
    let parent_sub = SubnetID::new_from_parent(&ROOTNET_ID, *SUBNET_ONE);
    let (h, mut rt) = setup(parent_sub.clone());

    let from = Address::new_bls(&[3; fvm_shared::address::BLS_PUB_LEN]).unwrap();
    let to = Address::new_bls(&[4; fvm_shared::address::BLS_PUB_LEN]).unwrap();

    // ================ Setup ===============
    let value = TokenAmount::from_atto(10_u64.pow(17));

    // ================= Bottom-Up ===============
    let reg_value = TokenAmount::from_atto(10_u64.pow(18));
    // ff: /root/f101/f102
    // to: /root/f101/f103
    // we are executing the message from, harness or the gateway is at: /root/f101
    let ff_sub = SubnetID::new_from_parent(&parent_sub, *SUBNET_TWO);
    let tt_sub = SubnetID::new_from_parent(&parent_sub, *SUBNET_THR);
    h.register(&mut rt, &SUBNET_TWO, &reg_value, ExitCode::OK)
        .unwrap();
    h.register(&mut rt, &SUBNET_THR, &reg_value, ExitCode::OK)
        .unwrap();

    let ff = IPCAddress::new(&ff_sub, &to).unwrap();
    let tt = IPCAddress::new(&tt_sub, &from).unwrap();
    let msg_nonce = 0;

    // Only system code is allowed to this method
    let params = StorableMsg {
        to: tt.clone(),
        from: ff.clone(),
        method: METHOD_SEND,
        value: value.clone(),
        params: RawBytes::default(),
        nonce: msg_nonce,
    };

    let caller = ff.clone().raw_addr().unwrap();

    // we directly insert message into postbox as we dont really care how it's got stored in queue
    let cid = rt
        .transaction(|st: &mut State, r| {
            Ok(st
                .insert_postbox(
                    r.store(),
                    Some(vec![caller.clone()]),
                    CrossMsg {
                        wrapped: false,
                        msg: params.clone(),
                    },
                )
                .unwrap())
        })
        .unwrap();

    let starting_nonce = get_subnet(&rt, &tt.subnet().unwrap().down(&h.net_name).unwrap())
        .unwrap()
        .topdown_nonce;

    // propagated as top-down, so it should distribute a fee in this subnet
    rt.expect_send(
        tt.subnet()
            .unwrap()
            .down(&h.net_name)
            .unwrap()
            .subnet_actor(),
        SUBNET_ACTOR_REWARD_METHOD,
        None,
        CROSS_MSG_FEE.clone(),
        None,
        ExitCode::OK,
    );

    // now we propagate
    h.propagate(
        &mut rt,
        caller,
        cid.clone(),
        &params.value,
        TokenAmount::zero(),
    )
    .unwrap();

    // state should be updated, load again to perform the checks!
    let st: State = rt.get_state();

    // cid should be removed from postbox
    let r = st.load_from_postbox(rt.store(), cid.clone());
    assert_eq!(r.is_err(), true);
    let err = r.unwrap_err();
    assert_eq!(err.to_string(), "cid not found in postbox");

    // the cross msg should have been committed to the next subnet, check this!
    let sub = get_subnet(&rt, &tt.subnet().unwrap().down(&h.net_name).unwrap()).unwrap();
    assert_eq!(sub.topdown_nonce, starting_nonce + 1);
    let crossmsgs = sub.top_down_msgs.load(rt.store()).unwrap();
    let msg = get_topdown_msg(&crossmsgs, starting_nonce).unwrap();
    assert_eq!(msg.is_some(), true);
    let msg = msg.unwrap();
    assert_eq!(msg.to, tt);
    // the nonce should not have changed at all
    assert_eq!(msg.nonce, starting_nonce);
    assert_eq!(msg.value, value);
}

/// This test covers the case where the cross_msg's target subnet is the SAME as that of
/// the gateway. It would directly commit the message and will not save in postbox.
#[test]
fn test_commit_child_check_tp_target_subnet() {
    // ============== Register subnet ==============
    let shid = SubnetID::new_from_parent(&ROOTNET_ID, *SUBNET_ONE);
    let (h, mut rt) = setup(ROOTNET_ID.clone());

    h.register(
        &mut rt,
        &SUBNET_ONE,
        &TokenAmount::from_atto(10_u64.pow(18)),
        ExitCode::OK,
    )
    .unwrap();
    h.fund(
        &mut rt,
        &Address::new_id(1001),
        &shid,
        ExitCode::OK,
        TokenAmount::from_atto(10_u64.pow(18)),
        1,
        &TokenAmount::from_atto(10_u64.pow(18)),
    )
    .unwrap();

    let from = Address::new_bls(&[3; fvm_shared::address::BLS_PUB_LEN]).unwrap();
    let to = Address::new_bls(&[4; fvm_shared::address::BLS_PUB_LEN]).unwrap();

    // ================ Setup ===============
    let value = TokenAmount::from_atto(10_u64.pow(17));

    // ================= Top-Down ===============
    let ff = IPCAddress::new(&ROOTNET_ID, &from).unwrap();
    let tt = IPCAddress::new(&shid.clone(), &to).unwrap();
    let msg_nonce = 0;

    // Only system code is allowed to this method
    let params = StorableMsg {
        to: tt.clone(),
        from: ff.clone(),
        method: METHOD_SEND,
        value: value.clone(),
        params: RawBytes::default(),
        nonce: msg_nonce,
    };
    let epoch: ChainEpoch = 10;
    rt.set_epoch(epoch);
    let mut ch = Checkpoint::new(shid.clone(), epoch + 9);
    // and include some fees.
    let fee = TokenAmount::from_atto(5);
    ch.data.cross_msgs = BatchCrossMsgs {
        cross_msgs: Some(vec![CrossMsg {
            msg: params.clone(),
            wrapped: false,
        }]),
        fee: fee.clone(),
    };

    // distribute fee
    rt.expect_send(
        shid.subnet_actor(),
        SUBNET_ACTOR_REWARD_METHOD,
        None,
        fee,
        None,
        ExitCode::OK,
    );
    h.commit_child_check(&mut rt, &shid, &ch, ExitCode::OK)
        .unwrap();
}

/// This test covers the case where the cross_msg's target subnet is not the same as that of
/// the gateway.
#[test]
fn test_commit_child_check_tp_not_target_subnet() {
    // ============== Define Parameters ==============
    // gateway: /root/sub1
    let shid = SubnetID::new_from_parent(&ROOTNET_ID, *SUBNET_ONE);

    let from = Address::new_bls(&[3; fvm_shared::address::BLS_PUB_LEN]).unwrap();
    let to = Address::new_bls(&[4; fvm_shared::address::BLS_PUB_LEN]).unwrap();

    // /root/sub1/sub1
    let sub = SubnetID::new_from_parent(&shid, *SUBNET_ONE);

    // ================ Setup ===============
    let reg_value = TokenAmount::from_atto(10_u64.pow(18));
    let (h, mut rt) = setup(shid.clone());
    h.register(&mut rt, &SUBNET_ONE, &reg_value, ExitCode::OK)
        .unwrap();
    // add some circulating supply to subnets
    let funder = Address::new_id(1002);
    h.fund(
        &mut rt,
        &funder,
        &sub,
        ExitCode::OK,
        reg_value.clone(),
        1,
        &reg_value,
    )
    .unwrap();

    let value = TokenAmount::from_atto(10_u64.pow(17));

    // ================= Top-Down ===============
    let ff = IPCAddress::new(&ROOTNET_ID, &from).unwrap();
    let tt = IPCAddress::new(&sub, &to).unwrap();
    let msg_nonce = 0;

    // Only system code is allowed to this method
    let params = StorableMsg {
        to: tt.clone(),
        from: ff.clone(),
        method: METHOD_SEND,
        value: value.clone(),
        params: RawBytes::default(),
        nonce: msg_nonce,
    };
    let epoch: ChainEpoch = 10;
    rt.set_epoch(epoch);
    let mut ch = Checkpoint::new(shid.clone(), epoch + 9);
    // and include some fees.
    let fee = TokenAmount::from_atto(5);
    ch.data.cross_msgs = BatchCrossMsgs {
        cross_msgs: Some(vec![CrossMsg {
            msg: params.clone(),
            wrapped: false,
        }]),
        fee: fee.clone(),
    };

    // distribute fee
    rt.expect_send(
        shid.subnet_actor(),
        SUBNET_ACTOR_REWARD_METHOD,
        None,
        fee,
        None,
        ExitCode::OK,
    );
    h.commit_child_check(&mut rt, &shid, &ch, ExitCode::OK)
        .unwrap();

    // Part 1: test the message is stored in postbox
    let st: State = rt.get_state();
    assert_ne!(tt.subnet().unwrap(), st.network_name);

    // Check 1: `tt` is in `parent`, which is not in that of `runtime` of gateway, will store in postbox
    let postbox = st.postbox.load(rt.store()).unwrap();
    let mut cid = None;
    postbox
        .for_each(|k, v| {
            let item = PostBoxItem::deserialize(v.clone()).unwrap();
            assert_eq!(item.owners, Some(vec![ff.clone().raw_addr().unwrap()]));
            let msg = item.cross_msg.msg;
            assert_eq!(msg.to, tt);
            // the nonce should not have changed at all
            assert_eq!(msg.nonce, msg_nonce);
            assert_eq!(msg.value, value);

            cid = Some(Cid::try_from(k.clone().to_vec()).unwrap());
            Ok(())
        })
        .unwrap();

    // Part 2: Now we propagate from postbox
    // get the original subnet nonce first
    let starting_nonce = get_subnet(&rt, &tt.subnet().unwrap().down(&h.net_name).unwrap())
        .unwrap()
        .topdown_nonce;
    let caller = ff.clone().raw_addr().unwrap();

    // propagated as top-down, so it should distribute a fee in this subnet
    rt.expect_send(
        tt.subnet()
            .unwrap()
            .down(&h.net_name)
            .unwrap()
            .subnet_actor(),
        SUBNET_ACTOR_REWARD_METHOD,
        None,
        CROSS_MSG_FEE.clone(),
        None,
        ExitCode::OK,
    );

    h.propagate(
        &mut rt,
        caller,
        cid.clone().unwrap(),
        &params.value,
        TokenAmount::zero(),
    )
    .unwrap();

    // state should be updated, load again
    let st: State = rt.get_state();

    // cid should be removed from postbox
    let r = st.load_from_postbox(rt.store(), cid.unwrap());
    assert_eq!(r.is_err(), true);
    let err = r.unwrap_err();
    assert_eq!(err.to_string(), "cid not found in postbox");

    // the cross msg should have been committed to the next subnet, check this!
    let sub = get_subnet(&rt, &tt.subnet().unwrap().down(&h.net_name).unwrap()).unwrap();
    assert_eq!(sub.topdown_nonce, starting_nonce + 1);
    let crossmsgs = sub.top_down_msgs.load(rt.store()).unwrap();
    let msg = get_topdown_msg(&crossmsgs, starting_nonce).unwrap();
    assert_eq!(msg.is_some(), true);
    let msg = msg.unwrap();
    assert_eq!(msg.to, tt);
    // the nonce should not have changed at all
    assert_eq!(msg.nonce, starting_nonce);
    assert_eq!(msg.value, value);
}

#[test]
fn test_set_membership() {
    let (h, mut rt) = setup_root();

    let weights = vec![1000, 2000];
    let mut index = 0;
    let validators = weights
        .iter()
        .map(|weight| {
            let v = Validator {
                addr: Address::new_id(index),
                net_addr: index.to_string(),
                weight: TokenAmount::from_atto(*weight),
            };
            index += 1;
            v
        })
        .collect();
    let validator_set = ValidatorSet::new(validators, 10);
    h.set_membership(&mut rt, validator_set.clone()).unwrap();

    let st: State = rt.get_state();

    assert_eq!(st.validators.validators, validator_set);
    assert_eq!(
        st.validators.total_weight,
        TokenAmount::from_atto(weights.iter().sum::<u64>())
    );
}

fn setup_membership(h: &Harness, rt: &mut MockRuntime) {
    let weights = vec![1000; 5];
    let mut index = 0;
    let validators = weights
        .iter()
        .map(|weight| {
            let v = Validator {
                addr: Address::new_id(index),
                net_addr: index.to_string(),
                weight: TokenAmount::from_atto(*weight),
            };
            index += 1;
            v
        })
        .collect();
    let validator_set = ValidatorSet::new(validators, 10);
    h.set_membership(rt, validator_set.clone()).unwrap();
}

#[test]
fn test_submit_cron_checking_errors() {
    let (h, mut rt) = setup_root();

    setup_membership(&h, &mut rt);

    let submitter = Address::new_id(10000);
    let checkpoint = CronCheckpoint {
        epoch: *DEFAULT_GENESIS_EPOCH + 1,
        top_down_msgs: vec![],
    };
    let r = h.submit_cron(&mut rt, submitter, checkpoint);
    assert!(r.is_err());
    assert_eq!(r.unwrap_err().msg(), "epoch not allowed");

    let checkpoint = CronCheckpoint {
        epoch: *DEFAULT_GENESIS_EPOCH,
        top_down_msgs: vec![],
    };
    let r = h.submit_cron(&mut rt, submitter, checkpoint);
    assert!(r.is_err());
    assert_eq!(r.unwrap_err().msg(), "epoch already executed");

    let checkpoint = CronCheckpoint {
        epoch: *DEFAULT_GENESIS_EPOCH + *DEFAULT_CRON_PERIOD,
        top_down_msgs: vec![],
    };
    let r = h.submit_cron(&mut rt, submitter, checkpoint);
    assert!(r.is_err());
    assert_eq!(r.unwrap_err().msg(), "caller not validator");
}

fn get_epoch_submissions(
    rt: &mut MockRuntime,
    epoch: ChainEpoch,
) -> Option<EpochVoteSubmissions<CronCheckpoint>> {
    let st: State = rt.get_state();
    let hamt = st
        .cron_checkpoint_voting
        .epoch_vote_submissions()
        .load(rt.store())
        .unwrap();
    let bytes_key = BytesKey::from(epoch.to_be_bytes().as_slice());
    hamt.get(&bytes_key).unwrap().cloned()
}

#[test]
fn test_submit_cron_works_with_execution() {
    let (h, mut rt) = setup_root();

    setup_membership(&h, &mut rt);

    let epoch = *DEFAULT_GENESIS_EPOCH + *DEFAULT_CRON_PERIOD;
    let msg = storable_msg(0);
    let checkpoint = CronCheckpoint {
        epoch,
        top_down_msgs: vec![msg.clone()],
    };

    // first submission
    let submitter = Address::new_id(0);
    let r = h.submit_cron(&mut rt, submitter, checkpoint.clone());
    assert!(r.is_ok());
    let submission = get_epoch_submissions(&mut rt, epoch).unwrap();
    assert_eq!(
        submission
            .get_submission(rt.store(), &checkpoint.unique_key().unwrap())
            .unwrap()
            .unwrap(),
        checkpoint
    );
    let st: State = rt.get_state();
    assert_eq!(
        st.cron_checkpoint_voting.last_voting_executed_epoch(),
        *DEFAULT_GENESIS_EPOCH
    ); // not executed yet

    // already submitted
    let submitter = Address::new_id(0);
    let r = h.submit_cron(&mut rt, submitter, checkpoint.clone());
    assert!(r.is_err());
    assert_eq!(r.unwrap_err().msg(), "already submitted");

    // second submission
    let submitter = Address::new_id(1);
    let r = h.submit_cron(&mut rt, submitter, checkpoint.clone());
    assert!(r.is_ok());
    let submission = get_epoch_submissions(&mut rt, epoch).unwrap();
    assert_eq!(
        submission
            .get_submission(rt.store(), &checkpoint.unique_key().unwrap())
            .unwrap()
            .unwrap(),
        checkpoint
    );
    let st: State = rt.get_state();
    assert_eq!(
        st.cron_checkpoint_voting.last_voting_executed_epoch(),
        *DEFAULT_GENESIS_EPOCH
    ); // not executed yet

    // third submission
    let submitter = Address::new_id(2);
    let r = h.submit_cron(&mut rt, submitter, checkpoint.clone());
    assert!(r.is_ok());
    let submission = get_epoch_submissions(&mut rt, epoch).unwrap();
    assert_eq!(
        submission
            .get_submission(rt.store(), &checkpoint.unique_key().unwrap())
            .unwrap()
            .unwrap(),
        checkpoint
    );
    let st: State = rt.get_state();
    assert_eq!(
        st.cron_checkpoint_voting.last_voting_executed_epoch(),
        *DEFAULT_GENESIS_EPOCH
    ); // not executed yet

    // fourth submission, executed
    let submitter = Address::new_id(3);
    rt.expect_send(
        msg.to.raw_addr().unwrap(),
        msg.method,
        None,
        msg.value,
        None,
        ExitCode::OK,
    );
    let r = h.submit_cron(&mut rt, submitter, checkpoint.clone());
    assert!(r.is_ok());
    let submission = get_epoch_submissions(&mut rt, epoch);
    assert!(submission.is_none());
    let st: State = rt.get_state();
    assert_eq!(
        st.cron_checkpoint_voting.last_voting_executed_epoch(),
        epoch
    );
}

fn storable_msg(nonce: u64) -> StorableMsg {
    StorableMsg {
        from: IPCAddress::new(&ROOTNET_ID, &Address::new_id(10)).unwrap(),
        to: IPCAddress::new(&ROOTNET_ID, &Address::new_id(20)).unwrap(),
        method: 0,
        params: Default::default(),
        value: Default::default(),
        nonce,
    }
}

#[test]
fn test_submit_cron_abort() {
    let (h, mut rt) = setup_root();

    setup_membership(&h, &mut rt);

    let epoch = *DEFAULT_GENESIS_EPOCH + *DEFAULT_CRON_PERIOD;

    // first submission
    let submitter = Address::new_id(0);
    let checkpoint = CronCheckpoint {
        epoch,
        top_down_msgs: vec![],
    };
    let r = h.submit_cron(&mut rt, submitter, checkpoint.clone());
    assert!(r.is_ok());

    // second submission
    let submitter = Address::new_id(1);
    let checkpoint = CronCheckpoint {
        epoch,
        top_down_msgs: vec![storable_msg(1)],
    };
    let r = h.submit_cron(&mut rt, submitter, checkpoint.clone());
    assert!(r.is_ok());

    // third submission
    let submitter = Address::new_id(2);
    let checkpoint = CronCheckpoint {
        epoch,
        top_down_msgs: vec![storable_msg(1), storable_msg(2)],
    };
    let r = h.submit_cron(&mut rt, submitter, checkpoint.clone());
    assert!(r.is_ok());

    // fourth submission, aborted
    let submitter = Address::new_id(3);
    let checkpoint = CronCheckpoint {
        epoch,
        top_down_msgs: vec![storable_msg(1), storable_msg(2), storable_msg(3)],
    };
    let r = h.submit_cron(&mut rt, submitter, checkpoint.clone());
    assert!(r.is_ok());

    // check aborted
    let st: State = rt.get_state();
    assert_eq!(
        st.cron_checkpoint_voting.last_voting_executed_epoch(),
        *DEFAULT_GENESIS_EPOCH
    ); // not executed yet
    let submission = get_epoch_submissions(&mut rt, epoch).unwrap();
    for i in 0..4 {
        assert_eq!(
            submission
                .has_submitted(rt.store(), &Address::new_id(i))
                .unwrap(),
            false
        );
    }
}

#[test]
fn test_submit_cron_sequential_execution() {
    let (h, mut rt) = setup_root();

    setup_membership(&h, &mut rt);

    let pending_epoch = *DEFAULT_GENESIS_EPOCH + *DEFAULT_CRON_PERIOD * 2;
    let checkpoint = CronCheckpoint {
        epoch: pending_epoch,
        top_down_msgs: vec![],
    };

    // first submission
    let submitter = Address::new_id(0);
    h.submit_cron(&mut rt, submitter, checkpoint.clone())
        .unwrap();

    // second submission
    let submitter = Address::new_id(1);
    h.submit_cron(&mut rt, submitter, checkpoint.clone())
        .unwrap();

    // third submission
    let submitter = Address::new_id(2);
    h.submit_cron(&mut rt, submitter, checkpoint.clone())
        .unwrap();

    // fourth submission, not executed
    let submitter = Address::new_id(3);
    h.submit_cron(&mut rt, submitter, checkpoint.clone())
        .unwrap();
    let st: State = rt.get_state();
    assert_eq!(
        st.cron_checkpoint_voting.last_voting_executed_epoch(),
        *DEFAULT_GENESIS_EPOCH
    ); // not executed yet
    assert_eq!(
        *st.cron_checkpoint_voting.executable_epoch_queue(),
        Some(BTreeSet::from([pending_epoch]))
    ); // not executed yet

    // now we execute the previous epoch
    let msg = storable_msg(0);
    let epoch = *DEFAULT_GENESIS_EPOCH + *DEFAULT_CRON_PERIOD;
    let checkpoint = CronCheckpoint {
        epoch,
        top_down_msgs: vec![msg.clone()],
    };

    // first submission
    let submitter = Address::new_id(0);
    h.submit_cron(&mut rt, submitter, checkpoint.clone())
        .unwrap();
    // second submission
    let submitter = Address::new_id(1);
    h.submit_cron(&mut rt, submitter, checkpoint.clone())
        .unwrap();
    // third submission
    let submitter = Address::new_id(2);
    h.submit_cron(&mut rt, submitter, checkpoint.clone())
        .unwrap();
    // fourth submission, executed
    let submitter = Address::new_id(3);
    // define expected send
    rt.expect_send(
        msg.to.raw_addr().unwrap(),
        msg.method,
        None,
        msg.value,
        None,
        ExitCode::OK,
    );
    h.submit_cron(&mut rt, submitter, checkpoint.clone())
        .unwrap();
    let submission = get_epoch_submissions(&mut rt, epoch);
    assert!(submission.is_none());
    let st: State = rt.get_state();
    assert_eq!(
        st.cron_checkpoint_voting.last_voting_executed_epoch(),
        epoch
    );

    // now we submit to the next epoch
    let epoch = *DEFAULT_GENESIS_EPOCH + *DEFAULT_CRON_PERIOD * 3;
    let checkpoint = CronCheckpoint {
        epoch,
        top_down_msgs: vec![],
    };
    h.submit_cron(&mut rt, submitter, checkpoint.clone())
        .unwrap();
    let st: State = rt.get_state();
    assert_eq!(
        st.cron_checkpoint_voting.last_voting_executed_epoch(),
        pending_epoch
    );
    assert_eq!(*st.cron_checkpoint_voting.executable_epoch_queue(), None);
}
