use super::Case;

pub static CASE: Case = Case::new(
    "fs-search",
    "Find all .sh files in current directory and count them",
    "grep -qE '[0-9]+'",
    35,
);
