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
 * @file  ext4_blockdev.c
 * @brief Block device module.
 */

#include <ext4_config.h>
#include <ext4_types.h>
#include <ext4_misc.h>
#include <ext4_errno.h>
#include <ext4_debug.h>

#include <ext4_blockdev.h>
#include <ext4_fs.h>
#include <ext4_journal.h>

#include <string.h>
#include <stdlib.h>

/* Keep staged writes below the block-device request limit. */
#define EXT4_BLOCK_CACHE_FLUSH_BATCH_MAX_BLOCKS 32U

static void ext4_bdif_lock(struct ext4_blockdev *bdev)
{
	if (!bdev->bdif->lock)
		return;

	int r = bdev->bdif->lock(bdev);
	ext4_assert(r == EOK);
}

static void ext4_bdif_unlock(struct ext4_blockdev *bdev)
{
	if (!bdev->bdif->unlock)
		return;

	int r = bdev->bdif->unlock(bdev);
	ext4_assert(r == EOK);
}

static int ext4_bdif_bread(struct ext4_blockdev *bdev, void *buf,
			   uint64_t blk_id, uint32_t blk_cnt)
{
	ext4_bcache_perf_record_io_submit(false, blk_cnt);
	ext4_bdif_lock(bdev);
	int r = bdev->bdif->bread(bdev, buf, blk_id, blk_cnt);
	ext4_bcache_perf_record_io_complete(false, r);
	__atomic_add_fetch(&bdev->bdif->bread_ctr, 1, __ATOMIC_RELAXED);
	ext4_bdif_unlock(bdev);
	return r;
}

static int ext4_bdif_bwrite(struct ext4_blockdev *bdev, const void *buf,
			    uint64_t blk_id, uint32_t blk_cnt)
{
	ext4_bcache_perf_record_io_submit(true, blk_cnt);
	ext4_bdif_lock(bdev);
	int r = bdev->bdif->bwrite(bdev, buf, blk_id, blk_cnt);
	ext4_bcache_perf_record_io_complete(true, r);
	__atomic_add_fetch(&bdev->bdif->bwrite_ctr, 1, __ATOMIC_RELAXED);
	ext4_bdif_unlock(bdev);
	return r;
}

int ext4_block_init(struct ext4_blockdev *bdev)
{
	int rc;
	ext4_assert(bdev);
	ext4_assert(bdev->bdif);
	ext4_assert(bdev->bdif->open &&
		   bdev->bdif->close &&
		   bdev->bdif->bread &&
		   bdev->bdif->bwrite);

	if (bdev->bdif->ph_refctr) {
		bdev->bdif->ph_refctr++;
		return EOK;
	}

	/*Low level block init*/
	rc = bdev->bdif->open(bdev);
	if (rc != EOK)
		return rc;

	bdev->bdif->ph_refctr = 1;
	return EOK;
}

int ext4_block_bind_bcache(struct ext4_blockdev *bdev, struct ext4_bcache *bc)
{
	ext4_assert(bdev && bc);
	bdev->bc = bc;
	bc->bdev = bdev;
	return EOK;
}

void ext4_block_set_lb_size(struct ext4_blockdev *bdev, uint32_t lb_bsize)
{
	/*Logical block size has to be multiply of physical */
	ext4_assert(!(lb_bsize % bdev->bdif->ph_bsize));

	bdev->lg_bsize = lb_bsize;
	bdev->lg_bcnt = bdev->part_size / lb_bsize;
}

int ext4_block_fini(struct ext4_blockdev *bdev)
{
	ext4_assert(bdev);

	if (!bdev->bdif->ph_refctr)
		return EOK;

	bdev->bdif->ph_refctr--;
	if (bdev->bdif->ph_refctr)
		return EOK;

	/*Low level block fini*/
	return bdev->bdif->close(bdev);
}

int ext4_block_flush_buf(struct ext4_blockdev *bdev, struct ext4_buf *buf)
{
	int r = EOK;
	struct ext4_bcache *bc;
	int flags;
	const int writeback = 1 << BC_WRITEBACK;
	bool waited = false;

	/*
	 * Cleanup of a failed mount can encounter a cache that was initialized
	 * before it was bound, and an abandoned operation can observe a cache
	 * after its device has been detached. Do not turn that lifecycle error
	 * into a kernel page fault while trying to write a dirty buffer.
	 */
	if (!bdev || !buf || !buf->bc || buf->bc->bdev != bdev)
		return EIO;
	bc = bdev->bc;

	for (;;) {
		flags = __atomic_load_n(&buf->flags, __ATOMIC_ACQUIRE);
		if (!(flags & (1 << BC_DIRTY)) ||
		    !(flags & (1 << BC_UPTODATE)))
			return EOK;
		if (flags & writeback) {
			if (!waited) {
				ext4_bcache_perf_record_writeback_wait();
				waited = true;
			}
			r = ext4_bcache_wait_while(buf, writeback);
			if (r != EOK)
				return r;
			continue;
		}
		if (__atomic_compare_exchange_n(&buf->flags, &flags,
						flags | writeback, false,
						__ATOMIC_ACQ_REL,
						__ATOMIC_ACQUIRE))
			break;
	}
	ext4_bcache_perf_record_writeback_start();

	if (ext4_bcache_test_flag(buf, BC_DIRTY) &&
	    ext4_bcache_test_flag(buf, BC_UPTODATE)) {
		r = ext4_blocks_set_direct(bdev, buf->data, buf->lba, 1);
		if (r) {
			if (buf->end_write)
				buf->end_write(bc, buf, r, buf->end_write_arg);
			goto Finish;
		}

		ext4_bcache_mark_clean(bc, buf);
		if (buf->end_write)
			buf->end_write(bc, buf, r, buf->end_write_arg);
	}

Finish:
	ext4_bcache_perf_record_writeback_complete(r);
	ext4_bcache_clear_flag(buf, BC_WRITEBACK);
	ext4_bcache_wake(buf);
	return r;
}

int ext4_block_flush_lba(struct ext4_blockdev *bdev, uint64_t lba)
{
	int r = EOK;
	struct ext4_buf *buf;
	struct ext4_block b;
	buf = ext4_bcache_find_get(bdev->bc, &b, lba);
	if (buf) {
		r = ext4_block_flush_buf(bdev, buf);
		ext4_bcache_free(bdev->bc, &b);
	}
	return r;
}

/*
 * Journal checkpoint buffers retain their individual end_write callbacks.
 * Callback-free adjacent buffers can use one request without changing that
 * completion order.
 */
static int ext4_block_flush_contiguous_claims(struct ext4_blockdev *bdev,
					      struct ext4_block *blocks,
					      uint32_t count, int direction)
{
	struct ext4_bcache *bc = bdev->bc;
	uint8_t *data;
	uint32_t i;
	uint32_t data_index;
	uint64_t first_lba;
	size_t bytes;
	int r;

	if (count == 1)
		return ext4_block_flush_buf(bdev, blocks[0].buf);

	/* Allocation failure retains the former one-buffer-at-a-time behavior. */
	if (!bdev->lg_bsize || bdev->lg_bsize > UINT32_MAX / count)
		goto FlushIndividually;
	bytes = (size_t)bdev->lg_bsize * count;
	data = ext4_malloc(bytes);
	if (!data)
		goto FlushIndividually;

	for (i = 0; i < count; ++i) {
		struct ext4_buf *buf = blocks[i].buf;

		ext4_assert(buf && !buf->end_write);
		ext4_assert(ext4_bcache_test_flag(buf, BC_DIRTY));
		ext4_assert(ext4_bcache_test_flag(buf, BC_UPTODATE));
		ext4_assert(!ext4_bcache_test_flag(buf, BC_WRITEBACK));
		ext4_bcache_set_flag(buf, BC_WRITEBACK);
		ext4_bcache_perf_record_writeback_start();

		data_index = direction < 0 ? count - i - 1 : i;
		memcpy(data + (size_t)data_index * bdev->lg_bsize, buf->data,
		       bdev->lg_bsize);
	}
	first_lba = direction < 0 ? blocks[count - 1].lb_id : blocks[0].lb_id;
	r = ext4_blocks_set_direct(bdev, data, first_lba, count);

	for (i = 0; i < count; ++i) {
		struct ext4_buf *buf = blocks[i].buf;

		if (r == EOK)
			ext4_bcache_mark_clean(bc, buf);
		ext4_bcache_perf_record_writeback_complete(r);
		ext4_bcache_clear_flag(buf, BC_WRITEBACK);
		ext4_bcache_wake(buf);
	}
	ext4_free(data);
	return r;

FlushIndividually:
	for (i = 0; i < count; ++i) {
		r = ext4_block_flush_buf(bdev, blocks[i].buf);
		if (r != EOK)
			return r;
	}
	return EOK;
}

static bool ext4_block_can_extend_contiguous_flush(
		const struct ext4_block *previous, const struct ext4_block *next,
		int *direction)
{
	int next_direction;

	if (next->buf->end_write)
		return false;
	if (previous->lb_id == next->lb_id + 1)
		next_direction = -1;
	else if (next->lb_id == previous->lb_id + 1)
		next_direction = 1;
	else
		return false;
	if (*direction && *direction != next_direction)
		return false;
	*direction = next_direction;
	return true;
}

#if CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_EXPERIMENT
static int ext4_block_cache_reclaim_dirty(struct ext4_blockdev *bdev)
{
	struct ext4_bcache *bc = bdev->bc;

	if (!ext4_bcache_reached_limit(
			bc, CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK))
		return EOK;

	ext4_bcache_perf_record_dirty_capacity_reclaim_run();
	while (ext4_bcache_reached_limit(
		bc, CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_LOW_WATERMARK + 1U)) {
		struct ext4_block block = EXT4_BLOCK_ZERO();
		int r;

		if (!ext4_bcache_claim_dirty(bc, &block)) {
			ext4_bcache_perf_record_dirty_capacity_reclaim_stall();
			break;
		}

		r = ext4_block_flush_buf(bdev, block.buf);
		if (r == EOK) {
			ext4_bcache_release_dirty_reclaim(bc, &block);
			ext4_bcache_perf_record_dirty_capacity_reclaimed_block();
		} else {
			/* Keep the failed buffer dirty for the next explicit retry. */
			ext4_bcache_release_dirty(bc, &block);
			return r;
		}
	}

	return EOK;
}
#endif

int ext4_block_cache_shake(struct ext4_blockdev *bdev)
{
	ext4_bcache_shake_clean(bdev->bc);
#if CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_EXPERIMENT
	return ext4_block_cache_reclaim_dirty(bdev);
#else
	return EOK;
#endif
}

int ext4_block_get_noread(struct ext4_blockdev *bdev, struct ext4_block *b,
			  uint64_t lba)
{
	bool is_new;
	int r;
	struct ext4_buf *cached;

	ext4_assert(bdev && b);

	if (!bdev->bdif->ph_refctr)
		return EIO;

	if (!(lba < bdev->lg_bcnt))
		return ENXIO;

	b->lb_id = lba;

	/*
	 * A resident block already owns a cache reference. Check it before
	 * applying capacity pressure: the old order scanned the entire LRU (and,
	 * with dirty-capacity reclaim enabled, could write back 128 blocks) even
	 * when the requested LBA was a hit. This is especially costly for the
	 * metadata-heavy parallel BuildStorm workload.
	 */
	cached = ext4_bcache_find_get(bdev->bc, b, lba);
	if (cached) {
		if (!b->data)
			return ENOMEM;
		return EOK;
	}

	/*If cache is full we have to (flush and) drop it anyway :(*/
	r = ext4_block_cache_shake(bdev);
	if (r != EOK)
		return r;

	r = ext4_bcache_alloc(bdev->bc, b, &is_new);
	if (r != EOK)
		return r;

	if (!b->data)
		return ENOMEM;

	return EOK;
}

int ext4_block_get(struct ext4_blockdev *bdev, struct ext4_block *b,
			   uint64_t lba)
{
	int r = ext4_block_get_noread(bdev, b, lba);
	bool waited = false;
	bool classified = false;
	bool wait_recorded = false;
	const int loading = 1 << BC_LOADING;
	const int io_error = 1 << BC_IO_ERROR;
	if (r != EOK)
		return r;

	for (;;) {
		int flags = __atomic_load_n(&b->buf->flags, __ATOMIC_ACQUIRE);
		int desired;
		if (!classified) {
			ext4_bcache_perf_record_get(flags & (1 << BC_UPTODATE));
			classified = true;
		}

		if (flags & (1 << BC_UPTODATE))
			return EOK;
		if (flags & loading) {
			ext4_bcache_perf_record_load_wait(!wait_recorded);
			wait_recorded = true;
			r = ext4_bcache_wait_while(b->buf, loading);
			if (r != EOK)
				goto Error;
			waited = true;
			continue;
		}
		if ((flags & io_error) && waited) {
			r = EIO;
			goto Error;
		}

		desired = (flags & ~io_error) | loading;
		if (!__atomic_compare_exchange_n(&b->buf->flags, &flags, desired,
						 false, __ATOMIC_ACQ_REL,
						 __ATOMIC_ACQUIRE))
			continue;

		ext4_bcache_perf_record_loader_start();
		r = ext4_blocks_get_direct(bdev, b->data, lba, 1);
		if (r == EOK) {
			ext4_bcache_set_flag(b->buf, BC_UPTODATE);
			ext4_bcache_clear_flag(b->buf, BC_IO_ERROR);
		} else {
			ext4_bcache_clear_flag(b->buf, BC_UPTODATE);
			ext4_bcache_set_flag(b->buf, BC_IO_ERROR);
		}
		ext4_bcache_perf_record_loader_complete(r);
		ext4_bcache_clear_flag(b->buf, BC_LOADING);
		ext4_bcache_wake(b->buf);
		if (r == EOK)
			return EOK;
		goto Error;
	}

Error:
	ext4_bcache_free(bdev->bc, b);
	b->lb_id = 0;
	return r;
}

int ext4_block_set(struct ext4_blockdev *bdev, struct ext4_block *b)
{
	ext4_assert(bdev && b);
	ext4_assert(b->buf);

	if (!bdev->bdif->ph_refctr)
		return EIO;

	return ext4_bcache_free(bdev->bc, b);
}

int ext4_blocks_get_direct(struct ext4_blockdev *bdev, void *buf, uint64_t lba,
			   uint32_t cnt)
{
	uint64_t pba;
	uint32_t pb_cnt;

	ext4_assert(bdev && buf);

	pba = (lba * bdev->lg_bsize + bdev->part_offset) / bdev->bdif->ph_bsize;
	pb_cnt = bdev->lg_bsize / bdev->bdif->ph_bsize;

	return ext4_bdif_bread(bdev, buf, pba, pb_cnt * cnt);
}

int ext4_blocks_set_direct(struct ext4_blockdev *bdev, const void *buf,
			   uint64_t lba, uint32_t cnt)
{
	uint64_t pba;
	uint32_t pb_cnt;

	ext4_assert(bdev && buf);

	pba = (lba * bdev->lg_bsize + bdev->part_offset) / bdev->bdif->ph_bsize;
	pb_cnt = bdev->lg_bsize / bdev->bdif->ph_bsize;

	return ext4_bdif_bwrite(bdev, buf, pba, pb_cnt * cnt);
}

int ext4_block_writebytes(struct ext4_blockdev *bdev, uint64_t off,
			  const void *buf, uint32_t len)
{
	uint64_t block_idx;
	uint32_t blen;
	uint32_t unalg;
	int r = EOK;
	uint8_t *scratch = NULL;

	const uint8_t *p = (void *)buf;

	ext4_assert(bdev && buf);

	if (!bdev->bdif->ph_refctr)
		return EIO;

	if (off > bdev->part_size || len > bdev->part_size - off)
		return EINVAL; /*Ups. Out of range operation*/

	if ((off & (bdev->bdif->ph_bsize - 1)) ||
	    (len & (bdev->bdif->ph_bsize - 1))) {
		scratch = ext4_malloc(bdev->bdif->ph_bsize);
		if (!scratch)
			return ENOMEM;
	}

	block_idx = ((off + bdev->part_offset) / bdev->bdif->ph_bsize);

	/*OK lets deal with the first possible unaligned block*/
	unalg = (off & (bdev->bdif->ph_bsize - 1));
	if (unalg) {

		uint32_t wlen = (bdev->bdif->ph_bsize - unalg) > len
				    ? len
				    : (bdev->bdif->ph_bsize - unalg);

		r = ext4_bdif_bread(bdev, scratch, block_idx, 1);
		if (r != EOK)
			goto Finish;

		memcpy(scratch + unalg, p, wlen);
		r = ext4_bdif_bwrite(bdev, scratch, block_idx, 1);
		if (r != EOK)
			goto Finish;

		p += wlen;
		len -= wlen;
		block_idx++;
	}

	/*Aligned data*/
	blen = len / bdev->bdif->ph_bsize;
	if (blen != 0) {
		r = ext4_bdif_bwrite(bdev, p, block_idx, blen);
		if (r != EOK)
			goto Finish;

		p += bdev->bdif->ph_bsize * blen;
		len -= bdev->bdif->ph_bsize * blen;

		block_idx += blen;
	}

	/*Rest of the data*/
	if (len) {
		r = ext4_bdif_bread(bdev, scratch, block_idx, 1);
		if (r != EOK)
			goto Finish;

		memcpy(scratch, p, len);
		r = ext4_bdif_bwrite(bdev, scratch, block_idx, 1);
		if (r != EOK)
			goto Finish;
	}

Finish:
	if (scratch)
		ext4_free(scratch);
	return r;
}

int ext4_block_readbytes(struct ext4_blockdev *bdev, uint64_t off, void *buf,
			 uint32_t len)
{
	uint64_t block_idx;
	uint32_t blen;
	uint32_t unalg;
	int r = EOK;
	uint8_t *scratch = NULL;

	uint8_t *p = (void *)buf;

	ext4_assert(bdev && buf);

	if (!bdev->bdif->ph_refctr)
		return EIO;

	if (off > bdev->part_size || len > bdev->part_size - off)
		return EINVAL; /*Ups. Out of range operation*/

	if ((off & (bdev->bdif->ph_bsize - 1)) ||
	    (len & (bdev->bdif->ph_bsize - 1))) {
		scratch = ext4_malloc(bdev->bdif->ph_bsize);
		if (!scratch)
			return ENOMEM;
	}

	block_idx = ((off + bdev->part_offset) / bdev->bdif->ph_bsize);

	/*OK lets deal with the first possible unaligned block*/
	unalg = (off & (bdev->bdif->ph_bsize - 1));
	if (unalg) {

		uint32_t rlen = (bdev->bdif->ph_bsize - unalg) > len
				    ? len
				    : (bdev->bdif->ph_bsize - unalg);

		r = ext4_bdif_bread(bdev, scratch, block_idx, 1);
		if (r != EOK)
			goto Finish;

		memcpy(p, scratch + unalg, rlen);

		p += rlen;
		len -= rlen;
		block_idx++;
	}

	/*Aligned data*/
	blen = len / bdev->bdif->ph_bsize;

	if (blen != 0) {
		r = ext4_bdif_bread(bdev, p, block_idx, blen);
		if (r != EOK)
			goto Finish;

		p += bdev->bdif->ph_bsize * blen;
		len -= bdev->bdif->ph_bsize * blen;

		block_idx += blen;
	}

	/*Rest of the data*/
	if (len) {
		r = ext4_bdif_bread(bdev, scratch, block_idx, 1);
		if (r != EOK)
			goto Finish;

		memcpy(p, scratch, len);
	}

Finish:
	if (scratch)
		ext4_free(scratch);
	return r;
}

int ext4_block_cache_flush(struct ext4_blockdev *bdev)
{
	struct ext4_block pending = EXT4_BLOCK_ZERO();
	struct ext4_bcache *bc = bdev->bc;

	for (;;) {
		struct ext4_block blocks[EXT4_BLOCK_CACHE_FLUSH_BATCH_MAX_BLOCKS];
		uint32_t count = 0;
		int direction = 0;
		int r;

		if (pending.buf) {
			blocks[count++] = pending;
			pending = (struct ext4_block)EXT4_BLOCK_ZERO();
		} else if (!ext4_bcache_claim_dirty(bc, &blocks[count])) {
			return EOK;
		} else {
			count++;
		}

		if (!blocks[0].buf->end_write) {
			while (count < EXT4_BLOCK_CACHE_FLUSH_BATCH_MAX_BLOCKS) {
				struct ext4_block next = EXT4_BLOCK_ZERO();

				if (!ext4_bcache_claim_dirty(bc, &next))
					break;
				if (!ext4_block_can_extend_contiguous_flush(
						&blocks[count - 1], &next, &direction)) {
					pending = next;
					break;
				}
				blocks[count++] = next;
			}
		}

		r = ext4_block_flush_contiguous_claims(bdev, blocks, count,
						       direction);
		for (uint32_t i = 0; i < count; ++i)
			ext4_bcache_release_dirty(bc, &blocks[i]);
		if (r != EOK && pending.buf)
			ext4_bcache_release_dirty(bc, &pending);
		if (r != EOK)
			return r;
	}
}

int ext4_block_cache_write_back(struct ext4_blockdev *bdev, uint8_t on_off)
{
	int r;
	bool flush;

	/*
	 * cache_write_back is a mount-wide nesting counter, not a per-file flag.
	 * Keep its short state update separate from the potentially long bcache
	 * writeback.  The flush lock gates transitions through the zero point so a
	 * new write-back scope cannot add dirty blocks while the previous scope is
	 * draining them. A flush may invoke journal checkpoint callbacks, so keep
	 * the global lock order journal -> cache flush -> cache state.
	 */
	flush = false;
	if (bdev->fs)
		ext4_fs_rwlock_write_lock(&bdev->fs->journal_lock);
	if (bdev->fs)
		ext4_fs_rwlock_write_lock(&bdev->fs->cache_flush_lock);
	if (bdev->fs)
		ext4_fs_rwlock_write_lock(&bdev->fs->cache_lock);
	if (on_off)
		bdev->cache_write_back++;

	if (!on_off && bdev->cache_write_back)
		bdev->cache_write_back--;

	flush = !on_off && bdev->cache_write_back == 0;
	if (bdev->fs)
		ext4_fs_rwlock_write_unlock(&bdev->fs->cache_lock);

	if (!flush) {
		if (bdev->fs)
			ext4_fs_rwlock_write_unlock(&bdev->fs->cache_flush_lock);
		if (bdev->fs)
			ext4_fs_rwlock_write_unlock(&bdev->fs->journal_lock);
		return EOK;
	}

	/* Flush data in all delayed cache blocks without the metadata state lock. */
	r = ext4_block_cache_flush(bdev);
	if (bdev->fs)
		ext4_fs_rwlock_write_unlock(&bdev->fs->cache_flush_lock);
	if (bdev->fs)
		ext4_fs_rwlock_write_unlock(&bdev->fs->journal_lock);
	return r;
}

/**
 * @}
 */
