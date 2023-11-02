// Copyright 2023 TiKV Project Authors. Licensed under Apache-2.0.

// #[PerformanceCriticalPath]
use txn_types::{Key, Lock, LockType, TimeStamp};

use crate::storage::{
    kv::WriteData,
    lock_manager::LockManager,
    mvcc::{MvccTxn, SnapshotReader},
    txn::{
        commands::{
            Command, CommandExt, ReaderWithStats, ReleasedLocks, ResponsePolicy, TypedCommand,
            WriteCommand, WriteContext, WriteResult,
        },
        Result,
    },
    ProcessResult, Snapshot,
};

command! {
    MemBufferSet:
        cmd_ty => (),
        display => "kv::command::mem_buffer_set keys({:?}) @ {} | {:?}", (keys, start_ts, ctx),
        content => {
            start_ts: TimeStamp,
            primary: Vec<u8>,
            keys: Vec<Key>,
            flags: Vec<u16>,
            values: Vec<Vec<u8>>,
        }
}

impl CommandExt for MemBufferSet {
    ctx!();
    tag!(mem_buffer_set);
    request_type!(KvMemBufferSet);
    ts!(start_ts);
    write_bytes!(keys: multiple);
    gen_lock!(keys: multiple);
}

impl<S: Snapshot, L: LockManager> WriteCommand<S, L> for MemBufferSet {
    fn process_write(self, snapshot: S, context: WriteContext<'_, L>) -> Result<WriteResult> {
        let mut txn = MvccTxn::new(self.start_ts, context.concurrency_manager);
        let mut reader = ReaderWithStats::new(
            SnapshotReader::new_with_ctx(self.start_ts, snapshot, &self.ctx),
            context.statistics,
        );

        for (i, k) in self.keys.iter().enumerate() {
            if let Some(lock) = reader.load_lock(k)? {
                if lock.ts == self.start_ts {
                    continue;
                }
                return Err(crate::storage::txn::Error::from(
                    crate::storage::txn::ErrorInner::Mvcc(crate::storage::mvcc::Error(Box::new(
                        crate::storage::mvcc::ErrorInner::KeyIsLocked(
                            lock.into_lock_info(k.to_raw()?),
                        ),
                    ))),
                ));
            }

            let flag = self.flags[i];
            let value = self.values[i].clone();
            txn.put_lock(
                k.clone(),
                &Lock {
                    lock_type: if value.is_empty() {
                        LockType::Delete
                    } else {
                        LockType::Put
                    },
                    primary: self.primary.clone(),
                    ts: Default::default(),
                    ttl: 0,
                    short_value: None,
                    for_update_ts: Default::default(),
                    txn_size: 0,
                    min_commit_ts: Default::default(),
                    use_async_commit: false,
                    secondaries: vec![],
                    rollback_ts: vec![],
                    last_change: Default::default(),
                    txn_source: 0,
                    is_locked_with_conflict: false,
                    is_mem_buffer: false,
                    mem_buffer_flags: flag,
                    mem_buffer_value: value,
                },
                true,
            );
        }

        let new_locks = txn.take_new_locks();
        Ok(WriteResult {
            ctx: self.ctx,
            to_be_write: WriteData::from_modifies(txn.into_modifies()),
            rows: self.keys.len(),
            pr: ProcessResult::Res,
            lock_info: vec![],
            released_locks: ReleasedLocks::new(),
            new_acquired_locks: new_locks,
            lock_guards: vec![],
            response_policy: ResponsePolicy::OnApplied,
            known_txn_status: vec![],
        })
    }
}
