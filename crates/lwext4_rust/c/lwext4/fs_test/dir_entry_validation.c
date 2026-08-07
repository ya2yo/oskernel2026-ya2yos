#include <ext4_dir.h>
#include <ext4_errno.h>
#include <ext4_fs.h>

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define TEST_BLOCK_SIZE 1024U

static int expect_corrupt_entry_rejected(uint16_t entry_len, uint8_t name_len,
					 const char *what)
{
	uint8_t data[TEST_BLOCK_SIZE];
	struct ext4_sblock sb;
	struct ext4_inode_ref parent;
	struct ext4_inode_ref child;
	struct ext4_block block = EXT4_BLOCK_ZERO();
	struct ext4_dir_en *entry = (struct ext4_dir_en *)data;

	memset(data, 0, sizeof(data));
	memset(&sb, 0, sizeof(sb));
	memset(&parent, 0, sizeof(parent));
	memset(&child, 0, sizeof(child));
	entry->entry_len = to_le16(entry_len);
	entry->name_len = name_len;
	block.data = data;

	int r = ext4_dir_try_insert_entry(&sb, &parent, &block, &child,
					   "maps", 4);
	if (r == EIO)
		return 0;

	fprintf(stderr, "%s: got %d, expected %d\n", what, r, EIO);
	return 1;
}

int main(void)
{
	int failed = 0;

	failed += expect_corrupt_entry_rejected(0, 0, "zero rec_len");
	failed += expect_corrupt_entry_rejected(6, 0, "short rec_len");
	failed += expect_corrupt_entry_rejected(10, 0, "unaligned rec_len");
	failed += expect_corrupt_entry_rejected(TEST_BLOCK_SIZE + 4, 0,
						 "out-of-block rec_len");
	failed += expect_corrupt_entry_rejected(8, 1, "oversized name_len");

	if (failed)
		return 1;
	puts("lwext4-dir-entry-validation: PASS");
	return 0;
}
