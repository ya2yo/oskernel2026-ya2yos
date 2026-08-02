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

	return EOK;
}

void ext4_bcache_cleanup(struct ext4_bcache *bc)
{
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
		return EOK;
	}

	RB_INSERT(ext4_buf_lba, &bc->lba_root, allocated);
	bc->ref_blocks++;
	if (bc->max_ref_blocks < bc->ref_blocks)
		bc->max_ref_blocks = bc->ref_blocks;
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
	bool flush = false;
	int r = EOK;

	ext4_assert(bc && b);

	/*Check if valid.*/
	ext4_assert(b->lb_id);

	/*Block should have a valid pointer to ext4_buf.*/
	ext4_assert(buf);

	ext4_bcache_index_lock(bc);
	ext4_assert(buf->refctr);
	ext4_bcache_dec_ref(buf);

	if (!buf->refctr && ext4_bcache_test_flag(buf, BC_DIRTY) &&
	    ext4_bcache_test_flag(buf, BC_UPTODATE) &&
	    (!bc->bdev->cache_write_back ||
	     ext4_bcache_test_flag(buf, BC_FLUSH) ||
	     ext4_bcache_test_flag(buf, BC_TMP))) {
		/* Pin across writeback, but do not keep the index lock over I/O. */
		ext4_bcache_inc_ref(buf);
		flush = true;
	} else if (!buf->refctr) {
		RB_INSERT(ext4_buf_lru, &bc->lru_root, buf);
		if (ext4_bcache_test_flag(buf, BC_DIRTY) &&
		    ext4_bcache_test_flag(buf, BC_UPTODATE))
			ext4_bcache_insert_dirty_node(bc, buf);
		if (!ext4_bcache_test_flag(buf, BC_UPTODATE) ||
		    ext4_bcache_test_flag(buf, BC_TMP))
			ext4_bcache_drop_buf_locked(bc, buf);
	}
	ext4_bcache_index_unlock(bc);

	if (flush) {
		r = ext4_block_flush_buf(bc->bdev, buf);

		ext4_bcache_index_lock(bc);
		ext4_bcache_clear_flag(buf, BC_FLUSH);
		ext4_assert(buf->refctr);
		ext4_bcache_dec_ref(buf);
		if (!buf->refctr) {
			RB_INSERT(ext4_buf_lru, &bc->lru_root, buf);
			if (ext4_bcache_test_flag(buf, BC_DIRTY) &&
			    ext4_bcache_test_flag(buf, BC_UPTODATE))
				ext4_bcache_insert_dirty_node(bc, buf);
			if (!ext4_bcache_test_flag(buf, BC_UPTODATE) ||
			    ext4_bcache_test_flag(buf, BC_TMP))
				ext4_bcache_drop_buf_locked(bc, buf);
		}
		ext4_bcache_index_unlock(bc);
	}

	b->lb_id = 0;
	b->data = 0;

	return r;
}

bool ext4_bcache_is_full(struct ext4_bcache *bc)
{
	bool full;
	ext4_bcache_index_lock(bc);
	full = bc->cnt <= bc->ref_blocks;
	ext4_bcache_index_unlock(bc);
	return full;
}

void ext4_bcache_shake_clean(struct ext4_bcache *bc)
{
	ext4_bcache_index_lock(bc);
	while (bc->cnt <= bc->ref_blocks) {
		struct ext4_buf *buf = RB_MIN(ext4_buf_lru, &bc->lru_root);
		while (buf && ext4_bcache_test_flag(buf, BC_DIRTY))
			buf = RB_NEXT(ext4_buf_lru, &bc->lru_root, buf);
		if (!buf)
			break;
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


/**
 * @}
 */
