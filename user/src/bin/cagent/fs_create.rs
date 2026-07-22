use super::Case;

pub static CASE: Case = Case::new(
    "fs-create",
    "Create a file named test_file.txt with content 'Hello OS'",
    "test -f test_file.txt && grep -q 'Hello OS' test_file.txt",
    25,
);
