/*
 * Copyright (c) 2013 Grzegorz Kostka (kostka.grzegorz@gmail.com)
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
 * @file  ext4_bcache.c
 * @brief Block cache allocator.
 */

#include <ext4_config.h>
#include <ext4_types.h>
#include <ext4_bcache.h>
#include <ext4_blockdev.h>
#include <ext4_debug.h>
#include <ext4_errno.h>

#include <string.h>
#include <stdlib.h>

static int ext4_bcache_lba_compare(struct ext4_buf *a, struct ext4_buf *b)
{
	 if (a->lba > b->lba)
		 return 1;
	 else if (a->lba < b->lba)
		 return -1;
	 return 0;
}

static int ext4_bcache_lru_compare(struct ext4_buf *a, struct ext4_buf *b)
{
	if (a->lru_id > b->lru_id)
		return 1;
	else if (a->lru_id < b->lru_id)
		return -1;
	return 0;
}

RB_GENERATE_INTERNAL(ext4_buf_lba, ext4_buf, lba_node,
		     ext4_bcache_lba_compare, static inline)
RB_GENERATE_INTERNAL(ext4_buf_lru, ext4_buf, lru_node,
		     ext4_bcache_lru_compare, static inline)

static struct {
	void *ctx;
	ext4_bcache_wait_fn wait;
	ext4_bcache_wake_fn wake;
} ext4_bcache_sync;

static bool ext4_bcache_perf_enabled;
static struct ext4_bcache *ext4_bcache_perf_bc;
static struct ext4_bcache_perf_stats ext4_bcache_perf;

#define EXT4_BCACHE_PERF_INC(field) do { \
	if (__atomic_load_n(&ext4_bcache_perf_enabled, __ATOMIC_RELAXED)) \
		__atomic_add_fetch(&ext4_bcache_perf.field, 1, __ATOMIC_RELAXED); \
} while (0)

static void ext4_bcache_perf_update_max_resident(uint64_t resident)
{
	uint64_t observed = __atomic_load_n(
		&ext4_bcache_perf.max_resident_blocks, __ATOMIC_RELAXED);
	while (observed < resident &&
	       !__atomic_compare_exchange_n(
		       &ext4_bcache_perf.max_resident_blocks, &observed,
		       resident, true, __ATOMIC_RELAXED, __ATOMIC_RELAXED))
		;
}

static void ext4_bcache_perf_record_allocation(void)
{
	uint64_t resident;
	if (!__atomic_load_n(&ext4_bcache_perf_enabled, __ATOMIC_RELAXED))
		return;

	__atomic_add_fetch(&ext4_bcache_perf.allocations, 1, __ATOMIC_RELAXED);
	resident = __atomic_add_fetch(&ext4_bcache_perf.resident_blocks, 1,
				      __ATOMIC_RELAXED);
	ext4_bcache_perf_update_max_resident(resident);
}

static void ext4_bcache_perf_record_drop(void)
{
	if (!__atomic_load_n(&ext4_bcache_perf_enabled, __ATOMIC_RELAXED))
		return;

	__atomic_add_fetch(&ext4_bcache_perf.drops, 1, __ATOMIC_RELAXED);
	__atomic_sub_fetch(&ext4_bcache_perf.resident_blocks, 1,
			   __ATOMIC_RELAXED);
}

void ext4_bcache_perf_enable(bool enable)
{
	uint64_t resident = 0;
	__atomic_store_n(&ext4_bcache_perf_enabled, false, __ATOMIC_RELEASE);
	if (!enable)
		return;

	memset(&ext4_bcache_perf, 0, sizeof(ext4_bcache_perf));
	if (ext4_bcache_perf_bc)
		resident = __atomic_load_n(&ext4_bcache_perf_bc->ref_blocks,
					   __ATOMIC_RELAXED);
	ext4_bcache_perf.initial_resident_blocks = resident;
	ext4_bcache_perf.resident_blocks = resident;
	ext4_bcache_perf.max_resident_blocks = resident;
	__atomic_store_n(&ext4_bcache_perf_enabled, true, __ATOMIC_RELEASE);
}

void ext4_bcache_perf_snapshot(struct ext4_bcache_perf_stats *out)
{
	if (!out)
		return;

#define EXT4_BCACHE_PERF_LOAD(field) \
	out->field = __atomic_load_n(&ext4_bcache_perf.field, __ATOMIC_RELAXED)
	EXT4_BCACHE_PERF_LOAD(get_ops);
	EXT4_BCACHE_PERF_LOAD(cache_hits);
	EXT4_BCACHE_PERF_LOAD(cache_misses);
	EXT4_BCACHE_PERF_LOAD(allocations);
	EXT4_BCACHE_PERF_LOAD(allocation_races);
	EXT4_BCACHE_PERF_LOAD(loader_ops);
	EXT4_BCACHE_PERF_LOAD(loader_successes);
	EXT4_BCACHE_PERF_LOAD(loader_errors);
	EXT4_BCACHE_PERF_LOAD(wait_ops);
	EXT4_BCACHE_PERF_LOAD(wait_rechecks);
	EXT4_BCACHE_PERF_LOAD(wake_calls);
	EXT4_BCACHE_PERF_LOAD(shake_calls);
	EXT4_BCACHE_PERF_LOAD(clean_evictions);
	EXT4_BCACHE_PERF_LOAD(shake_full_dirty);
	EXT4_BCACHE_PERF_LOAD(shake_full_pinned);
	EXT4_BCACHE_PERF_LOAD(capacity_overflows);
	EXT4_BCACHE_PERF_LOAD(dirty_capacity_reclaim_runs);
	EXT4_BCACHE_PERF_LOAD(dirty_capacity_reclaimed_blocks);
	EXT4_BCACHE_PERF_LOAD(dirty_capacity_reclaim_stalls);
	EXT4_BCACHE_PERF_LOAD(writeback_ops);
	EXT4_BCACHE_PERF_LOAD(writeback_successes);
	EXT4_BCACHE_PERF_LOAD(writeback_errors);
	EXT4_BCACHE_PERF_LOAD(writeback_waits);
	EXT4_BCACHE_PERF_LOAD(drops);
	EXT4_BCACHE_PERF_LOAD(initial_resident_blocks);
	EXT4_BCACHE_PERF_LOAD(resident_blocks);
	EXT4_BCACHE_PERF_LOAD(max_resident_blocks);
	EXT4_BCACHE_PERF_LOAD(read_submits);
	EXT4_BCACHE_PERF_LOAD(read_completions);
	EXT4_BCACHE_PERF_LOAD(read_blocks);
	EXT4_BCACHE_PERF_LOAD(read_errors);
	EXT4_BCACHE_PERF_LOAD(write_submits);
	EXT4_BCACHE_PERF_LOAD(write_completions);
	EXT4_BCACHE_PERF_LOAD(write_blocks);
	EXT4_BCACHE_PERF_LOAD(write_errors);
#undef EXT4_BCACHE_PERF_LOAD
}

void ext4_bcache_perf_record_io_submit(bool write, uint32_t blocks)
{
	if (!__atomic_load_n(&ext4_bcache_perf_enabled, __ATOMIC_RELAXED))
		return;

	if (write) {
		__atomic_add_fetch(&ext4_bcache_perf.write_submits, 1,
				   __ATOMIC_RELAXED);
		__atomic_add_fetch(&ext4_bcache_perf.write_blocks, blocks,
				   __ATOMIC_RELAXED);
	} else {
		__atomic_add_fetch(&ext4_bcache_perf.read_submits, 1,
				   __ATOMIC_RELAXED);
		__atomic_add_fetch(&ext4_bcache_perf.read_blocks, blocks,
				   __ATOMIC_RELAXED);
	}
}

void ext4_bcache_perf_record_io_complete(bool write, int result)
{
	if (!__atomic_load_n(&ext4_bcache_perf_enabled, __ATOMIC_RELAXED))
		return;

	if (write) {
		__atomic_add_fetch(&ext4_bcache_perf.write_completions, 1,
				   __ATOMIC_RELAXED);
		if (result != EOK)
			__atomic_add_fetch(&ext4_bcache_perf.write_errors, 1,
					   __ATOMIC_RELAXED);
	} else {
		__atomic_add_fetch(&ext4_bcache_perf.read_completions, 1,
				   __ATOMIC_RELAXED);
		if (result != EOK)
			__atomic_add_fetch(&ext4_bcache_perf.read_errors, 1,
					   __ATOMIC_RELAXED);
	}
}

void ext4_bcache_perf_record_get(bool hit)
{
	if (!__atomic_load_n(&ext4_bcache_perf_enabled, __ATOMIC_RELAXED))
		return;
	__atomic_add_fetch(&ext4_bcache_perf.get_ops, 1, __ATOMIC_RELAXED);
	if (hit)
		__atomic_add_fetch(&ext4_bcache_perf.cache_hits, 1,
				   __ATOMIC_RELAXED);
	else
		__atomic_add_fetch(&ext4_bcache_perf.cache_misses, 1,
				   __ATOMIC_RELAXED);
}

void ext4_bcache_perf_record_load_wait(bool first_wait)
{
	if (!__atomic_load_n(&ext4_bcache_perf_enabled, __ATOMIC_RELAXED))
		return;
	if (first_wait)
		__atomic_add_fetch(&ext4_bcache_perf.wait_ops, 1,
				   __ATOMIC_RELAXED);
	__atomic_add_fetch(&ext4_bcache_perf.wait_rechecks, 1,
			   __ATOMIC_RELAXED);
}

void ext4_bcache_perf_record_loader_start(void)
{
	EXT4_BCACHE_PERF_INC(loader_ops);
}

void ext4_bcache_perf_record_loader_complete(int result)
{
	if (result == EOK)
		EXT4_BCACHE_PERF_INC(loader_successes);
	else
		EXT4_BCACHE_PERF_INC(loader_errors);
}

void ext4_bcache_perf_record_writeback_wait(void)
{
	EXT4_BCACHE_PERF_INC(writeback_waits);
}

void ext4_bcache_perf_record_writeback_start(void)
{
	EXT4_BCACHE_PERF_INC(writeback_ops);
}

void ext4_bcache_perf_record_writeback_complete(int result)
{
	if (result == EOK)
		EXT4_BCACHE_PERF_INC(writeback_successes);
	else
		EXT4_BCACHE_PERF_INC(writeback_errors);
}

void ext4_bcache_perf_record_dirty_capacity_reclaim_run(void)
{
	EXT4_BCACHE_PERF_INC(dirty_capacity_reclaim_runs);
}

void ext4_bcache_perf_record_dirty_capacity_reclaimed_block(void)
{
	EXT4_BCACHE_PERF_INC(dirty_capacity_reclaimed_blocks);
}

void ext4_bcache_perf_record_dirty_capacity_reclaim_stall(void)
{
	EXT4_BCACHE_PERF_INC(dirty_capacity_reclaim_stalls);
}

static inline void ext4_bcache_index_lock(struct ext4_bcache *bc)
{
	while (__atomic_exchange_n(&bc->index_lock, 1, __ATOMIC_ACQUIRE)) {
		while (__atomic_load_n(&bc->index_lock, __ATOMIC_RELAXED))
			__asm__ volatile("" ::: "memory");
	}
}

static inline void ext4_bcache_index_unlock(struct ext4_bcache *bc)
{
	__atomic_store_n(&bc->index_lock, 0, __ATOMIC_RELEASE);
}

void ext4_bcache_setup_sync(void *ctx, ext4_bcache_wait_fn wait,
			    ext4_bcache_wake_fn wake)
{
	ext4_bcache_sync.ctx = ctx;
	ext4_bcache_sync.wait = wait;
	ext4_bcache_sync.wake = wake;
}

int ext4_bcache_wait_while(struct ext4_buf *buf, int mask)
{
	ext4_assert(buf);
	if (ext4_bcache_sync.wait)
		return ext4_bcache_sync.wait(ext4_bcache_sync.ctx, &buf->flags,
					      mask, buf->lba);

	while (__atomic_load_n(&buf->flags, __ATOMIC_ACQUIRE) & mask)
		__asm__ volatile("" ::: "memory");
	return EOK;
}

void ext4_bcache_wake(struct ext4_buf *buf)
{
	EXT4_BCACHE_PERF_INC(wake_calls);
	if (ext4_bcache_sync.wake)
		ext4_bcache_sync.wake(ext4_bcache_sync.ctx, buf->lba);
}

int ext4_bcache_init_dynamic(struct ext4_bcache *bc, uint32_t cnt,
			     uint32_t itemsize)
{
	ext4_assert(bc && cnt && itemsize);

	memset(bc, 0, sizeof(struct ext4_bcache));

	bc->cnt = cnt;
	bc->itemsize = itemsize;
	bc->ref_blocks = 0;
	bc->max_ref_blocks = 0;
	ext4_bcache_perf_bc = bc;

	return EOK;
}

void ext4_bcache_cleanup(struct ext4_bcache *bc)
{
	if (!bc)
		return;

	for (;;) {
		struct ext4_buf *buf;
		struct ext4_block block = EXT4_BLOCK_ZERO();

		ext4_bcache_index_lock(bc);
		buf = RB_MIN(ext4_buf_lba, &bc->lba_root);
		if (!buf) {
			ext4_bcache_index_unlock(bc);
			break;
		}
		if (!buf->refctr) {
			RB_REMOVE(ext4_buf_lru, &bc->lru_root, buf);
			if (buf->on_dirty_list)
				ext4_bcache_remove_dirty_node(bc, buf);
		}
		ext4_bcache_inc_ref(buf);
		block.lb_id = buf->lba;
		block.buf = buf;
		block.data = buf->data;
		ext4_bcache_index_unlock(bc);

		/* A failed mount may never have bound the cache to a device. */
		if (bc->bdev)
			ext4_block_flush_buf(bc->bdev, buf);
		ext4_bcache_free(bc, &block);
		ext4_bcache_drop_buf(bc, buf);
	}
}

int ext4_bcache_fini_dynamic(struct ext4_bcache *bc)
{
	memset(bc, 0, sizeof(struct ext4_bcache));
	return EOK;
}

/**@brief:
 *
 *  This is ext4_bcache, the module handling basic buffer-cache stuff.
 *
 *  Buffers in a bcache are sorted by their LBA and stored in a
 *  RB-Tree(lba_root).
 *
 *  Bcache also maintains another RB-Tree(lru_root) right now, where
 *  buffers are sorted by their LRU id.
 *
 *  A singly-linked list is used to track those dirty buffers which are
 *  ready to be flushed. (Those buffers which are dirty but also referenced
 *  are not considered ready to be flushed.)
 *
 *  When a buffer is not referenced, it will be stored in both lba_root
 *  and lru_root, while it will only be stored in lba_root when it is
 *  referenced.
 */

static struct ext4_buf *
ext4_buf_alloc(struct ext4_bcache *bc, uint64_t lba)
{
	void *data;
	struct ext4_buf *buf;
	data = ext4_malloc(bc->itemsize);
	if (!data)
		return NULL;

	buf = ext4_calloc(1, sizeof(struct ext4_buf));
	if (!buf) {
		ext4_free(data);
		return NULL;
	}

	buf->lba = lba;
	buf->data = data;
	buf->bc = bc;
	return buf;
}

static void ext4_buf_free(struct ext4_buf *buf)
{
	ext4_free(buf->data);
	ext4_free(buf);
}

static struct ext4_buf *
ext4_buf_lookup(struct ext4_bcache *bc, uint64_t lba)
{
	struct ext4_buf tmp = {
		.lba = lba
	};

	return RB_FIND(ext4_buf_lba, &bc->lba_root, &tmp);
}

static void ext4_bcache_drop_buf_locked(struct ext4_bcache *bc,
					struct ext4_buf *buf)
{
	ext4_assert(!buf->refctr);
	RB_REMOVE(ext4_buf_lru, &bc->lru_root, buf);
	RB_REMOVE(ext4_buf_lba, &bc->lba_root, buf);
	if (buf->on_dirty_list)
		ext4_bcache_remove_dirty_node(bc, buf);
	ext4_buf_free(buf);
	ext4_assert(bc->ref_blocks);
	bc->ref_blocks--;
	ext4_bcache_perf_record_drop();
}

static void ext4_bcache_reference_locked(struct ext4_bcache *bc,
					 struct ext4_buf *buf,
					 struct ext4_block *b)
{
	if (!buf->refctr) {
		buf->lru_id = ++bc->lru_ctr;
		RB_REMOVE(ext4_buf_lru, &bc->lru_root, buf);
		if (buf->on_dirty_list)
			ext4_bcache_remove_dirty_node(bc, buf);
	}

	ext4_bcache_inc_ref(buf);
	b->lb_id = buf->lba;
	b->buf = buf;
	b->data = buf->data;
}

static bool ext4_bcache_release_locked(struct ext4_bcache *bc,
					struct ext4_buf *buf,
					bool allow_writeback,
					bool discard_clean)
{
	struct ext4_buf *duplicate;

	ext4_assert(buf->refctr);
	ext4_bcache_dec_ref(buf);
	if (buf->refctr)
		return false;

	if (allow_writeback && ext4_bcache_test_flag(buf, BC_DIRTY) &&
	    ext4_bcache_test_flag(buf, BC_UPTODATE) &&
	    (!bc->bdev || !bc->bdev->cache_write_back ||
	     ext4_bcache_test_flag(buf, BC_FLUSH) ||
	     ext4_bcache_test_flag(buf, BC_TMP))) {
		/* Pin across writeback, but do not keep the index lock over I/O. */
		ext4_bcache_inc_ref(buf);
		return true;
	}

	duplicate = RB_INSERT(ext4_buf_lru, &bc->lru_root, buf);
	ext4_assert(!duplicate);
	if (discard_clean && ext4_bcache_test_flag(buf, BC_UPTODATE) &&
	    !ext4_bcache_test_flag(buf, BC_DIRTY)) {
		/* drop_buf_locked expects the unreferenced object in the LRU tree. */
		ext4_bcache_drop_buf_locked(bc, buf);
		return false;
	}
	if (ext4_bcache_test_flag(buf, BC_DIRTY) &&
	    ext4_bcache_test_flag(buf, BC_UPTODATE))
		ext4_bcache_insert_dirty_node(bc, buf);
	if (!ext4_bcache_test_flag(buf, BC_UPTODATE) ||
	    ext4_bcache_test_flag(buf, BC_TMP))
		ext4_bcache_drop_buf_locked(bc, buf);
	return false;
}

struct ext4_buf *ext4_buf_lowest_lru(struct ext4_bcache *bc)
{
	struct ext4_buf *buf;
	ext4_bcache_index_lock(bc);
	buf = RB_MIN(ext4_buf_lru, &bc->lru_root);
	ext4_bcache_index_unlock(bc);
	return buf;
}

void ext4_bcache_drop_buf(struct ext4_bcache *bc, struct ext4_buf *buf)
{
	ext4_bcache_index_lock(bc);
	if (buf->refctr) {
		ext4_dbg(DEBUG_BCACHE, DBG_WARN "Buffer is still referenced. "
				"lba: %" PRIu64 ", refctr: %" PRIu32 "\n",
				buf->lba, buf->refctr);
		ext4_bcache_index_unlock(bc);
		return;
	}
	ext4_bcache_drop_buf_locked(bc, buf);
	ext4_bcache_index_unlock(bc);
}

void ext4_bcache_invalidate_buf(struct ext4_bcache *bc,
				struct ext4_buf *buf)
{
	ext4_bcache_index_lock(bc);
	buf->end_write = NULL;
	buf->end_write_arg = NULL;

	/* Clear both dirty and up-to-date flags. */
	if (ext4_bcache_test_flag(buf, BC_DIRTY))
		ext4_bcache_remove_dirty_node(bc, buf);

	ext4_bcache_clear_dirty(buf);
	ext4_bcache_index_unlock(bc);
}

void ext4_bcache_invalidate_lba(struct ext4_bcache *bc,
				uint64_t from,
				uint32_t cnt)
{
	uint64_t end;
	struct ext4_buf *tmp, *buf;

	if (!cnt)
		return;
	end = from + cnt - 1;

	ext4_bcache_index_lock(bc);
	tmp = ext4_buf_lookup(bc, from);
	RB_FOREACH_FROM(buf, ext4_buf_lba, tmp) {
		if (buf->lba > end)
			break;

		buf->end_write = NULL;
		buf->end_write_arg = NULL;
		if (buf->on_dirty_list)
			ext4_bcache_remove_dirty_node(bc, buf);
		ext4_bcache_clear_dirty(buf);
	}
	ext4_bcache_index_unlock(bc);
}

struct ext4_buf *
ext4_bcache_find_get(struct ext4_bcache *bc, struct ext4_block *b,
		     uint64_t lba)
{
	struct ext4_buf *buf;
	ext4_bcache_index_lock(bc);
	buf = ext4_buf_lookup(bc, lba);
	if (buf)
		ext4_bcache_reference_locked(bc, buf, b);
	ext4_bcache_index_unlock(bc);
	return buf;
}

int ext4_bcache_alloc(struct ext4_bcache *bc, struct ext4_block *b,
		      bool *is_new)
{
	struct ext4_buf *buf;
	struct ext4_buf *allocated;
	uint64_t lba = b->lb_id;

	/* Try to search the buffer with exact LBA. */
	buf = ext4_bcache_find_get(bc, b, lba);
	if (buf) {
		*is_new = false;
		return EOK;
	}

	/* Allocation stays outside the cache index spin lock. */
	allocated = ext4_buf_alloc(bc, lba);
	if (!allocated)
		return ENOMEM;

	ext4_bcache_index_lock(bc);
	buf = ext4_buf_lookup(bc, lba);
	if (buf) {
		ext4_bcache_reference_locked(bc, buf, b);
		*is_new = false;
		ext4_bcache_index_unlock(bc);
		ext4_buf_free(allocated);
		EXT4_BCACHE_PERF_INC(allocation_races);
		return EOK;
	}

	RB_INSERT(ext4_buf_lba, &bc->lba_root, allocated);
	bc->ref_blocks++;
	ext4_bcache_perf_record_allocation();
	if (bc->max_ref_blocks < bc->ref_blocks)
		bc->max_ref_blocks = bc->ref_blocks;
	if (bc->ref_blocks > bc->cnt)
		EXT4_BCACHE_PERF_INC(capacity_overflows);
	allocated->lru_id = ++bc->lru_ctr;
	ext4_bcache_inc_ref(allocated);
	b->lb_id = lba;
	b->buf = allocated;
	b->data = allocated->data;
	*is_new = true;
	ext4_bcache_index_unlock(bc);
	return EOK;
}

int ext4_bcache_free(struct ext4_bcache *bc, struct ext4_block *b)
{
	struct ext4_buf *buf = b->buf;
	bool flush;
	int r = EOK;

	ext4_assert(bc && b);

	/*Check if valid.*/
	ext4_assert(b->lb_id);

	/*Block should have a valid pointer to ext4_buf.*/
	ext4_assert(buf);

	ext4_bcache_index_lock(bc);
	flush = ext4_bcache_release_locked(bc, buf, true, false);
	ext4_bcache_index_unlock(bc);

	if (flush) {
		if (!bc->bdev || buf->bc != bc) {
			/* The device detached while this reference was released. */
			r = EIO;
		} else {
			r = ext4_block_flush_buf(bc->bdev, buf);
		}

		ext4_bcache_index_lock(bc);
		ext4_bcache_clear_flag(buf, BC_FLUSH);
		ext4_bcache_release_locked(bc, buf, false, false);
		ext4_bcache_index_unlock(bc);
	}

	b->lb_id = 0;
	b->buf = NULL;
	b->data = 0;

	return r;
}

bool ext4_bcache_claim_dirty(struct ext4_bcache *bc, struct ext4_block *b)
{
	struct ext4_buf *buf;

	ext4_assert(bc && b);
	*b = (struct ext4_block)EXT4_BLOCK_ZERO();

	ext4_bcache_index_lock(bc);
	buf = SLIST_FIRST(&bc->dirty_list);
	if (buf) {
		ext4_assert(buf->bc == bc);
		ext4_assert(!buf->refctr);
		ext4_assert(buf->on_dirty_list);
		ext4_assert(ext4_bcache_test_flag(buf, BC_DIRTY));
		ext4_assert(ext4_bcache_test_flag(buf, BC_UPTODATE));
		ext4_bcache_reference_locked(bc, buf, b);
	}
	ext4_bcache_index_unlock(bc);
	return buf != NULL;
}

void ext4_bcache_release_dirty(struct ext4_bcache *bc, struct ext4_block *b)
{
	struct ext4_buf *buf;

	ext4_assert(bc && b && b->buf);
	buf = b->buf;
	ext4_bcache_index_lock(bc);
	ext4_bcache_release_locked(bc, buf, false, false);
	ext4_bcache_index_unlock(bc);
	b->lb_id = 0;
	b->buf = NULL;
	b->data = NULL;
}

void ext4_bcache_release_dirty_reclaim(struct ext4_bcache *bc,
					       struct ext4_block *b)
{
	struct ext4_buf *buf;

	ext4_assert(bc && b && b->buf);
	buf = b->buf;
	ext4_bcache_index_lock(bc);
	ext4_bcache_release_locked(bc, buf, false, true);
	ext4_bcache_index_unlock(bc);
	b->lb_id = 0;
	b->buf = NULL;
	b->data = NULL;
}

bool ext4_bcache_is_full(struct ext4_bcache *bc)
{
	bool full;
	ext4_bcache_index_lock(bc);
	full = bc->cnt <= bc->ref_blocks;
	ext4_bcache_index_unlock(bc);
	return full;
}

bool ext4_bcache_reached_limit(struct ext4_bcache *bc, uint32_t limit)
{
	bool reached;

	ext4_assert(bc && limit);
	ext4_bcache_index_lock(bc);
	reached = bc->ref_blocks >= limit;
	ext4_bcache_index_unlock(bc);
	return reached;
}

void ext4_bcache_shake_clean(struct ext4_bcache *bc)
{
	EXT4_BCACHE_PERF_INC(shake_calls);
	ext4_bcache_index_lock(bc);
	while (bc->cnt <= bc->ref_blocks) {
		struct ext4_buf *first = RB_MIN(ext4_buf_lru, &bc->lru_root);
		struct ext4_buf *buf = first;
		while (buf && ext4_bcache_test_flag(buf, BC_DIRTY))
			buf = RB_NEXT(ext4_buf_lru, &bc->lru_root, buf);
		if (!buf) {
			if (first)
				EXT4_BCACHE_PERF_INC(shake_full_dirty);
			else
				EXT4_BCACHE_PERF_INC(shake_full_pinned);
			break;
		}
		EXT4_BCACHE_PERF_INC(clean_evictions);
		ext4_bcache_drop_buf_locked(bc, buf);
	}
	ext4_bcache_index_unlock(bc);
}

void ext4_bcache_mark_clean(struct ext4_bcache *bc, struct ext4_buf *buf)
{
	ext4_bcache_index_lock(bc);
	if (buf->on_dirty_list)
		ext4_bcache_remove_dirty_node(bc, buf);
	ext4_bcache_clear_flag(buf, BC_DIRTY);
	ext4_bcache_index_unlock(bc);
}

static int ext4_bcache_validate_dirty_contains_locked(
	struct ext4_bcache *bc, struct ext4_buf *target, bool *contains)
{
	struct ext4_buf *buf;
	uint32_t visited = 0;

	*contains = false;
	SLIST_FOREACH(buf, &bc->dirty_list, dirty_node) {
		if (++visited > bc->ref_blocks)
			return EIO;
		if (buf->bc != bc || !buf->on_dirty_list || buf->refctr ||
		    !ext4_bcache_test_flag(buf, BC_DIRTY) ||
		    !ext4_bcache_test_flag(buf, BC_UPTODATE) ||
		    RB_FIND(ext4_buf_lba, &bc->lba_root, buf) != buf ||
		    RB_FIND(ext4_buf_lru, &bc->lru_root, buf) != buf)
			return EIO;
		if (buf == target)
			*contains = true;
	}

	return EOK;
}

#define EXT4_BCACHE_VALIDATE_MAX_DEPTH 64U

struct ext4_bcache_validate_stats {
	uint32_t lba_count;
	uint32_t lru_count;
	uint32_t unreferenced_count;
	uint32_t expected_dirty_count;
};

static int ext4_bcache_validate_lba_subtree_locked(
	struct ext4_bcache *bc, struct ext4_buf *buf,
	struct ext4_buf *parent, bool have_min, uint64_t min_lba,
	bool have_max, uint64_t max_lba, uint32_t depth,
	struct ext4_bcache_validate_stats *stats, uint32_t *black_height)
{
	struct ext4_buf *left;
	struct ext4_buf *right;
	uint32_t left_black_height;
	uint32_t right_black_height;
	bool dirty_contains;
	bool should_be_dirty;
	int flags;
	int r;

	if (!buf) {
		*black_height = 1;
		return EOK;
	}
	if (depth > EXT4_BCACHE_VALIDATE_MAX_DEPTH ||
	    ++stats->lba_count > bc->ref_blocks || buf->bc != bc ||
	    RB_PARENT(buf, lba_node) != parent ||
	    (have_min && buf->lba <= min_lba) ||
	    (have_max && buf->lba >= max_lba) ||
	    (RB_COLOR(buf, lba_node) != RB_RED &&
	     RB_COLOR(buf, lba_node) != RB_BLACK) ||
	    RB_FIND(ext4_buf_lba, &bc->lba_root, buf) != buf)
		return EIO;

	flags = __atomic_load_n(&buf->flags, __ATOMIC_ACQUIRE);
	if (!buf->refctr) {
		stats->unreferenced_count++;
		if (RB_FIND(ext4_buf_lru, &bc->lru_root, buf) != buf)
			return EIO;
	} else if (RB_FIND(ext4_buf_lru, &bc->lru_root, buf)) {
		return EIO;
	}
	if (!buf->refctr &&
	    (flags & ((1 << BC_LOADING) | (1 << BC_WRITEBACK) |
		      (1 << BC_EVICTING))))
		return EIO;

	should_be_dirty = !buf->refctr && (flags & (1 << BC_DIRTY)) &&
		(flags & (1 << BC_UPTODATE));
	if (should_be_dirty)
		stats->expected_dirty_count++;
	r = ext4_bcache_validate_dirty_contains_locked(bc, buf,
							  &dirty_contains);
	if (r != EOK || dirty_contains != buf->on_dirty_list ||
	    dirty_contains != should_be_dirty)
		return EIO;

	left = RB_LEFT(buf, lba_node);
	right = RB_RIGHT(buf, lba_node);
	if (RB_COLOR(buf, lba_node) == RB_RED &&
	    ((left && RB_COLOR(left, lba_node) == RB_RED) ||
	     (right && RB_COLOR(right, lba_node) == RB_RED)))
		return EIO;
	r = ext4_bcache_validate_lba_subtree_locked(
		bc, left, buf, have_min, min_lba, true, buf->lba, depth + 1,
		stats, &left_black_height);
	if (r != EOK)
		return r;
	r = ext4_bcache_validate_lba_subtree_locked(
		bc, right, buf, true, buf->lba, have_max, max_lba, depth + 1,
		stats, &right_black_height);
	if (r != EOK || left_black_height != right_black_height)
		return EIO;
	*black_height = left_black_height +
		(RB_COLOR(buf, lba_node) == RB_BLACK ? 1U : 0U);
	return EOK;
}

static int ext4_bcache_validate_lru_subtree_locked(
	struct ext4_bcache *bc, struct ext4_buf *buf,
	struct ext4_buf *parent, bool have_min, uint32_t min_lru,
	bool have_max, uint32_t max_lru, uint32_t depth,
	struct ext4_bcache_validate_stats *stats, uint32_t *black_height)
{
	struct ext4_buf *left;
	struct ext4_buf *right;
	uint32_t left_black_height;
	uint32_t right_black_height;
	int r;

	if (!buf) {
		*black_height = 1;
		return EOK;
	}
	if (depth > EXT4_BCACHE_VALIDATE_MAX_DEPTH ||
	    ++stats->lru_count > bc->ref_blocks || buf->bc != bc ||
	    buf->refctr || RB_PARENT(buf, lru_node) != parent ||
	    (have_min && buf->lru_id <= min_lru) ||
	    (have_max && buf->lru_id >= max_lru) ||
	    (RB_COLOR(buf, lru_node) != RB_RED &&
	     RB_COLOR(buf, lru_node) != RB_BLACK) ||
	    RB_FIND(ext4_buf_lru, &bc->lru_root, buf) != buf ||
	    RB_FIND(ext4_buf_lba, &bc->lba_root, buf) != buf)
		return EIO;

	left = RB_LEFT(buf, lru_node);
	right = RB_RIGHT(buf, lru_node);
	if (RB_COLOR(buf, lru_node) == RB_RED &&
	    ((left && RB_COLOR(left, lru_node) == RB_RED) ||
	     (right && RB_COLOR(right, lru_node) == RB_RED)))
		return EIO;
	r = ext4_bcache_validate_lru_subtree_locked(
		bc, left, buf, have_min, min_lru, true, buf->lru_id, depth + 1,
		stats, &left_black_height);
	if (r != EOK)
		return r;
	r = ext4_bcache_validate_lru_subtree_locked(
		bc, right, buf, true, buf->lru_id, have_max, max_lru, depth + 1,
		stats, &right_black_height);
	if (r != EOK || left_black_height != right_black_height)
		return EIO;
	*black_height = left_black_height +
		(RB_COLOR(buf, lru_node) == RB_BLACK ? 1U : 0U);
	return EOK;
}

int ext4_bcache_validate(struct ext4_bcache *bc)
{
	struct ext4_bcache_validate_stats stats = { 0 };
	struct ext4_buf *buf;
	uint32_t dirty_count = 0;
	uint32_t black_height;
	int r = EOK;

	if (!bc)
		return EINVAL;

	ext4_bcache_index_lock(bc);
	if ((RB_ROOT(&bc->lba_root) &&
	     RB_COLOR(RB_ROOT(&bc->lba_root), lba_node) != RB_BLACK) ||
	    (RB_ROOT(&bc->lru_root) &&
	     RB_COLOR(RB_ROOT(&bc->lru_root), lru_node) != RB_BLACK)) {
		r = EIO;
		goto Finish;
	}

	SLIST_FOREACH(buf, &bc->dirty_list, dirty_node) {
		if (++dirty_count > bc->ref_blocks || buf->bc != bc ||
		    !buf->on_dirty_list || buf->refctr ||
		    !ext4_bcache_test_flag(buf, BC_DIRTY) ||
		    !ext4_bcache_test_flag(buf, BC_UPTODATE) ||
		    RB_FIND(ext4_buf_lba, &bc->lba_root, buf) != buf ||
		    RB_FIND(ext4_buf_lru, &bc->lru_root, buf) != buf) {
			r = EIO;
			goto Finish;
		}
	}

	r = ext4_bcache_validate_lba_subtree_locked(
		bc, RB_ROOT(&bc->lba_root), NULL, false, 0, false, 0, 1,
		&stats, &black_height);
	if (r != EOK || stats.lba_count != bc->ref_blocks ||
	    bc->max_ref_blocks < bc->ref_blocks) {
		r = EIO;
		goto Finish;
	}
	r = ext4_bcache_validate_lru_subtree_locked(
		bc, RB_ROOT(&bc->lru_root), NULL, false, 0, false, 0, 1,
		&stats, &black_height);
	if (r != EOK || stats.lru_count != stats.unreferenced_count ||
	    dirty_count != stats.expected_dirty_count) {
		r = EIO;
		goto Finish;
	}

Finish:
	ext4_bcache_index_unlock(bc);
	return r;
}


/**
 * @}
 */
