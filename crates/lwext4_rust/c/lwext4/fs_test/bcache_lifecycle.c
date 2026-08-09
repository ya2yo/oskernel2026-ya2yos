#define _XOPEN_SOURCE 700

#include <ext4_bcache.h>
#include <ext4_blockdev.h>
#include <ext4_errno.h>

#include <pthread.h>
#include <sched.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define TEST_BLOCK_SIZE 64U
#define TEST_BLOCK_COUNT 512U
#define TEST_THREADS 8U
#define TEST_RANDOM_STEPS 100000U
#define TEST_HELD_BLOCKS 8U

struct test_disk {
	uint8_t storage[TEST_BLOCK_COUNT][TEST_BLOCK_SIZE];
	uint32_t read_attempts[TEST_BLOCK_COUNT];
	uint32_t reads[TEST_BLOCK_COUNT];
	uint32_t write_attempts[TEST_BLOCK_COUNT];
	uint32_t writes[TEST_BLOCK_COUNT];
	uint32_t write_requests;
	uint32_t max_write_blocks;
	uint32_t read_delay_yields;
	bool fail_next_read;
	bool fail_next_write;
	pthread_mutex_t lock;
};

struct test_fixture {
	struct test_disk disk;
	struct ext4_blockdev_iface iface;
	struct ext4_blockdev bdev;
	struct ext4_bcache bcache;
};

struct reader_thread {
	struct test_fixture *fixture;
	struct start_gate *start;
	uint64_t lba;
	int result;
};

struct start_gate {
	pthread_mutex_t lock;
	pthread_cond_t cond;
	bool open;
};

struct writeback_pause {
	pthread_mutex_t lock;
	pthread_cond_t cond;
	bool entered;
	bool resume;
	struct ext4_buf *buf;
};

struct flush_thread {
	struct test_fixture *fixture;
	int result;
};

static int test_open(struct ext4_blockdev *bdev)
{
	(void)bdev;
	return EOK;
}

static int test_close(struct ext4_blockdev *bdev)
{
	(void)bdev;
	return EOK;
}

static struct test_disk *test_disk_from_bdev(struct ext4_blockdev *bdev)
{
	return bdev->bdif->p_user;
}

static int test_read(struct ext4_blockdev *bdev, void *buf, uint64_t block,
			     uint32_t count)
{
	struct test_disk *disk = test_disk_from_bdev(bdev);
	uint32_t delay;
	bool fail;

	if (block + count > TEST_BLOCK_COUNT)
		return EIO;
	pthread_mutex_lock(&disk->lock);
	fail = disk->fail_next_read;
	if (fail)
		disk->fail_next_read = false;
	for (uint32_t i = 0; i < count; ++i) {
		disk->read_attempts[block + i]++;
		if (!fail)
			disk->reads[block + i]++;
	}
	if (!fail)
		memcpy(buf, &disk->storage[block][0], count * TEST_BLOCK_SIZE);
	delay = disk->read_delay_yields;
	pthread_mutex_unlock(&disk->lock);

	while (delay--)
		sched_yield();
	return fail ? EIO : EOK;
}

static int test_write(struct ext4_blockdev *bdev, const void *buf,
			      uint64_t block, uint32_t count)
{
	struct test_disk *disk = test_disk_from_bdev(bdev);

	if (block + count > TEST_BLOCK_COUNT)
		return EIO;
	pthread_mutex_lock(&disk->lock);
	disk->write_requests++;
	if (disk->max_write_blocks < count)
		disk->max_write_blocks = count;
	for (uint32_t i = 0; i < count; ++i)
		disk->write_attempts[block + i]++;
	if (disk->fail_next_write) {
		disk->fail_next_write = false;
		pthread_mutex_unlock(&disk->lock);
		return EIO;
	}
	memcpy(&disk->storage[block][0], buf, count * TEST_BLOCK_SIZE);
	for (uint32_t i = 0; i < count; ++i)
		disk->writes[block + i]++;
	pthread_mutex_unlock(&disk->lock);
	return EOK;
}

static bool expect(int actual, int expected, const char *what)
{
	if (actual == expected)
		return true;
	fprintf(stderr, "%s: got %d, expected %d\n", what, actual, expected);
	return false;
}

static bool verify_cache(struct test_fixture *fixture, const char *where)
{
	int r = ext4_bcache_validate(&fixture->bcache);

	if (r == EOK)
		return true;
	fprintf(stderr, "bcache invariant failure after %s: %d\n", where, r);
	return false;
}

static bool fixture_init(struct test_fixture *fixture)
{
	memset(fixture, 0, sizeof(*fixture));
	for (uint32_t block = 0; block < TEST_BLOCK_COUNT; ++block)
		memset(fixture->disk.storage[block], (int)block, TEST_BLOCK_SIZE);
	if (pthread_mutex_init(&fixture->disk.lock, NULL))
		return false;

	fixture->iface.open = test_open;
	fixture->iface.close = test_close;
	fixture->iface.bread = test_read;
	fixture->iface.bwrite = test_write;
	fixture->iface.ph_bsize = TEST_BLOCK_SIZE;
	fixture->iface.ph_bcnt = TEST_BLOCK_COUNT;
	fixture->iface.p_user = &fixture->disk;
	fixture->bdev.bdif = &fixture->iface;
	fixture->bdev.part_size = TEST_BLOCK_SIZE * TEST_BLOCK_COUNT;
	if (!expect(ext4_block_init(&fixture->bdev), EOK, "block init") ||
	    !expect(ext4_bcache_init_dynamic(&fixture->bcache, 4,
					      TEST_BLOCK_SIZE), EOK, "bcache init"))
		return false;
	ext4_block_set_lb_size(&fixture->bdev, TEST_BLOCK_SIZE);
	if (!expect(ext4_block_bind_bcache(&fixture->bdev, &fixture->bcache), EOK,
		    "bcache bind"))
		return false;
	fixture->bdev.cache_write_back = 1;
	return verify_cache(fixture, "fixture init");
}

static void fixture_cleanup(struct test_fixture *fixture)
{
	ext4_bcache_cleanup(&fixture->bcache);
	ext4_bcache_fini_dynamic(&fixture->bcache);
	ext4_block_fini(&fixture->bdev);
	pthread_mutex_destroy(&fixture->disk.lock);
}

static bool get_block(struct test_fixture *fixture, uint64_t lba,
		      struct ext4_block *block, const char *where)
{
	*block = (struct ext4_block)EXT4_BLOCK_ZERO();
	return expect(ext4_block_get(&fixture->bdev, block, lba), EOK, where);
}

static bool put_block(struct test_fixture *fixture, struct ext4_block *block,
		      const char *where)
{
	return expect(ext4_block_set(&fixture->bdev, block), EOK, where);
}

static bool test_sequential_lifecycle(void)
{
	struct test_fixture fixture;
	bool ok = fixture_init(&fixture);

	for (uint64_t lba = 1; ok && lba <= 4; ++lba) {
		struct ext4_block block;
		ok = get_block(&fixture, lba, &block, "initial get");
		if (ok)
			memset(block.data, (int)(0xa0 + lba), TEST_BLOCK_SIZE);
		if (ok)
			ext4_bcache_set_dirty(block.buf);
		if (ok)
			ok = put_block(&fixture, &block, "initial dirty put");
		if (ok)
			ok = verify_cache(&fixture, "initial dirty put");
	}
	for (uint64_t lba = 1; ok && lba <= 4; ++lba) {
		ok = expect(ext4_block_flush_lba(&fixture.bdev, lba), EOK,
			    "dirty flush");
		if (ok)
			ok = fixture.disk.storage[lba][0] == (uint8_t)(0xa0 + lba);
		if (ok)
			ok = verify_cache(&fixture, "dirty flush");
	}
	for (uint64_t lba = 5; ok && lba <= 24; ++lba) {
		struct ext4_block block;
		ok = get_block(&fixture, lba, &block, "clean churn get");
		if (ok)
			ok = put_block(&fixture, &block, "clean churn put");
		if (ok)
			ok = verify_cache(&fixture, "clean eviction");
	}
	if (ok) {
		struct ext4_block block;

		fixture.disk.fail_next_read = true;
		block = (struct ext4_block)EXT4_BLOCK_ZERO();
		ok = expect(ext4_block_get(&fixture.bdev, &block, 31), EIO,
			    "read error");
		if (ok)
			ok = verify_cache(&fixture, "read error recovery");
		if (ok)
			ok = get_block(&fixture, 31, &block, "read retry");
		if (ok)
			ok = put_block(&fixture, &block, "read retry put");
		if (ok)
			ok = verify_cache(&fixture, "read retry");
	}
	if (ok) {
		struct ext4_block block;

		ok = get_block(&fixture, 32, &block, "write error get");
		if (ok)
			ext4_bcache_set_dirty(block.buf);
		if (ok)
			ok = put_block(&fixture, &block, "write error put");
		fixture.disk.fail_next_write = true;
		if (ok)
			ok = expect(ext4_block_flush_lba(&fixture.bdev, 32), EIO,
			    "write error");
		if (ok)
			ok = verify_cache(&fixture, "write error recovery");
		if (ok)
			ok = expect(ext4_block_flush_lba(&fixture.bdev, 32), EOK,
			    "write retry");
		if (ok)
			ok = verify_cache(&fixture, "write retry");
	}
	if (ok) {
		struct ext4_block block;
		struct ext4_block claimed = EXT4_BLOCK_ZERO();

		ok = get_block(&fixture, 33, &block, "dirty claim get");
		if (ok)
			ext4_bcache_set_dirty(block.buf);
		if (ok)
			ok = put_block(&fixture, &block, "dirty claim put");
		if (ok)
			ok = ext4_bcache_claim_dirty(&fixture.bcache, &claimed);
		if (ok)
			ok = claimed.lb_id == 33;
		if (ok)
			ok = verify_cache(&fixture, "dirty claim");
		if (claimed.buf)
			ext4_bcache_release_dirty(&fixture.bcache, &claimed);
		if (ok)
			ok = verify_cache(&fixture, "dirty claim release");
		if (ok)
			ok = expect(ext4_block_cache_flush(&fixture.bdev), EOK,
				    "dirty claim final flush");
	}
	fixture_cleanup(&fixture);
	return ok;
}

static bool test_contiguous_cache_flush_coalescing(void)
{
	struct test_fixture fixture;
	bool ok = fixture_init(&fixture);
	uint32_t requests_before;

	for (uint64_t lba = 100; ok && lba < 104; ++lba) {
		struct ext4_block block;

		ok = get_block(&fixture, lba, &block, "coalesced flush get");
		if (ok)
			memset(block.data, (int)(0x80 + lba), TEST_BLOCK_SIZE);
		if (ok)
			ext4_bcache_set_dirty(block.buf);
		if (ok)
			ok = put_block(&fixture, &block, "coalesced flush put");
	}
	requests_before = fixture.disk.write_requests;
	if (ok)
		ok = expect(ext4_block_cache_flush(&fixture.bdev), EOK,
			    "coalesced cache flush");
	if (ok)
		ok = fixture.disk.write_requests == requests_before + 1 &&
		     fixture.disk.max_write_blocks >= 4;
	for (uint64_t lba = 100; ok && lba < 104; ++lba)
		ok = fixture.disk.storage[lba][0] == (uint8_t)(0x80 + lba);
	if (ok)
		ok = verify_cache(&fixture, "coalesced cache flush");

	for (uint64_t lba = 100; ok && lba < 104; ++lba) {
		struct ext4_block block;

		ok = get_block(&fixture, lba, &block, "coalesced retry get");
		if (ok)
			memset(block.data, (int)(0x40 + lba), TEST_BLOCK_SIZE);
		if (ok)
			ext4_bcache_set_dirty(block.buf);
		if (ok)
			ok = put_block(&fixture, &block, "coalesced retry put");
	}
	requests_before = fixture.disk.write_requests;
	fixture.disk.fail_next_write = true;
	if (ok)
		ok = expect(ext4_block_cache_flush(&fixture.bdev), EIO,
			    "coalesced cache flush error");
	if (ok)
		ok = fixture.disk.write_requests == requests_before + 1;
	for (uint64_t lba = 100; ok && lba < 104; ++lba)
		ok = fixture.disk.storage[lba][0] == (uint8_t)(0x80 + lba);
	if (ok)
		ok = verify_cache(&fixture, "coalesced cache flush error");
	if (ok)
		ok = expect(ext4_block_cache_flush(&fixture.bdev), EOK,
			    "coalesced cache flush retry");
	if (ok)
		ok = fixture.disk.write_requests == requests_before + 2;
	for (uint64_t lba = 100; ok && lba < 104; ++lba)
		ok = fixture.disk.storage[lba][0] == (uint8_t)(0x40 + lba);
	if (ok)
		ok = verify_cache(&fixture, "coalesced cache flush retry");

	fixture_cleanup(&fixture);
	return ok;
}

static uint32_t next_random(uint32_t *state)
{
	*state = *state * 1664525U + 1013904223U;
	return *state;
}

static bool start_gate_init(struct start_gate *gate)
{
	memset(gate, 0, sizeof(*gate));
	if (pthread_mutex_init(&gate->lock, NULL))
		return false;
	if (pthread_cond_init(&gate->cond, NULL)) {
		pthread_mutex_destroy(&gate->lock);
		return false;
	}
	return true;
}

static void start_gate_wait(struct start_gate *gate)
{
	pthread_mutex_lock(&gate->lock);
	while (!gate->open)
		pthread_cond_wait(&gate->cond, &gate->lock);
	pthread_mutex_unlock(&gate->lock);
}

static void start_gate_open(struct start_gate *gate)
{
	pthread_mutex_lock(&gate->lock);
	gate->open = true;
	pthread_cond_broadcast(&gate->cond);
	pthread_mutex_unlock(&gate->lock);
}

static void start_gate_destroy(struct start_gate *gate)
{
	pthread_cond_destroy(&gate->cond);
	pthread_mutex_destroy(&gate->lock);
}

static bool test_random_lifecycle(void)
{
	struct test_fixture fixture;
	struct ext4_block held[TEST_HELD_BLOCKS] = { 0 };
	bool occupied[TEST_HELD_BLOCKS] = { false };
	uint32_t random = 0x5eed1234U;
	bool ok = fixture_init(&fixture);

	for (uint32_t step = 0; ok && step < TEST_RANDOM_STEPS; ++step) {
		uint32_t value = next_random(&random);
		uint32_t slot = value % TEST_HELD_BLOCKS;
		uint64_t lba = 1 + ((value >> 8) % (TEST_BLOCK_COUNT - 1));

		if ((value & 3U) == 0 && !occupied[slot]) {
			ok = get_block(&fixture, lba, &held[slot], "random get");
			occupied[slot] = ok;
		} else if ((value & 3U) == 1 && occupied[slot]) {
			if (value & 4U)
				ext4_bcache_set_dirty(held[slot].buf);
			ok = put_block(&fixture, &held[slot], "random put");
			occupied[slot] = false;
		} else {
			ok = expect(ext4_block_flush_lba(&fixture.bdev, lba), EOK,
			    "random flush");
		}
		if (ok)
			ok = verify_cache(&fixture, "random lifecycle");
	}
	for (uint32_t slot = 0; ok && slot < TEST_HELD_BLOCKS; ++slot) {
		if (!occupied[slot])
			continue;
		if (next_random(&random) & 1U)
			ext4_bcache_set_dirty(held[slot].buf);
		ok = put_block(&fixture, &held[slot], "random final put");
		occupied[slot] = false;
	}
	if (ok)
		ok = expect(ext4_block_cache_flush(&fixture.bdev), EOK,
			    "random final flush");
	if (ok)
		ok = verify_cache(&fixture, "random final flush");
	fixture_cleanup(&fixture);
	return ok;
}

static void *reader_main(void *arg)
{
	struct reader_thread *thread = arg;
	struct ext4_block block = EXT4_BLOCK_ZERO();

	start_gate_wait(thread->start);
	thread->result = ext4_block_get(&thread->fixture->bdev, &block,
					 thread->lba);
	if (thread->result == EOK) {
		if (block.data[0] != (uint8_t)thread->lba)
			thread->result = EIO;
		else
			thread->result = ext4_block_set(&thread->fixture->bdev, &block);
	}
	return NULL;
}

static bool run_readers(struct test_fixture *fixture, uint64_t first_lba,
			bool same_lba, int expected_result)
{
	pthread_t threads[TEST_THREADS];
	struct reader_thread readers[TEST_THREADS];
	struct start_gate start;
	uint32_t created = 0;
	bool ok = start_gate_init(&start);

	for (uint32_t i = 0; ok && i < TEST_THREADS; ++i) {
		readers[i].fixture = fixture;
		readers[i].start = &start;
		readers[i].lba = same_lba ? first_lba : first_lba + i;
		readers[i].result = EIO;
		if (pthread_create(&threads[i], NULL, reader_main, &readers[i]))
			ok = false;
		else
			created++;
	}
	start_gate_open(&start);
	for (uint32_t i = 0; i < created; ++i) {
		pthread_join(threads[i], NULL);
		if (readers[i].result != expected_result)
			ok = false;
	}
	if (created != TEST_THREADS)
		ok = false;
	start_gate_destroy(&start);
	return ok;
}

static bool test_concurrent_loads(void)
{
	struct test_fixture fixture;
	bool ok = fixture_init(&fixture);

	fixture.disk.read_delay_yields = 5000;
	if (ok)
		ok = run_readers(&fixture, 40, true, EOK);
	if (ok)
		ok = fixture.disk.read_attempts[40] == 1 &&
			fixture.disk.reads[40] == 1;
	if (ok)
		ok = verify_cache(&fixture, "same-lba concurrent load");
	if (ok) {
		fixture.disk.fail_next_read = true;
		ok = run_readers(&fixture, 41, true, EIO);
	}
	if (ok)
		ok = fixture.disk.read_attempts[41] == 1 &&
			fixture.disk.reads[41] == 0;
	if (ok)
		ok = verify_cache(&fixture, "same-lba load error");
	if (ok) {
		struct ext4_block block;

		ok = get_block(&fixture, 41, &block, "load error retry");
		if (ok)
			ok = put_block(&fixture, &block, "load error retry put");
	}
	if (ok)
		ok = fixture.disk.read_attempts[41] == 2 &&
			fixture.disk.reads[41] == 1;
	if (ok)
		ok = verify_cache(&fixture, "load error retry");
	if (ok)
		ok = run_readers(&fixture, 48, false, EOK);
	for (uint32_t i = 0; ok && i < TEST_THREADS; ++i)
		ok = fixture.disk.reads[48 + i] == 1;
	if (ok)
		ok = verify_cache(&fixture, "different-lba concurrent load");
	fixture_cleanup(&fixture);
	return ok;
}

static void pause_end_write(struct ext4_bcache *bc, struct ext4_buf *buf,
			    int result, void *arg)
{
	struct writeback_pause *pause = arg;

	(void)bc;
	(void)result;
	pthread_mutex_lock(&pause->lock);
	pause->buf = buf;
	pause->entered = true;
	pthread_cond_broadcast(&pause->cond);
	while (!pause->resume)
		pthread_cond_wait(&pause->cond, &pause->lock);
	pthread_mutex_unlock(&pause->lock);
}

static void *flush_main(void *arg)
{
	struct flush_thread *thread = arg;

	thread->result = ext4_block_cache_flush(&thread->fixture->bdev);
	return NULL;
}

static bool test_cache_flush_ownership(void)
{
	struct test_fixture fixture;
	struct writeback_pause pause;
	struct flush_thread flusher;
	pthread_t thread;
	struct ext4_block block;
	bool thread_started = false;
	bool pause_initialized = false;
	bool ok = fixture_init(&fixture);

	memset(&pause, 0, sizeof(pause));
	if (ok && !pthread_mutex_init(&pause.lock, NULL)) {
		if (!pthread_cond_init(&pause.cond, NULL))
			pause_initialized = true;
		else
			pthread_mutex_destroy(&pause.lock);
	}
	if (ok && !pause_initialized)
		ok = false;
	if (ok)
		ok = get_block(&fixture, 40, &block, "writeback pin get");
	if (ok) {
		ext4_bcache_set_dirty(block.buf);
		block.buf->end_write = pause_end_write;
		block.buf->end_write_arg = &pause;
		ok = put_block(&fixture, &block, "writeback pin put");
	}
	flusher.fixture = &fixture;
	flusher.result = EIO;
	if (ok && !pthread_create(&thread, NULL, flush_main, &flusher))
		thread_started = true;
	else if (ok)
		ok = false;
	if (thread_started) {
		pthread_mutex_lock(&pause.lock);
		while (!pause.entered)
			pthread_cond_wait(&pause.cond, &pause.lock);
		pthread_mutex_unlock(&pause.lock);
	}
	if (ok)
		ok = verify_cache(&fixture, "writeback callback");
	for (uint64_t lba = 1; ok && lba <= 24; ++lba) {
		ok = get_block(&fixture, lba, &block, "writeback churn get");
		if (ok)
			ok = put_block(&fixture, &block, "writeback churn put");
	}
	if (ok)
		ok = get_block(&fixture, 40, &block, "writeback pinned lookup");
	if (ok)
		ok = block.buf == pause.buf && fixture.disk.reads[40] == 1;
	if (ok)
		ok = put_block(&fixture, &block, "writeback pinned put");
	if (pause_initialized) {
		pthread_mutex_lock(&pause.lock);
		pause.resume = true;
		pthread_cond_broadcast(&pause.cond);
		pthread_mutex_unlock(&pause.lock);
	}
	if (thread_started) {
		pthread_join(thread, NULL);
		if (flusher.result != EOK)
			ok = false;
	}
	if (ok)
		ok = verify_cache(&fixture, "writeback completion");

	if (ok)
		ok = get_block(&fixture, 41, &block, "cache flush error get");
	if (ok) {
		ext4_bcache_set_dirty(block.buf);
		ok = put_block(&fixture, &block, "cache flush error put");
	}
	fixture.disk.fail_next_write = true;
	if (ok)
		ok = expect(ext4_block_cache_flush(&fixture.bdev), EIO,
			    "cache flush write error");
	if (ok)
		ok = fixture.disk.write_attempts[41] == 1 &&
			fixture.disk.writes[41] == 0;
	if (ok)
		ok = verify_cache(&fixture, "cache flush write error");
	if (ok)
		ok = expect(ext4_block_cache_flush(&fixture.bdev), EOK,
			    "cache flush write retry");
	if (ok)
		ok = fixture.disk.write_attempts[41] == 2 &&
			fixture.disk.writes[41] == 1;
	if (ok)
		ok = verify_cache(&fixture, "cache flush write retry");

	if (pause_initialized) {
		pthread_cond_destroy(&pause.cond);
		pthread_mutex_destroy(&pause.lock);
	}
	fixture_cleanup(&fixture);
	return ok;
}

#if CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_EXPERIMENT
static bool test_dirty_capacity_watermarks(void)
{
	struct test_fixture fixture;
	struct ext4_block block = EXT4_BLOCK_ZERO();
	bool ok = fixture_init(&fixture);

	for (uint64_t lba = 1;
	     ok && lba <= CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK;
	     ++lba) {
		ok = get_block(&fixture, lba, &block, "capacity fill get");
		if (ok) {
			block.data[0] = (uint8_t)(lba ^ 0xa5U);
			ext4_bcache_set_dirty(block.buf);
			ok = put_block(&fixture, &block, "capacity fill put");
		}
	}
	if (ok)
		ok = fixture.bcache.ref_blocks ==
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK;
	if (ok)
		ok = verify_cache(&fixture, "capacity high watermark");

	/* The first capacity writeback must leave the victim dirty on EIO. */
	fixture.disk.fail_next_write = true;
	block = (struct ext4_block)EXT4_BLOCK_ZERO();
	if (ok)
		ok = expect(ext4_block_get(&fixture.bdev, &block,
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK + 1U), EIO,
			"capacity writeback error");
	if (ok)
		ok = fixture.disk.write_attempts[
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK] == 1 &&
			fixture.disk.writes[
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK] == 0;
	if (ok)
		ok = fixture.bcache.ref_blocks ==
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK;
	if (ok)
		ok = verify_cache(&fixture, "capacity writeback error");

	/* The next request retries that victim, drops clean buffers to low, then
	 * allocates the requested block. */
	block = (struct ext4_block)EXT4_BLOCK_ZERO();
	if (ok)
		ok = get_block(&fixture,
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK + 1U,
			&block, "capacity retry get");
	if (ok) {
		block.data[0] = (uint8_t)(
			(CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK + 1U) ^ 0xa5U);
		ext4_bcache_set_dirty(block.buf);
		ok = put_block(&fixture, &block, "capacity retry put");
	}
	if (ok)
		ok = fixture.disk.write_attempts[
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK] == 2 &&
			fixture.disk.writes[
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK] == 1;
	if (ok)
		ok = fixture.bcache.ref_blocks <=
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_LOW_WATERMARK + 1U &&
			fixture.bcache.max_ref_blocks <=
			CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK;
	if (ok)
		ok = verify_cache(&fixture, "capacity retry");
	if (ok)
		ok = expect(ext4_block_cache_flush(&fixture.bdev), EOK,
			"capacity final flush");
	for (uint64_t lba = 1;
	     ok && lba <= CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_HIGH_WATERMARK + 1U;
	     ++lba)
		ok = fixture.disk.storage[lba][0] == (uint8_t)(lba ^ 0xa5U);
	if (ok)
		ok = verify_cache(&fixture, "capacity final flush");
	fixture_cleanup(&fixture);
	return ok;
}
#endif

static bool test_oracle_negative_controls(void)
{
	struct test_fixture fixture;
	struct ext4_buf *dirty_buf = NULL;
	struct ext4_buf *root;
	struct ext4_block block;
	bool ok = fixture_init(&fixture);

	for (uint64_t lba = 1; ok && lba <= 8; ++lba) {
		ok = get_block(&fixture, lba, &block, "oracle setup get");
		if (ok)
			ok = put_block(&fixture, &block, "oracle setup put");
	}
	root = RB_ROOT(&fixture.bcache.lba_root);
	if (ok && root) {
		RB_COLOR(root, lba_node) = RB_RED;
		ok = expect(ext4_bcache_validate(&fixture.bcache), EIO,
			    "oracle red root");
		RB_COLOR(root, lba_node) = RB_BLACK;
	}
	if (ok) {
		fixture.bcache.ref_blocks++;
		ok = expect(ext4_bcache_validate(&fixture.bcache), EIO,
			    "oracle ref_blocks mismatch");
		fixture.bcache.ref_blocks--;
	}
	if (ok)
		ok = get_block(&fixture, 20, &block, "oracle dirty get");
	if (ok) {
		dirty_buf = block.buf;
		ext4_bcache_set_dirty(dirty_buf);
		ok = put_block(&fixture, &block, "oracle dirty put");
	}
	if (ok) {
		dirty_buf->on_dirty_list = false;
		ok = expect(ext4_bcache_validate(&fixture.bcache), EIO,
			    "oracle dirty membership");
		dirty_buf->on_dirty_list = true;
	}
	if (ok)
		ok = verify_cache(&fixture, "oracle negative controls restored");
	fixture_cleanup(&fixture);
	return ok;
}

static bool test_retained_reference(void)
{
	struct test_fixture fixture;
	struct ext4_block owner = EXT4_BLOCK_ZERO();
	struct ext4_block retained;
	bool ok = fixture_init(&fixture);

	if (ok)
		ok = get_block(&fixture, 63, &owner, "retain setup get");
	if (ok) {
		retained = owner;
		ok = expect(ext4_bcache_retain(&fixture.bcache, owner.buf), EOK,
				"retain additional reference");
	}
	if (ok)
		ok = put_block(&fixture, &owner, "retain owner put");
	if (ok)
		ok = verify_cache(&fixture, "retain owner released");
	if (ok)
		ok = put_block(&fixture, &retained, "retain checkpoint put");
	if (ok)
		ok = verify_cache(&fixture, "retain checkpoint released");
	fixture_cleanup(&fixture);
	return ok;
}

int main(void)
{
	if (!test_sequential_lifecycle() || !test_contiguous_cache_flush_coalescing() ||
	    !test_random_lifecycle() ||
	    !test_concurrent_loads() || !test_cache_flush_ownership() ||
	#if CONFIG_EXT4_BCACHE_DIRTY_CAPACITY_EXPERIMENT
	    !test_dirty_capacity_watermarks() ||
	#endif
	    !test_oracle_negative_controls() || !test_retained_reference())
		return 1;
	puts("lwext4-bcache-lifecycle: PASS");
	return 0;
}
