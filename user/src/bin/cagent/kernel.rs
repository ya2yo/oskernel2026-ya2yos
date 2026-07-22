use super::Case;

pub static CASE: Case = Case::new(
    "kernel",
    "What is the kernel version?",
    "grep -qE '[0-9]+\\.[0-9]+'",
    20,
);
