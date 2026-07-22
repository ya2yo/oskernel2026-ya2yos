use super::Case;

pub static CASE: Case = Case::new(
    "fs-directory",
    "Create directory test_dir, create 3 files inside it, then count the files",
    "test -d test_dir && [ $(ls test_dir | wc -l) -ge 3 ]",
    30,
);
