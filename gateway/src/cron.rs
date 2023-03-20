use crate::StorableMsg;
use anyhow::anyhow;
use cid::multihash::Code;
use cid::multihash::MultihashDigest;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::to_vec;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_hamt::BytesKey;
use fvm_shared::address::Address;
use fvm_shared::clock::ChainEpoch;
use ipc_sdk::Validator;
use primitives::{TCid, THamt};
use std::cmp::Ordering;

pub type HashOutput = Vec<u8>;
const RATIO_NUMERATOR: u16 = 2;
const RATIO_DENOMINATOR: u16 = 3;

/// Validators tracks all the validator in the subnet. It is useful in handling cron checkpoints.
#[derive(Clone, Debug, Serialize_tuple, Deserialize_tuple)]
pub struct Validators {
    /// Total number of validators
    pub total_count: u16,
    /// The data structure that tracks all the validators in the subnet.
    /// We are using hamt due to:
    ///     - Since the size of validators can grow to significant value, it's not efficient to
    ///       read all the data every time
    ///     - We only care about whether some address is a validator instead of the whole validators
    /// The key is the `Validator.addr` converted to bytes.
    pub validators: TCid<THamt<String, Validator>>,
}

impl Validators {
    pub fn new<BS: Blockstore>(store: &BS) -> anyhow::Result<Self> {
        Ok(Self {
            total_count: 0,
            validators: TCid::new_hamt(store)?,
        })
    }

    fn hamt_key(addr: &Address) -> BytesKey {
        BytesKey::from(addr.to_bytes())
    }

    /// Add a validator to existing validators
    pub fn add_validator<BS: Blockstore>(
        &mut self,
        store: &BS,
        validator: Validator,
    ) -> anyhow::Result<()> {
        let key = Self::hamt_key(&validator.addr);

        self.validators.modify(store, |hamt| {
            if hamt.contains_key(&key)? {
                return Ok(());
            }

            // not containing the validator
            self.total_count += 1;
            hamt.set(key, validator)?;

            Ok(())
        })
    }

    /// Remove a validator from existing validators
    pub fn remove_validator<BS: Blockstore>(
        &mut self,
        store: &BS,
        addr: &Address,
    ) -> anyhow::Result<()> {
        let key = Self::hamt_key(addr);

        self.validators.modify(store, |hamt| {
            if !hamt.contains_key(&key)? {
                return Ok(());
            }

            // containing the validator
            self.total_count -= 1;
            hamt.delete(&key)?;

            Ok(())
        })
    }
}

/// Checkpoints propagated from parent to child to signal the "final view" of the parent chain
/// from the different validators in the subnet.
#[derive(Clone, Debug, Serialize_tuple, Deserialize_tuple, PartialEq, Eq)]
pub struct CronCheckpoint {
    pub epoch: ChainEpoch,
    pub top_down_msgs: Vec<StorableMsg>,
}

impl CronCheckpoint {
    /// Hash the checkpoint.
    ///
    /// To compare the cron checkpoint and ensure they are the same, we need to make sure the
    /// top_down_msgs are the same. However, the top_down_msgs are vec, they may contain the same
    /// content, but their orders are different. In this case, we need to ensure the same order is
    /// maintained in the cron checkpoint submission.
    ///
    /// To ensure we have the same consistent output for different submissions, we require:
    ///     - top down messages are sorted by `nonce` in descending order
    ///
    /// Actor will not perform sorting to save gas. Client should do it, actor just check.
    fn hash(&self) -> anyhow::Result<HashOutput> {
        // check top down msgs
        for i in 1..self.top_down_msgs.len() {
            match self.top_down_msgs[i - 1]
                .nonce
                .cmp(&self.top_down_msgs[i].nonce)
            {
                Ordering::Less => {}
                Ordering::Equal => return Err(anyhow!("top down messages not distinct")),
                Ordering::Greater => return Err(anyhow!("top down messages not sorted")),
            };
        }

        let mh_code = Code::Blake2b256;
        // TODO: to avoid serialization again, maybe we should perform deserialization in the actor
        // TODO: dispatch call to save gas? The actor dispatching contains the raw serialized data,
        // TODO: which we dont have to serialize here again
        Ok(mh_code.digest(&to_vec(self).unwrap()).to_bytes())
    }
}

/// Track all the cron checkpoint submissions of an epoch
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone)]
pub struct CronSubmission {
    /// Total number of submissions from validators
    total_submissions: u16,
    /// The most submitted hash.
    most_voted_hash: Option<HashOutput>,
    /// The addresses of all the submitters
    submitters: TCid<THamt<Address, ()>>,
    /// The map to track the max submitted
    submission_counts: TCid<THamt<HashOutput, u16>>,
    /// The different cron checkpoints, with cron checkpoint hash as key
    submissions: TCid<THamt<HashOutput, CronCheckpoint>>,
}

impl CronSubmission {
    pub fn new<BS: Blockstore>(store: &BS) -> anyhow::Result<Self> {
        Ok(CronSubmission {
            total_submissions: 0,
            submitters: TCid::new_hamt(store)?,
            most_voted_hash: None,
            submission_counts: TCid::new_hamt(store)?,
            submissions: TCid::new_hamt(store)?,
        })
    }

    /// Abort the current round and reset the submission data.
    pub fn abort<BS: Blockstore>(&mut self, store: &BS) -> anyhow::Result<()> {
        self.total_submissions = 0;
        self.submitters = TCid::new_hamt(store)?;
        self.most_voted_hash = None;
        self.submission_counts = TCid::new_hamt(store)?;

        // no need reset `self.submissions`, we can still reuse the previous self.submissions
        // new submissions will be inserted, old submission will not be inserted to save
        // gas.

        Ok(())
    }

    /// Submit a cron checkpoint as the submitter.
    pub fn submit<BS: Blockstore>(
        &mut self,
        store: &BS,
        submitter: Address,
        checkpoint: CronCheckpoint,
    ) -> anyhow::Result<u16> {
        self.update_submitters(store, submitter)?;
        let checkpoint_hash = self.insert_checkpoint(store, checkpoint)?;
        self.update_submission_count(store, checkpoint_hash)
    }

    pub fn load_most_submitted_checkpoint<BS: Blockstore>(
        &self,
        store: &BS,
    ) -> anyhow::Result<Option<CronCheckpoint>> {
        // we will only have one entry in the `most_submitted` set if more than 2/3 has reached
        if let Some(hash) = &self.most_voted_hash {
            self.get_submission(store, hash)
        } else {
            Ok(None)
        }
    }

    pub fn get_submission<BS: Blockstore>(
        &self,
        store: &BS,
        hash: &HashOutput,
    ) -> anyhow::Result<Option<CronCheckpoint>> {
        let hamt = self.submissions.load(store)?;
        let key = BytesKey::from(hash.as_slice());
        Ok(hamt.get(&key)?.cloned())
    }

    pub fn derive_execution_status(
        &self,
        total_validators: u16,
        most_voted_count: u16,
    ) -> VoteExecutionStatus {
        // use u16 numerator and denominator to avoid floating point calculation and external crate
        // total validators should be within u16::MAX.
        let threshold = total_validators as u16 * RATIO_NUMERATOR / RATIO_DENOMINATOR;

        // note that we require THRESHOLD to be surpassed, equality is not enough!
        if self.total_submissions <= threshold {
            return VoteExecutionStatus::ThresholdNotReached;
        }

        // now we have reached the threshold

        // consensus reached
        if most_voted_count > threshold {
            return VoteExecutionStatus::ConsensusReached;
        }

        // now the total submissions has reached the threshold, but the most submitted vote
        // has yet to reach the threshold, that means consensus has not reached.

        // we do a early termination check, to see if consensus will ever be reached.
        //
        // consider an example that consensus will never be reached:
        //
        // -------- | -------------------------|--------------- | ------------- |
        //     MOST_VOTED                 THRESHOLD     TOTAL_SUBMISSIONS  TOTAL_VALIDATORS
        //
        // we see MOST_VOTED is smaller than THRESHOLD, TOTAL_SUBMISSIONS and TOTAL_VALIDATORS, if
        // the potential extra votes any vote can obtain, i.e. TOTAL_VALIDATORS - TOTAL_SUBMISSIONS,
        // is smaller than or equal to the potential extra vote the most voted can obtain, i.e.
        // THRESHOLD - MOST_VOTED, then consensus will never be reached, no point voting, just abort.
        if threshold - most_voted_count >= total_validators - self.total_submissions {
            VoteExecutionStatus::RoundAbort
        } else {
            VoteExecutionStatus::ReachingConsensus
        }
    }
}

/// The status indicating if the voting should be executed
#[derive(Eq, PartialEq, Debug)]
pub enum VoteExecutionStatus {
    /// The execution threshold has yet to be reached
    ThresholdNotReached,
    /// The voting threshold has reached, but consensus has yet to be reached, needs more
    /// voting to reach consensus
    ReachingConsensus,
    /// Consensus cannot be reached in this round
    RoundAbort,
    /// Execution threshold reached
    ConsensusReached,
}

impl CronSubmission {
    /// Update the total submitters, returns the latest total number of submitters
    fn update_submitters<BS: Blockstore>(
        &mut self,
        store: &BS,
        submitter: Address,
    ) -> anyhow::Result<u16> {
        let addr_byte_key = BytesKey::from(submitter.to_bytes());
        self.submitters.modify(store, |hamt| {
            // check the submitter has not submitted before
            if hamt.contains_key(&addr_byte_key)? {
                return Err(anyhow!("already submitted"));
            }

            // now the submitter has not submitted before, mark as submitted
            hamt.set(addr_byte_key, ())?;
            self.total_submissions += 1;

            Ok(self.total_submissions)
        })
    }

    /// Insert the checkpoint to store if it has not been submitted before. Returns the hash of the checkpoint.
    fn insert_checkpoint<BS: Blockstore>(
        &mut self,
        store: &BS,
        checkpoint: CronCheckpoint,
    ) -> anyhow::Result<HashOutput> {
        let hash = checkpoint.hash()?;
        let hash_key = BytesKey::from(hash.as_slice());

        let hamt = self.submissions.load(store)?;
        if hamt.contains_key(&hash_key)? {
            return Ok(hash);
        }

        // checkpoint has not submitted before
        self.submissions.modify(store, |hamt| {
            hamt.set(hash_key, checkpoint)?;
            Ok(())
        })?;

        Ok(hash)
    }

    /// Update submission count of the hash. Returns the currently most submitted submission count.
    fn update_submission_count<BS: Blockstore>(
        &mut self,
        store: &BS,
        hash: HashOutput,
    ) -> anyhow::Result<u16> {
        let hash_byte_key = BytesKey::from(hash.as_slice());

        self.submission_counts.modify(store, |hamt| {
            let new_count = hamt.get(&hash_byte_key)?.map(|v| v + 1).unwrap_or(1);

            // update the new count
            hamt.set(hash_byte_key, new_count)?;

            // now we compare with the most submitted hash or cron checkpoint
            if self.most_voted_hash.is_none() {
                // no most submitted hash set yet, set to current
                self.most_voted_hash = Some(hash);
                return Ok(new_count);
            }

            let most_submitted_hash = self.most_voted_hash.as_mut().unwrap();

            // the current submission is already one of the most submitted entries
            if most_submitted_hash == &hash {
                // the current submission is already the only one submission, no need update

                // return the current checkpoint's count as the current most submitted checkpoint
                return Ok(new_count);
            }

            // the current submission is not part of the most submitted entries, need to check
            // the most submitted entry to compare if the current submission is exceeding

            let most_submitted_key = BytesKey::from(most_submitted_hash.as_slice());

            // safe to unwrap as the hamt must contain the key
            let most_submitted_count = hamt.get(&most_submitted_key)?.unwrap();

            // current submission is not the most voted checkpoints
            // if new_count < *most_submitted_count, we do nothing as the new count is not close to the most submitted
            if new_count > *most_submitted_count {
                *most_submitted_hash = hash;
                Ok(new_count)
            } else {
                Ok(*most_submitted_count)
            }
        })
    }

    /// Checks if the submitter has already submitted the checkpoint. Currently used only in
    /// tests, but can be used in prod as well.
    #[cfg(test)]
    fn has_submitted<BS: Blockstore>(
        &self,
        store: &BS,
        submitter: &Address,
    ) -> anyhow::Result<bool> {
        let addr_byte_key = BytesKey::from(submitter.to_bytes());
        let hamt = self.submitters.load(store)?;
        Ok(hamt.contains_key(&addr_byte_key)?)
    }

    /// Checks if the checkpoint hash has already inserted in the store
    #[cfg(test)]
    fn has_checkpoint_inserted<BS: Blockstore>(
        &self,
        store: &BS,
        hash: &HashOutput,
    ) -> anyhow::Result<bool> {
        let hamt = self.submissions.load(store)?;
        Ok(hamt.contains_key(&BytesKey::from(hash.as_slice()))?)
    }

    /// Checks if the checkpoint hash has already inserted in the store
    #[cfg(test)]
    fn get_submission_count<BS: Blockstore>(
        &self,
        store: &BS,
        hash: &HashOutput,
    ) -> anyhow::Result<Option<u16>> {
        let hamt = self.submission_counts.load(store)?;
        let r = hamt.get(&BytesKey::from(hash.as_slice()))?;
        Ok(r.cloned())
    }
}

#[cfg(test)]
mod tests {
    use crate::{CronCheckpoint, CronSubmission, VoteExecutionStatus};
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_shared::address::Address;

    #[test]
    fn test_new_works() {
        let store = MemoryBlockstore::new();
        let r = CronSubmission::new(&store);
        assert!(r.is_ok());
    }

    #[test]
    fn test_update_submitters() {
        let store = MemoryBlockstore::new();
        let mut submission = CronSubmission::new(&store).unwrap();

        let submitter = Address::new_id(0);
        submission.update_submitters(&store, submitter).unwrap();
        assert!(submission.has_submitted(&store, &submitter).unwrap());

        // now submit again, but should fail
        assert!(submission.update_submitters(&store, submitter).is_err());
    }

    #[test]
    fn test_insert_checkpoint() {
        let store = MemoryBlockstore::new();
        let mut submission = CronSubmission::new(&store).unwrap();

        let checkpoint = CronCheckpoint {
            epoch: 100,
            top_down_msgs: vec![],
        };

        let hash = checkpoint.hash().unwrap();

        submission
            .insert_checkpoint(&store, checkpoint.clone())
            .unwrap();
        assert!(submission.has_checkpoint_inserted(&store, &hash).unwrap());

        // insert again should not have caused any error
        submission
            .insert_checkpoint(&store, checkpoint.clone())
            .unwrap();

        let inserted_checkpoint = submission.get_submission(&store, &hash).unwrap().unwrap();
        assert_eq!(inserted_checkpoint, checkpoint);
    }

    #[test]
    fn test_update_submission_count() {
        let store = MemoryBlockstore::new();
        let mut submission = CronSubmission::new(&store).unwrap();

        let hash1 = vec![1, 2, 1];
        let hash2 = vec![1, 2, 2];

        // insert hash1, should have only one item
        assert_eq!(submission.most_voted_hash, None);
        assert_eq!(
            submission
                .update_submission_count(&store, hash1.clone())
                .unwrap(),
            1
        );
        assert_eq!(
            submission
                .get_submission_count(&store, &hash1)
                .unwrap()
                .unwrap(),
            1
        );
        assert_eq!(submission.most_voted_hash, Some(hash1.clone()));

        // insert hash2, we should have two items, and there is a tie, hash1 still the most voted
        assert_eq!(
            submission
                .update_submission_count(&store, hash2.clone())
                .unwrap(),
            1
        );
        assert_eq!(
            submission
                .get_submission_count(&store, &hash2)
                .unwrap()
                .unwrap(),
            1
        );
        assert_eq!(
            submission
                .get_submission_count(&store, &hash1)
                .unwrap()
                .unwrap(),
            1
        );
        assert_eq!(submission.most_voted_hash, Some(hash1.clone()));

        // insert hash2 again, we should have only 1 most submitted hash
        assert_eq!(
            submission
                .update_submission_count(&store, hash2.clone())
                .unwrap(),
            2
        );
        assert_eq!(
            submission
                .get_submission_count(&store, &hash2)
                .unwrap()
                .unwrap(),
            2
        );
        assert_eq!(submission.most_voted_hash, Some(hash2.clone()));

        // insert hash2 again, we should have only 1 most submitted hash, but count incr by 1
        assert_eq!(
            submission
                .update_submission_count(&store, hash2.clone())
                .unwrap(),
            3
        );
        assert_eq!(
            submission
                .get_submission_count(&store, &hash2)
                .unwrap()
                .unwrap(),
            3
        );
        assert_eq!(submission.most_voted_hash, Some(hash2.clone()));
    }

    #[test]
    fn test_derive_execution_status() {
        let store = MemoryBlockstore::new();
        let mut s = CronSubmission::new(&store).unwrap();

        let total_validators = 35;
        let total_submissions = 10;
        let most_voted_count = 5;

        s.total_submissions = total_submissions;
        assert_eq!(
            s.derive_execution_status(total_validators, most_voted_count),
            VoteExecutionStatus::ThresholdNotReached,
        );

        // We could have 3 submissions: A, B, C
        // Current submissions and their counts are: A - 2, B - 2.
        // If the threshold is 1 / 2, we could have:
        //      If the last vote is C, then we should abort.
        //      If the last vote is any of A or B, we can execute.
        // If the threshold is 1 / 3, we have to abort.
        let total_validators = 5;
        let total_submissions = 4;
        let most_voted_count = 2;
        s.total_submissions = total_submissions;
        assert_eq!(
            s.derive_execution_status(total_submissions, most_voted_count),
            VoteExecutionStatus::RoundAbort,
        );

        // We could have 1 submission: A
        // Current submissions and their counts are: A - 4.
        let total_submissions = 4;
        let most_voted_count = 4;
        s.total_submissions = total_submissions;
        assert_eq!(
            s.derive_execution_status(total_validators, most_voted_count),
            VoteExecutionStatus::ConsensusReached,
        );

        // We could have 2 submission: A, B
        // Current submissions and their counts are: A - 3, B - 1.
        // Say the threshold is 2 / 3. If the last vote is B, we should abort, if the last vote is
        // A, then we have reached consensus. The current votes are in conclusive.
        let total_submissions = 4;
        let most_voted_count = 3;
        s.total_submissions = total_submissions;
        assert_eq!(
            s.derive_execution_status(total_validators, most_voted_count),
            VoteExecutionStatus::ReachingConsensus,
        );
    }
}