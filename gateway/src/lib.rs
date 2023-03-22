#![feature(let_chains)]
#![feature(map_first_last)] // For some simpler syntax for if let Some conditions

extern crate core;

pub use self::checkpoint::{Checkpoint, CrossMsgMeta};
pub use self::cross::{is_bottomup, CrossMsg, CrossMsgs, IPCMsgType, StorableMsg};
pub use self::state::*;
pub use self::subnet::*;
pub use self::types::*;
pub use crate::cron::{CronSubmission, VoteExecutionStatus};
pub use cron::CronCheckpoint;
use cross::{burn_bu_funds, cross_msg_side_effects, distribute_crossmsg_fee};
use fil_actors_runtime::runtime::fvm::resolve_secp_bls;
use fil_actors_runtime::runtime::{ActorCode, Runtime};
use fil_actors_runtime::{
    actor_dispatch, actor_error, restrict_internal_api, ActorDowncast, ActorError,
    CALLER_TYPES_SIGNABLE, INIT_ACTOR_ADDR, SYSTEM_ACTOR_ADDR,
};
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_ipld_hamt::BytesKey;
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::clock::ChainEpoch;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::METHOD_SEND;
use fvm_shared::{MethodNum, METHOD_CONSTRUCTOR};
pub use ipc_sdk::address::IPCAddress;
pub use ipc_sdk::subnet_id::SubnetID;
use ipc_sdk::ValidatorSet;
use lazy_static::lazy_static;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use primitives::TCid;

#[cfg(feature = "fil-gateway-actor")]
fil_actors_runtime::wasm_trampoline!(Actor);

pub mod checkpoint;
mod cron;
mod cross;
mod error;
#[doc(hidden)]
pub mod ext;
mod state;
pub mod subnet;
mod types;

// TODO: make this into constructor!
lazy_static! {
    pub static ref CROSS_MSG_FEE: TokenAmount = TokenAmount::from_nano(100);
}

/// Gateway actor methods available
#[derive(FromPrimitive)]
#[repr(u64)]
pub enum Method {
    /// Constructor for Storage Power Actor
    Constructor = METHOD_CONSTRUCTOR,
    Register = frc42_dispatch::method_hash!("Register"),
    AddStake = frc42_dispatch::method_hash!("AddStake"),
    ReleaseStake = frc42_dispatch::method_hash!("ReleaseStake"),
    Kill = frc42_dispatch::method_hash!("Kill"),
    CommitChildCheckpoint = frc42_dispatch::method_hash!("CommitChildCheckpoint"),
    Fund = frc42_dispatch::method_hash!("Fund"),
    Release = frc42_dispatch::method_hash!("Release"),
    SendCross = frc42_dispatch::method_hash!("SendCross"),
    ApplyMessage = frc42_dispatch::method_hash!("ApplyMessage"),
    Propagate = frc42_dispatch::method_hash!("Propagate"),
    WhiteListPropagator = frc42_dispatch::method_hash!("WhiteListPropagator"),
    SubmitCron = frc42_dispatch::method_hash!("SubmitCron"),
    SetMembership = frc42_dispatch::method_hash!("SetMembership"),
}

/// Gateway Actor
pub struct Actor;

impl Actor {
    /// Constructor for gateway actor
    fn constructor(rt: &mut impl Runtime, params: ConstructorParams) -> Result<(), ActorError> {
        rt.validate_immediate_caller_is(std::iter::once(&INIT_ACTOR_ADDR))?;

        let st = State::new(rt.store(), params).map_err(|e| {
            e.downcast_default(
                ExitCode::USR_ILLEGAL_STATE,
                "Failed to create SCA actor state",
            )
        })?;
        rt.create(&st)?;
        Ok(())
    }

    /// Register is called by subnet actors to put the required collateral
    /// and register the subnet to the hierarchy.
    fn register(rt: &mut impl Runtime) -> Result<SubnetID, ActorError> {
        rt.validate_immediate_caller_accept_any()?;

        let subnet_addr = rt.message().caller();
        let mut shid = SubnetID::default();
        rt.transaction(|st: &mut State, rt| {
            shid = SubnetID::new_from_parent(&st.network_name, subnet_addr);
            let sub = st.get_subnet(rt.store(), &shid).map_err(|e| {
                e.downcast_default(ExitCode::USR_ILLEGAL_STATE, "failed to load subnet")
            })?;
            match sub {
                Some(_) => {
                    return Err(actor_error!(
                        illegal_argument,
                        "subnet with id {} already registered",
                        shid
                    ));
                }
                None => {
                    st.register_subnet(rt, &shid).map_err(|e| {
                        e.downcast_default(
                            ExitCode::USR_ILLEGAL_ARGUMENT,
                            "Failed to register subnet",
                        )
                    })?;
                }
            }

            Ok(())
        })?;

        log::debug!("registered new subnet: {:?}", shid);
        Ok(shid)
    }

    /// Add stake adds stake to the collateral of a subnet.
    fn add_stake(rt: &mut impl Runtime) -> Result<(), ActorError> {
        rt.validate_immediate_caller_accept_any()?;

        let subnet_addr = rt.message().caller();

        let val = rt.message().value_received();
        if val <= TokenAmount::zero() {
            return Err(actor_error!(illegal_argument, "no stake to add"));
        }

        rt.transaction(|st: &mut State, rt| {
            let shid = SubnetID::new_from_parent(&st.network_name, subnet_addr);
            let sub = st.get_subnet(rt.store(), &shid).map_err(|e| {
                e.downcast_default(ExitCode::USR_ILLEGAL_STATE, "failed to load subnet")
            })?;
            match sub {
                Some(mut sub) => {
                    sub.add_stake(rt, st, &val).map_err(|e| {
                        e.downcast_default(
                            ExitCode::USR_ILLEGAL_STATE,
                            "Failed to add stake to subnet",
                        )
                    })?;
                }
                None => {
                    return Err(actor_error!(
                        illegal_argument,
                        "subnet with id {} not registered",
                        shid
                    ));
                }
            }

            Ok(())
        })?;

        Ok(())
    }

    /// Release stake recovers some collateral of the subnet
    fn release_stake(rt: &mut impl Runtime, params: FundParams) -> Result<(), ActorError> {
        rt.validate_immediate_caller_accept_any()?;

        let subnet_addr = rt.message().caller();

        let send_val = params.value;

        if send_val <= TokenAmount::zero() {
            return Err(actor_error!(
                illegal_argument,
                "no funds to release in params"
            ));
        }

        rt.transaction(|st: &mut State, rt| {
            let shid = SubnetID::new_from_parent(&st.network_name, subnet_addr);
            let sub = st.get_subnet(rt.store(), &shid).map_err(|e| {
                e.downcast_default(ExitCode::USR_ILLEGAL_STATE, "failed to load subnet")
            })?;
            match sub {
                Some(mut sub) => {
                    if sub.stake < send_val {
                        return Err(actor_error!(
                            illegal_state,
                            "subnet actor not allowed to release so many funds"
                        ));
                    }
                    // sanity-check: see if the actor has enough balance.
                    if rt.current_balance() < send_val {
                        return Err(actor_error!(
                            illegal_state,
                            "something went really wrong! the actor doesn't have enough balance to release"
                        ));
                    }
                    sub.add_stake(rt, st, &-send_val.clone()).map_err(|e| {
                        e.downcast_default(
                            ExitCode::USR_ILLEGAL_STATE,
                            "Failed to add stake to subnet",
                        )
                    })?;
                }
                None => {
                    return Err(actor_error!(
                        illegal_argument,
                        "subnet with id {} not registered",
                        shid
                    ));
                }
            }

            Ok(())
        })?;

        rt.send(&subnet_addr, METHOD_SEND, None, send_val.clone())?;
        Ok(())
    }

    /// Kill propagates the kill signal from a subnet actor to unregister it from th
    /// hierarchy.
    fn kill(rt: &mut impl Runtime) -> Result<(), ActorError> {
        rt.validate_immediate_caller_accept_any()?;

        let subnet_addr = rt.message().caller();
        let mut send_val = TokenAmount::zero();

        rt.transaction(|st: &mut State, rt| {
            let shid = SubnetID::new_from_parent(&st.network_name, subnet_addr);
            let sub = st.get_subnet(rt.store(), &shid).map_err(|e| {
                e.downcast_default(ExitCode::USR_ILLEGAL_STATE, "failed to load subnet")
            })?;
            match sub {
                Some(sub) => {
                    if rt.current_balance() < sub.stake {
                        return Err(actor_error!(
                            illegal_state,
                            "something went really wrong! the actor doesn't have enough balance to release"
                        ));
                    }
                    if sub.circ_supply > TokenAmount::zero() {
                        return Err(actor_error!(
                            illegal_state,
                            "cannot kill a subnet that still holds user funds in its circ. supply"
                        ));
                    }
                    send_val = sub.stake;
                    // delete subnet
                    st.rm_subnet(rt.store(), &shid).map_err(|e| {
                        e.downcast_default(ExitCode::USR_ILLEGAL_STATE, "failed to load subnet")
                    })?;
                }
                None => {
                    return Err(actor_error!(
                        illegal_argument,
                        "subnet with id {} not registered",
                        shid
                    ));
                }
            }

            Ok(())
        })?;

        rt.send(&subnet_addr, METHOD_SEND, None, send_val.clone())?;
        Ok(())
    }

    /// CommitChildCheck propagates the commitment of a checkpoint from a child subnet,
    /// process the cross-messages directed to the subnet.
    fn commit_child_check(rt: &mut impl Runtime, params: Checkpoint) -> Result<(), ActorError> {
        rt.validate_immediate_caller_accept_any()?;

        let subnet_addr = rt.message().caller();
        let commit = params;
        let subnet_actor = commit.source().subnet_actor();

        // check if the checkpoint belongs to the subnet
        if subnet_addr != subnet_actor {
            return Err(actor_error!(
                illegal_argument,
                "source in checkpoint doesn't belong to subnet"
            ));
        }

        let fee = rt.transaction(|st: &mut State, rt| {
            let shid = SubnetID::new_from_parent(&st.network_name, subnet_addr);
            let sub = st.get_subnet(rt.store(), &shid).map_err(|e| {
                e.downcast_default(ExitCode::USR_ILLEGAL_STATE, "failed to load subnet")
            })?;

            let mut fee = TokenAmount::zero();
            match sub {
                Some(mut sub) => {
                    // check if subnet active
                    if sub.status != Status::Active {
                        return Err(actor_error!(
                            illegal_state,
                            "can't commit checkpoint for an inactive subnet"
                        ));
                    }

                    // get window checkpoint being populated to include child info
                    let mut ch = st
                        .get_window_checkpoint(rt.store(), rt.curr_epoch())
                        .map_err(|e| {
                            e.downcast_default(
                                ExitCode::USR_ILLEGAL_STATE,
                                "failed to get current epoch checkpoint",
                            )
                        })?;

                    // if this is not the first checkpoint we need to perform some
                    // additional verifications.
                    if let Some(ref prev_checkpoint) = sub.prev_checkpoint {
                        if prev_checkpoint.epoch() > commit.epoch() {
                            return Err(actor_error!(
                                illegal_argument,
                                "checkpoint being committed belongs to the past"
                            ));
                        }
                        // check that the previous cid is consistent with the previous one
                        if commit.prev_check().cid() != prev_checkpoint.cid() {
                            return Err(actor_error!(
                                illegal_argument,
                                "previous checkpoint not consistente with previous one"
                            ));
                        }
                    }

                    // commit cross-message in checkpoint to either execute them or
                    // queue them for propagation if there are cross-msgs availble.
                    if let Some(cross_msg) = commit.cross_msgs() {
                        // if tcid not default it means cross-msgs are being propagated.
                        if cross_msg.msgs_cid != TCid::default() {
                            st.store_bottomup_msg(rt.store(), cross_msg).map_err(|e| {
                                e.downcast_default(
                                    ExitCode::USR_ILLEGAL_STATE,
                                    "error storing bottom_up messages from checkpoint",
                                )
                            })?;
                        }

                        // release circulating supply
                        sub.release_supply(&cross_msg.value).map_err(|e| {
                            e.downcast_default(
                                ExitCode::USR_ILLEGAL_STATE,
                                "error releasing circulating supply",
                            )
                        })?;

                        // distribute fee
                        fee = cross_msg.fee.clone();
                    }

                    // append new checkpoint to the list of childs
                    ch.add_child_check(&commit).map_err(|e| {
                        e.downcast_default(
                            ExitCode::USR_ILLEGAL_ARGUMENT,
                            "error adding child checkpoint",
                        )
                    })?;

                    // flush checkpoint
                    st.flush_checkpoint(rt.store(), &ch).map_err(|e| {
                        e.downcast_default(ExitCode::USR_ILLEGAL_STATE, "error flushing checkpoint")
                    })?;

                    // update prev_check for child
                    sub.prev_checkpoint = Some(commit);
                    // flush subnet
                    st.flush_subnet(rt.store(), &sub).map_err(|e| {
                        e.downcast_default(ExitCode::USR_ILLEGAL_STATE, "error flushing subnet")
                    })?;
                }
                None => {
                    return Err(actor_error!(
                        illegal_argument,
                        "subnet with id {} not registered",
                        shid
                    ));
                }
            }

            Ok(fee)
        })?;

        // distribute rewards
        distribute_crossmsg_fee(rt, &subnet_actor, fee)
    }

    /// Fund injects new funds from an account of the parent chain to a subnet.
    ///
    /// This functions receives a transaction with the FILs that want to be injected in the subnet.
    /// - Funds injected are frozen.
    /// - A new fund cross-message is created and stored to propagate it to the subnet. It will be
    /// picked up by miners to include it in the next possible block.
    /// - The cross-message nonce is updated.
    fn fund(rt: &mut impl Runtime, params: SubnetID) -> Result<(), ActorError> {
        // funds can only be moved between subnets by signable addresses
        rt.validate_immediate_caller_type(CALLER_TYPES_SIGNABLE.iter())?;

        let mut value = rt.message().value_received();
        if value <= TokenAmount::zero() {
            return Err(actor_error!(
                illegal_argument,
                "no funds included in fund message"
            ));
        }

        let sig_addr = resolve_secp_bls(rt, &rt.message().caller())?;

        let fee = CROSS_MSG_FEE.clone();
        rt.transaction(|st: &mut State, rt| {
            st.collect_cross_fee(&mut value, &fee)?;
            // Create fund message
            let mut f_msg = CrossMsg {
                msg: StorableMsg::new_fund_msg(&params, &sig_addr, value).map_err(|e| {
                    e.downcast_default(
                        ExitCode::USR_ILLEGAL_STATE,
                        "error creating fund cross-message",
                    )
                })?,
                wrapped: false,
            };

            log::debug!("fund cross msg is: {:?}", f_msg);

            // Commit top-down message.
            st.commit_topdown_msg(rt.store(), &mut f_msg).map_err(|e| {
                e.downcast_default(
                    ExitCode::USR_ILLEGAL_STATE,
                    "error committing top-down message",
                )
            })?;
            Ok(())
        })?;

        // distribute top-down message fee to validators.
        distribute_crossmsg_fee(rt, &params.subnet_actor(), fee)
    }

    /// Release creates a new check message to release funds in parent chain
    ///
    /// This function burns the funds that will be released in the current subnet
    /// and propagates a new checkpoint message to the parent chain to signal
    /// the amount of funds that can be released for a specific address.
    fn release(rt: &mut impl Runtime) -> Result<(), ActorError> {
        // funds can only be moved between subnets by signable addresses
        rt.validate_immediate_caller_type(CALLER_TYPES_SIGNABLE.iter())?;

        // FIXME: Only supporting cross-messages initiated by signable addresses for
        // now. Consider supporting also send-cross messages initiated by actors.

        let mut value = rt.message().value_received();
        if value <= TokenAmount::zero() {
            return Err(actor_error!(
                illegal_argument,
                "no funds included in message"
            ));
        }

        let sig_addr = resolve_secp_bls(rt, &rt.message().caller())?;

        rt.transaction(|st: &mut State, rt| {
            let fee = &CROSS_MSG_FEE;
            // collect fees
            st.collect_cross_fee(&mut value, fee)?;

            // Create release message
            let r_msg = CrossMsg {
                msg: StorableMsg::new_release_msg(
                    &st.network_name,
                    &sig_addr,
                    value.clone(),
                    st.nonce,
                )
                .map_err(|e| {
                    e.downcast_default(
                        ExitCode::USR_ILLEGAL_STATE,
                        "error creating release cross-message",
                    )
                })?,
                wrapped: false,
            };

            // Commit bottom-up message.
            st.commit_bottomup_msg(rt.store(), &r_msg, fee, rt.curr_epoch())
                .map_err(|e| {
                    e.downcast_default(
                        ExitCode::USR_ILLEGAL_STATE,
                        "error committing top-down message",
                    )
                })?;
            Ok(())
        })?;

        // burn funds that are send as bottom-up
        burn_bu_funds(rt, value)
    }

    /// SendCross sends an arbitrary cross-message to other subnet in the hierarchy.
    ///
    /// If the message includes any funds they need to be burnt (like in Release)
    /// before being propagated to the corresponding subnet.
    /// The circulating supply in each subnet needs to be updated as the message passes through them.
    ///
    /// Params expect a raw message without any subnet context (the IPC address is
    /// included in the message by the actor). Only actors are allowed to send arbitrary
    /// cross-messages as a side-effect of their execution. For plain token exchanges
    /// fund and release have to be used.
    fn send_cross(rt: &mut impl Runtime, params: CrossMsgParams) -> Result<(), ActorError> {
        // only actor are allowed to send cross-message
        rt.validate_immediate_caller_not_type(CALLER_TYPES_SIGNABLE.iter())?;

        // FIXME: Should we add an additional check to ensure that the included message
        // has an actor ID as from and thus that the message doesn't come from a
        // account actor or a multisig?

        if params.destination == SubnetID::default() {
            return Err(actor_error!(
                illegal_argument,
                "no destination for cross-message explicitly set"
            ));
        }
        let CrossMsgParams {
            mut cross_msg,
            destination,
        } = params;
        let (mut do_burn, mut top_down_fee) = (false, TokenAmount::zero());

        rt.transaction(|st: &mut State, rt| {
            if destination == st.network_name {
                return Err(actor_error!(
                    illegal_argument,
                    "destination is the current network, you are better off with a good ol' message, no cross needed"
                ));
            }
            // we disregard the to of the message. the caller is the one set as the from of the
            // message.
            let msg = &mut cross_msg.msg;
            let to = msg.to.raw_addr().map_err(|_| actor_error!(illegal_argument, "invalid to addr"))?;
            msg.to = match IPCAddress::new(&destination, &to) {
                Ok(addr) => addr,
                Err(_) => {
                    return Err(actor_error!(
                        illegal_argument,
                        "error setting IPC address in cross-msg to param"
                    ));
                }
            };
            msg.from = match IPCAddress::new(&st.network_name, &rt.message().caller()) {
                Ok(addr) => addr,
                Err(_) => {
                    return Err(actor_error!(
                        illegal_argument,
                        "error setting IPC address in cross-msg from param"
                    ));
                }
            };

            // check that the right funds were sent in message
            // TODO: The cross_message fee will be deducted from the value of the
            // cross-message. Should we deduct it before this check? Or should we even
            // remove this check and return the remainder of the value sent in the message
            // and the cross-fee to the originating contract?
            if rt.message().value_received() != msg.value {
                return Err(actor_error!(
                    illegal_argument,
                    "the funds in cross-msg params are not equal to the ones sent in the message"
                ));
            }

            // collect cross-fee
            let fee = CROSS_MSG_FEE.clone();
            st.collect_cross_fee(&mut msg.value, &fee)?;

            // commit cross-message for propagation
            (do_burn, top_down_fee) = Self::commit_cross_message(rt, st, &mut cross_msg, fee)?;
            Ok(())
        })?;

        // side-effects sent without any remainders
        cross_msg_side_effects(rt, &cross_msg, do_burn, &top_down_fee)?;

        Ok(())
    }

    /// ApplyMessage triggers the execution of a cross-subnet message validated through the consensus.
    ///
    /// This function can only be triggered using `ApplyImplicitMessage`, and the source needs to
    /// be the SystemActor. Cross messages are applied similarly to how rewards are applied once
    /// a block has been validated. This function:
    /// - Determines the type of cross-message.
    /// - Performs the corresponding state changes.
    /// - And updated the latest nonce applied for future checks.
    fn apply_msg(rt: &mut impl Runtime, params: ApplyMsgParams) -> Result<RawBytes, ActorError> {
        rt.validate_immediate_caller_is([&SYSTEM_ACTOR_ADDR as &Address])?;
        let ApplyMsgParams { cross_msg } = params;
        Self::apply_msg_inner(rt, cross_msg)
    }

    fn apply_msg_inner(rt: &mut impl Runtime, cross_msg: CrossMsg) -> Result<RawBytes, ActorError> {
        let rto = match cross_msg.msg.to.raw_addr() {
            Ok(to) => to,
            Err(_) => {
                return Err(actor_error!(
                    illegal_argument,
                    "error getting raw address from msg"
                ));
            }
        };
        let sto = match cross_msg.msg.to.subnet() {
            Ok(to) => to,
            Err(_) => {
                return Err(actor_error!(
                    illegal_argument,
                    "error getting subnet from msg"
                ));
            }
        };

        let st: State = rt.state()?;

        log::debug!("sto: {:?}, network: {:?}", sto, st.network_name);

        match cross_msg.msg.apply_type(&st.network_name) {
            Ok(IPCMsgType::BottomUp) => {
                // if directed to current network, execute message.
                if sto == st.network_name {
                    rt.transaction(|st: &mut State, _| {
                        st.bottomup_state_transition(&cross_msg.msg).map_err(|e| {
                            e.downcast_default(
                                ExitCode::USR_ILLEGAL_STATE,
                                "failed applying bottomup message",
                            )
                        })?;
                        Ok(())
                    })?;
                    return cross_msg.send(rt, &rto);
                }
            }
            Ok(IPCMsgType::TopDown) => {
                // Mint funds for the gateway, as any topdown message
                // including tokens traversing the subnet will use
                // some balance from the gateway to increase the circ_supply.
                // check if the gateway has enough funds to mint new FIL,
                // if not fail right-away and do not allow the execution of
                // the message.
                // TODO: It may be a good idea in the future to decouple the
                // balance provisioned in the gateway to mint new circulating supply
                // in an independent actor. Minting tokens would require a call to the new actor actor to unlock
                // additional circulating supply. This prevents an attacker from being able token
                // vulnerabilities in the gateway
                // from draining the whole balance.
                if rt.current_balance() < cross_msg.msg.value {
                    return Err(actor_error!(
                        illegal_state,
                        "not enough balance to mint new tokens as part of the cross-message"
                    ));
                }

                if sto == st.network_name {
                    if st.applied_topdown_nonce != cross_msg.msg.nonce {
                        return Err(actor_error!(
                            illegal_state,
                            "the top-down message being applied doesn't hold the subsequent nonce"
                        ));
                    }

                    rt.transaction(|st: &mut State, _| {
                        st.applied_topdown_nonce += 1;
                        Ok(())
                    })?;

                    // We can return the send result
                    return cross_msg.send(rt, &rto);
                }
            }
            _ => {
                return Err(actor_error!(
                    illegal_argument,
                    "cross-message to apply dosen't have the right type"
                ))
            }
        };

        let cid = rt.transaction(|st: &mut State, rt| {
            let owner = cross_msg
                .msg
                .from
                .raw_addr()
                .map_err(|_| actor_error!(illegal_argument, "invalid address"))?;
            let r = st
                .insert_postbox(rt.store(), Some(vec![owner]), cross_msg)
                .map_err(|e| {
                    e.downcast_default(ExitCode::USR_ILLEGAL_STATE, "error save topdown messages")
                })?;
            Ok(r)
        })?;

        // it is safe to just unwrap. If `transaction` fails, cid is None and wont reach here.
        Ok(RawBytes::new(cid.to_bytes()))
    }

    /// Whitelist a series of addresses as propagator of a cross net message.
    /// This is basically adding this list of addresses to the `PostBoxItem::owners`.
    /// Only existing owners can perform this operation.
    fn whitelist_propagator(
        rt: &mut impl Runtime,
        params: WhitelistPropagatorParams,
    ) -> Result<(), ActorError> {
        // does not really need check as we are checking against the PostboxItem.owners
        rt.validate_immediate_caller_accept_any()?;

        let caller = rt.message().caller();
        let WhitelistPropagatorParams {
            postbox_cid,
            to_add,
        } = params;

        rt.transaction(|st: &mut State, rt| {
            let mut postbox_item = st.load_from_postbox(rt.store(), postbox_cid).map_err(|e| {
                log::error!("encountered error loading from postbox: {:?}", e);
                actor_error!(unhandled_message, "cannot load from postbox")
            })?;

            // Currently we dont support adding owners if the owners field is None.
            // This might change in the future.
            if postbox_item.owners.is_none() {
                return Err(actor_error!(
                    illegal_state,
                    "postbox item cannot add owner for now"
                ));
            }

            let owners = postbox_item.owners.as_mut().unwrap();
            if !owners.contains(&caller) {
                return Err(actor_error!(illegal_state, "not owner"));
            }
            owners.extend(to_add);

            st.swap_postbox_item(rt.store(), postbox_cid, postbox_item)
                .map_err(|e| {
                    log::error!("encountered error loading from postbox: {:?}", e);
                    actor_error!(unhandled_message, "cannot load from postbox")
                })?;

            Ok(())
        })?;

        Ok(())
    }

    fn propagate(rt: &mut impl Runtime, params: PropagateParams) -> Result<(), ActorError> {
        // does not really need check as we are checking against the PostboxItem.owners
        rt.validate_immediate_caller_accept_any()?;

        let PropagateParams { postbox_cid } = params;
        let owner = rt.message().caller();
        let mut value = rt.message().value_received();
        let (mut do_burn, mut top_down_fee) = (false, TokenAmount::zero());

        let cross_msg = rt.transaction(|st: &mut State, rt| {
            let postbox_item = st.load_from_postbox(rt.store(), postbox_cid).map_err(|e| {
                log::error!("encountered error loading from postbox: {:?}", e);
                actor_error!(unhandled_message, "cannot load from postbox")
            })?;

            if let Some(owners) = postbox_item.owners && !owners.contains(&owner) {
                return Err(actor_error!(illegal_state, "owner not match"));
            }

            // collect cross-fee
            let fee = CROSS_MSG_FEE.clone();
            st.collect_cross_fee(&mut value, &fee)?;

            let PostBoxItem { mut cross_msg, .. } = postbox_item;
            (do_burn, top_down_fee) = Self::commit_cross_message(rt, st, &mut cross_msg, fee)?;
            st.remove_from_postbox(rt.store(), postbox_cid)?;
            Ok(cross_msg)
        })?;

        // trigger cross-message side-effects returning the remainder of the fee
        // to the source.
        cross_msg_side_effects(rt, &cross_msg, do_burn, &top_down_fee)?;
        // return fee remainder to owner
        if !value.is_zero() {
            rt.send(&owner, METHOD_SEND, None, value.clone())?;
        }
        Ok(())
    }

    /// Set the memberships of the validators
    fn set_membership(
        rt: &mut impl Runtime,
        validator_set: ValidatorSet,
    ) -> Result<RawBytes, ActorError> {
        rt.validate_immediate_caller_is([&SYSTEM_ACTOR_ADDR as &Address])?;
        rt.transaction(|st: &mut State, _| {
            st.set_membership(validator_set);
            Ok(RawBytes::default())
        })
    }

    /// Submit a new cron checkpoint
    ///
    /// It only accepts submission at multiples of `cron_period` since `genesis_epoch`, which are
    /// set during construction. Each checkpoint will have its number of submissions tracked. The
    /// same address cannot submit twice. Once the number of submissions is more than or equal to 2/3
    /// of the total number of validators, the messages will be applied.
    ///
    /// Each cron checkpoint will be checked against each other using blake hashing.
    fn submit_cron(
        rt: &mut impl Runtime,
        checkpoint: CronCheckpoint,
    ) -> Result<RawBytes, ActorError> {
        // submit cron can only be performed by signable addresses
        rt.validate_immediate_caller_type(CALLER_TYPES_SIGNABLE.iter())?;

        Self::execute_next_cron_epoch(rt)?;

        let msgs = rt.transaction(|st: &mut State, rt| {
            let submitter = rt.message().caller();
            let submitter_weight = Self::validate_submitter(&st, checkpoint.epoch, &submitter)?;
            let store = rt.store();

            Self::handle_cron_submission(store, st, checkpoint, submitter, submitter_weight)
                .map_err(|e| {
                    log::error!(
                        "encountered error processing submit cron checkpoint: {:?}",
                        e
                    );
                    actor_error!(unhandled_message, e.to_string())
                })
        })?;

        if let Some(msgs) = msgs {
            for m in msgs {
                Self::apply_msg_inner(
                    rt,
                    CrossMsg {
                        msg: m,
                        wrapped: false,
                    },
                )?;
            }
        }

        Ok(RawBytes::default())
    }

    /// Commit the cross message to storage. It outputs a flag signaling
    /// if the committed messages was bottom-up and some funds need to be
    /// burnt or if a top-down message fee needs to be distributed.
    ///
    /// NOTE: This function should always be called inside an `rt.transaction`
    fn commit_cross_message(
        rt: &mut impl Runtime,
        st: &mut State,
        cross_msg: &mut CrossMsg,
        fee: TokenAmount,
    ) -> Result<(bool, TokenAmount), ActorError> {
        let mut do_burn = false;

        let sto = cross_msg
            .msg
            .to
            .subnet()
            .map_err(|_| actor_error!(illegal_argument, "error getting subnet from msg"))?;
        if sto == st.network_name {
            return Err(actor_error!(illegal_state, "should already be committed"));
        }

        match cross_msg.msg.apply_type(&st.network_name).map_err(|e| {
            e.downcast_default(
                ExitCode::USR_ILLEGAL_STATE,
                "cannot convert cross message type",
            )
        })? {
            IPCMsgType::BottomUp => {
                let mut top_down_fee = TokenAmount::zero();
                let sfrom =
                    cross_msg.msg.from.subnet().map_err(|_| {
                        actor_error!(illegal_argument, "error getting subnet from msg")
                    })?;
                let nearest_common_parent = sto.common_parent(&sfrom).unwrap().1;

                log::debug!(
                    "nearest common parent: {:?}, current network: {:?}",
                    nearest_common_parent,
                    st.network_name
                );

                // if the message is a bottom-up message and it reached the common-parent
                // then we need to start propagating it down to the destination.
                let r = if nearest_common_parent == st.network_name {
                    top_down_fee = fee;
                    st.commit_topdown_msg(rt.store(), cross_msg)
                } else {
                    if cross_msg.msg.value > TokenAmount::zero() {
                        do_burn = true;
                    }
                    st.commit_bottomup_msg(rt.store(), cross_msg, &fee, rt.curr_epoch())
                };

                r.map_err(|e| {
                    e.downcast_default(
                        ExitCode::USR_ILLEGAL_STATE,
                        "error committing bottom-up messages",
                    )
                })?;

                Ok((do_burn, top_down_fee))
            }
            IPCMsgType::TopDown => {
                st.applied_topdown_nonce += 1;
                st.commit_topdown_msg(rt.store(), cross_msg).map_err(|e| {
                    e.downcast_default(
                        ExitCode::USR_ILLEGAL_STATE,
                        "error committing top-down message while applying it",
                    )
                })?;
                Ok((do_burn, fee))
            }
        }
    }
}

/// All the validator code for the actor calls
impl Actor {
    /// Validate the submitter's submission against the state, also returns the weight of the validator
    fn validate_submitter(
        st: &State,
        epoch: ChainEpoch,
        submitter: &Address,
    ) -> Result<TokenAmount, ActorError> {
        // first we check the epoch is the correct one, we process only it's multiple
        // of cron_period since genesis_epoch
        if (epoch - st.genesis_epoch) % st.cron_period != 0 {
            return Err(actor_error!(illegal_argument, "epoch not allowed"));
        }

        if st.last_cron_executed_epoch >= epoch {
            return Err(actor_error!(illegal_argument, "epoch already executed"));
        }

        st.validators
            .get_validator_weight(submitter)
            .ok_or(actor_error!(illegal_argument, "caller not validator"))
    }
}

/// Contains private method invocation
impl Actor {
    fn handle_cron_submission<BS: Blockstore>(
        store: &BS,
        st: &mut State,
        checkpoint: CronCheckpoint,
        submitter: Address,
        submitter_weight: TokenAmount,
    ) -> anyhow::Result<Option<Vec<StorableMsg>>> {
        let total_weight = st.validators.total_weight.clone();
        let params_epoch = checkpoint.epoch;

        // We are doing this manually because we have to modify `state` while processing the `hamt`.
        // The current `st.cron_submissions.modify(...)` does not allow us to modify state in the
        // function closure passed to modify.
        let mut hamt = st.cron_submissions.load(store)?;

        let epoch_key = BytesKey::from(params_epoch.to_be_bytes().as_slice());
        let mut submission = match hamt.get(&epoch_key)? {
            Some(s) => s.clone(),
            None => CronSubmission::new(store)?,
        };

        let most_voted_weight =
            submission.submit(store, submitter, submitter_weight, checkpoint)?;
        let execution_status = submission.derive_execution_status(total_weight, most_voted_weight);

        let messages = match execution_status {
            VoteExecutionStatus::ThresholdNotReached | VoteExecutionStatus::ReachingConsensus => {
                // threshold or consensus not reached, store submission and return
                hamt.set(epoch_key, submission)?;
                None
            }
            VoteExecutionStatus::RoundAbort => {
                submission.abort(store)?;
                hamt.set(epoch_key, submission)?;
                None
            }
            VoteExecutionStatus::ConsensusReached => {
                if st.last_cron_executed_epoch + st.cron_period != params_epoch {
                    // there are pending epochs to be executed,
                    // just store the submission and skip execution
                    hamt.set(epoch_key, submission)?;
                    st.insert_executable_epoch(params_epoch);
                    return Ok(None);
                }

                // we reach consensus in the checkpoints submission
                st.last_cron_executed_epoch = params_epoch;

                let msgs = submission
                    .load_most_submitted_checkpoint(store)?
                    .unwrap()
                    .top_down_msgs;
                hamt.delete(&epoch_key)?;

                Some(msgs)
            }
        };

        // don't forget to flush
        st.cron_submissions = TCid::from(hamt.flush()?);

        Ok(messages)
    }

    /// Externally trigger cron submission epoch. This is an edge case to ensure none of the epoches
    /// will be stuck. Consider the following example:
    ///
    /// Epoch 10 and 20 are two cron epoch to be executed. However, all the validators have submitted
    /// epoch 20, and the status is to be executed, however, epoch 10 has yet to be executed. Now,
    /// epoch 10 has reached consensus and executed, but epoch 20 cannot be executed because every
    /// validator has already voted, no one can vote again to trigger the execution. Epoch 20 is stuck.
    ///
    /// Introduce this method so that anyone can trigger the execution of an epoch, but provided the
    /// status of the epoch is already consensus reached.
    fn execute_next_cron_epoch(rt: &mut impl Runtime) -> Result<(), ActorError> {
        let msgs = rt.transaction(|st: &mut State, rt| {
            let epoch_queue = match st.executable_epoch_queue.as_mut() {
                None => return Ok(None),
                Some(queue) => queue,
            };

            match epoch_queue.first() {
                None => {
                    unreachable!("`epoch_queue` is not None, it should not be empty, report bug")
                }
                Some(epoch) => {
                    if *epoch > st.last_cron_executed_epoch + st.cron_period {
                        log::debug!("earliest executable epoch not the same cron period");
                        return Ok(None);
                    }
                }
            }

            let store = rt.store();
            let epoch = epoch_queue.pop_first().unwrap();

            if epoch_queue.is_empty() {
                st.executable_epoch_queue = None;
            }

            st.cron_submissions
                .modify(store, |hamt| {
                    let epoch_key = BytesKey::from(epoch.to_be_bytes().as_slice());
                    let submission = match hamt.get(&epoch_key)? {
                        Some(s) => s,
                        None => unreachable!("Submission in epoch not found, report bug"),
                    };

                    st.last_cron_executed_epoch = epoch;

                    let msgs = submission
                        .load_most_submitted_checkpoint(store)?
                        .unwrap()
                        .top_down_msgs;
                    hamt.delete(&epoch_key)?;

                    Ok(Some(msgs))
                })
                .map_err(|e| {
                    log::error!(
                        "encountered error processing submit cron checkpoint: {:?}",
                        e
                    );
                    actor_error!(unhandled_message, e.to_string())
                })
        })?;

        if let Some(msgs) = msgs {
            for m in msgs {
                Self::apply_msg_inner(
                    rt,
                    CrossMsg {
                        msg: m,
                        wrapped: false,
                    },
                )?;
            }
        }
        Ok(())
    }
}

impl ActorCode for Actor {
    type Methods = Method;

    actor_dispatch! {
        Constructor => constructor,
        Register => register,
        AddStake => add_stake,
        ReleaseStake => release_stake,
        Kill => kill,
        CommitChildCheckpoint => commit_child_check,
        Fund => fund,
        Release => release,
        SendCross => send_cross,
        ApplyMessage => apply_msg,
        Propagate => propagate,
        WhiteListPropagator => whitelist_propagator,
        SubmitCron => submit_cron,
        SetMembership => set_membership,
    }
}
