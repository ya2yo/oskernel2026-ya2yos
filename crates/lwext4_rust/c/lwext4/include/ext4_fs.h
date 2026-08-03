/*
 * Copyright (c) 2013 Grzegorz Kostka (kostka.grzegorz@gmail.com)
 *
 *
 * HelenOS:
 * Copyright (c) 2012 Martin Sucha
 * Copyright (c) 2012 Frantisek Princ
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 *
 * - Redistributions of source code must retain the above copyright
 *   notice, this list of conditions and the following disclaimer.
 * - Redistributions in binary form must reproduce the above copyright
 *   notice, this list of conditions and the following disclaimer in the
 *   documentation and/or other materials provided with the distribution.
 * - The name of the author may not be used to endorse or promote products
 *   derived from this software without specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
 * OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
 * IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
 * NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
 * THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
 * THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

/** @addtogroup lwext4
 * @{
 */
/**
 * @file  ext4_fs.c
 * @brief More complex filesystem functions.
 */

#ifndef EXT4_FS_H_
#define EXT4_FS_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <ext4_config.h>
#include <ext4_types.h>
#include <ext4_misc.h>

#include <stdint.h>
#include <stdbool.h>

/*
 * lwext4 historically relied on one caller supplied mount lock.  That works
 * for a single-threaded embedded caller, but turns every pathname operation
 * into a filesystem-wide critical section in an SMP kernel.  Keep locking
 * state with the mounted filesystem instead: namespace changes, inode data,
 * block-group bitmaps, superblock counters, the single journal handle and
 * cache-mode nesting are independent resources.
 *
 * The implementation deliberately uses striped locks.  EXT4 inode and block
 * group numbers are stable resource identities, while a fixed-size table
 * keeps the mount structure bounded for embedded users of lwext4.  A stripe
 * collision only adds contention; it never weakens mutual exclusion.
 */
#define EXT4_FS_LOCK_STRIPES 257U

struct ext4_fs_rwlock {
	int state;
};

/*
 * lwext4 is also used by small single-threaded environments, where a raw
 * atomic lock is sufficient.  A preemptible SMP kernel needs a different
 * waiting discipline: spinning all runnable CPUs while the lock owner is
 * descheduled can prevent that owner from ever running again.  The optional
 * hooks below let the embedding kernel park waiters on the *actual resource
 * lock* (inode, block group, journal, cache, ...), instead of adding a
 * mount-wide admission lock around every API call.
 *
 * Hooks are process-wide because lock addresses uniquely identify live
 * `struct ext4_fs` members.  They must be installed before the first mount
 * and remain valid until all mounts are gone.  With no hooks installed the
 * original atomic-spin behaviour remains available for lwext4's standalone
 * and host test users.
 */
typedef void (*ext4_fs_rwlock_lock_hook_t)(void *ctx,
						   struct ext4_fs_rwlock *lock,
						   bool write);
typedef void (*ext4_fs_rwlock_unlock_hook_t)(void *ctx,
						     struct ext4_fs_rwlock *lock,
						     bool write);

void ext4_fs_rwlock_set_hooks(void *ctx,
				      ext4_fs_rwlock_lock_hook_t lock_hook,
				      ext4_fs_rwlock_unlock_hook_t unlock_hook);
void ext4_fs_rwlock_read_lock(struct ext4_fs_rwlock *lock);
void ext4_fs_rwlock_read_unlock(struct ext4_fs_rwlock *lock);
void ext4_fs_rwlock_write_lock(struct ext4_fs_rwlock *lock);
void ext4_fs_rwlock_write_unlock(struct ext4_fs_rwlock *lock);

struct ext4_fs {
	bool read_only;

	struct ext4_blockdev *bdev;
	struct ext4_sblock sb;

	uint64_t inode_block_limits[4];
	uint64_t inode_blocks_per_level[4];

	uint32_t last_inode_bg_id;

	struct jbd_fs *jbd_fs;
	struct jbd_journal *jbd_journal;
	struct jbd_trans *curr_trans;

	/* See the locking note above.  All fields are zero-initialized at mount. */
	struct ext4_fs_rwlock namespace_lock;
	struct ext4_fs_rwlock inode_locks[EXT4_FS_LOCK_STRIPES];
	struct ext4_fs_rwlock group_locks[EXT4_FS_LOCK_STRIPES];
	struct ext4_fs_rwlock super_lock;
	struct ext4_fs_rwlock journal_lock;
	struct ext4_fs_rwlock cache_lock;
	/*
	 * The Rust lock is recursive for the owning task, so transaction helpers
	 * may nest while sharing one jbd transaction.  Keep the C-side nesting
	 * state separately from the lock implementation; a boolean lets an inner
	 * stop commit and detach the outer transaction too early.
	 */
	uint32_t journal_trans_depth;
	bool journal_trans_abort_pending;
	/* True while the current task owns journal_lock for a transaction scope. */
	bool journal_lock_held;
};

static inline struct ext4_fs_rwlock *
ext4_fs_inode_lock_ptr(struct ext4_fs *fs, uint32_t inode)
{
	return &fs->inode_locks[inode % EXT4_FS_LOCK_STRIPES];
}

static inline struct ext4_fs_rwlock *
ext4_fs_group_lock_ptr(struct ext4_fs *fs, uint32_t group)
{
	return &fs->group_locks[group % EXT4_FS_LOCK_STRIPES];
}

static inline void ext4_fs_inode_read_lock(struct ext4_fs *fs, uint32_t inode)
{
	ext4_fs_rwlock_read_lock(ext4_fs_inode_lock_ptr(fs, inode));
}

static inline void ext4_fs_inode_read_unlock(struct ext4_fs *fs, uint32_t inode)
{
	ext4_fs_rwlock_read_unlock(ext4_fs_inode_lock_ptr(fs, inode));
}

static inline void ext4_fs_inode_write_lock(struct ext4_fs *fs, uint32_t inode)
{
	ext4_fs_rwlock_write_lock(ext4_fs_inode_lock_ptr(fs, inode));
}

static inline void ext4_fs_inode_write_unlock(struct ext4_fs *fs, uint32_t inode)
{
	ext4_fs_rwlock_write_unlock(ext4_fs_inode_lock_ptr(fs, inode));
}

static inline void ext4_fs_group_write_lock(struct ext4_fs *fs, uint32_t group)
{
	ext4_fs_rwlock_write_lock(ext4_fs_group_lock_ptr(fs, group));
}

static inline void ext4_fs_group_write_unlock(struct ext4_fs *fs, uint32_t group)
{
	ext4_fs_rwlock_write_unlock(ext4_fs_group_lock_ptr(fs, group));
}

struct ext4_block_group_ref {
	struct ext4_block block;
	struct ext4_bgroup *block_group;
	struct ext4_fs *fs;
	uint32_t index;
	bool dirty;
};

struct ext4_inode_ref {
	struct ext4_block block;
	struct ext4_inode *inode;
	struct ext4_fs *fs;
	uint32_t index;
	bool dirty;
};


/**@brief Convert block address to relative index in block group.
 * @param s Superblock pointer
 * @param baddr Block number to convert
 * @return Relative number of block
 */
static inline uint32_t ext4_fs_addr_to_idx_bg(struct ext4_sblock *s,
						     ext4_fsblk_t baddr)
{
	if (ext4_get32(s, first_data_block) && baddr)
		baddr--;

	return baddr % ext4_get32(s, blocks_per_group);
}

/**@brief Convert relative block address in group to absolute address.
 * @param s Superblock pointer
 * @param index Relative block address
 * @param bgid Block group
 * @return Absolute block address
 */
static inline ext4_fsblk_t ext4_fs_bg_idx_to_addr(struct ext4_sblock *s,
						     uint32_t index,
						     uint32_t bgid)
{
	if (ext4_get32(s, first_data_block))
		index++;

	return ext4_get32(s, blocks_per_group) * bgid + index;
}

/**@brief TODO: */
static inline ext4_fsblk_t ext4_fs_first_bg_block_no(struct ext4_sblock *s,
						 uint32_t bgid)
{
	return (uint64_t)bgid * ext4_get32(s, blocks_per_group) +
	       ext4_get32(s, first_data_block);
}

/**@brief Initialize filesystem and read all needed data.
 * @param fs Filesystem instance to be initialized
 * @param bdev Identifier if device with the filesystem
 * @param read_only Mark the filesystem as read-only.
 * @return Error code
 */
int ext4_fs_init(struct ext4_fs *fs, struct ext4_blockdev *bdev,
		 bool read_only);

/**@brief Destroy filesystem instance (used by unmount operation).
 * @param fs Filesystem to be destroyed
 * @return Error code
 */
int ext4_fs_fini(struct ext4_fs *fs);

/**@brief Check filesystem's features, if supported by this driver
 * Function can return EOK and set read_only flag. It mean's that
 * there are some not-supported features, that can cause problems
 * during some write operations.
 * @param fs        Filesystem to be checked
 * @param read_only Flag if filesystem should be mounted only for reading
 * @return Error code
 */
int ext4_fs_check_features(struct ext4_fs *fs, bool *read_only);

/**@brief Get reference to block group specified by index.
 * @param fs   Filesystem to find block group on
 * @param bgid Index of block group to load
 * @param ref  Output pointer for reference
 * @return Error code
 */
int ext4_fs_get_block_group_ref(struct ext4_fs *fs, uint32_t bgid,
				struct ext4_block_group_ref *ref);

/**@brief Put reference to block group.
 * @param ref Pointer for reference to be put back
 * @return Error code
 */
int ext4_fs_put_block_group_ref(struct ext4_block_group_ref *ref);

/**@brief Get reference to i-node specified by index.
 * @param fs    Filesystem to find i-node on
 * @param index Index of i-node to load
 * @param ref   Output pointer for reference
 * @return Error code
 */
int ext4_fs_get_inode_ref(struct ext4_fs *fs, uint32_t index,
			  struct ext4_inode_ref *ref);

/**@brief Reset blocks field of i-node.
 * @param fs        Filesystem to reset blocks field of i-inode on
 * @param inode_ref ref Pointer for inode to be operated on
 */
void ext4_fs_inode_blocks_init(struct ext4_fs *fs,
			       struct ext4_inode_ref *inode_ref);

/**@brief Put reference to i-node.
 * @param ref Pointer for reference to be put back
 * @return Error code
 */
int ext4_fs_put_inode_ref(struct ext4_inode_ref *ref);

/**@brief Convert filetype to inode mode.
 * @param filetype File type
 * @return inode mode
 */
uint32_t ext4_fs_correspond_inode_mode(int filetype);

/**@brief Allocate new i-node in the filesystem.
 * @param fs        Filesystem to allocated i-node on
 * @param inode_ref Output pointer to return reference to allocated i-node
 * @param filetype  File type of newly created i-node
 * @return Error code
 */
int ext4_fs_alloc_inode(struct ext4_fs *fs, struct ext4_inode_ref *inode_ref,
			int filetype);

/**@brief Release i-node and mark it as free.
 * @param inode_ref I-node to be released
 * @return Error code
 */
int ext4_fs_free_inode(struct ext4_inode_ref *inode_ref);

/**@brief Truncate i-node data blocks.
 * @param inode_ref I-node to be truncated
 * @param new_size  New size of inode (must be < current size)
 * @return Error code
 */
int ext4_fs_truncate_inode(struct ext4_inode_ref *inode_ref, uint64_t new_size);

/**@brief Compute 'goal' for inode index
 * @param inode_ref Reference to inode, to allocate block for
 * @return goal
 */
ext4_fsblk_t ext4_fs_inode_to_goal_block(struct ext4_inode_ref *inode_ref);

/**@brief Compute 'goal' for allocation algorithm (For blockmap).
 * @param inode_ref Reference to inode, to allocate block for
 * @return error code
 */
int ext4_fs_indirect_find_goal(struct ext4_inode_ref *inode_ref,
				ext4_fsblk_t *goal);

/**@brief Get physical block address by logical index of the block.
 * @param inode_ref I-node to read block address from
 * @param iblock            Logical index of block
 * @param fblock            Output pointer for return physical
 *                          block address
 * @param support_unwritten Indicate whether unwritten block range
 *                          is supported under the current context
 * @return Error code
 */
int ext4_fs_get_inode_dblk_idx(struct ext4_inode_ref *inode_ref,
				 ext4_lblk_t iblock, ext4_fsblk_t *fblock,
				 bool support_unwritten);

/**@brief Initialize a part of unwritten range of the inode.
 * @param inode_ref I-node to proceed on.
 * @param iblock    Logical index of block
 * @param fblock    Output pointer for return physical block address
 * @return Error code
 */
int ext4_fs_init_inode_dblk_idx(struct ext4_inode_ref *inode_ref,
				  ext4_lblk_t iblock, ext4_fsblk_t *fblock);

/**@brief Append following logical block to the i-node.
 * @param inode_ref I-node to append block to
 * @param fblock    Output physical block address of newly allocated block
 * @param iblock    Output logical number of newly allocated block
 * @return Error code
 */
int ext4_fs_append_inode_dblk(struct ext4_inode_ref *inode_ref,
			      ext4_fsblk_t *fblock, ext4_lblk_t *iblock);

/**@brief   Increment inode link count.
 * @param   inode_ref none handle
 */
void ext4_fs_inode_links_count_inc(struct ext4_inode_ref *inode_ref);

/**@brief   Decrement inode link count.
 * @param   inode_ref none handle
 */
void ext4_fs_inode_links_count_dec(struct ext4_inode_ref *inode_ref);

#ifdef __cplusplus
}
#endif

#endif /* EXT4_FS_H_ */

/**
 * @}
 */
