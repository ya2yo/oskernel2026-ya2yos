use super::Case;

pub static CASE: Case = Case::new(
    "fs-usage",
    "Check disk usage of current directory in human readable format",
    "grep -qE '[0-9]+[KMG]?'",
    25,
);
