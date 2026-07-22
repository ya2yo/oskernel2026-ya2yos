use super::Case;

pub static CASE: Case = Case::new(
    "network",
    "Count ESTABLISHED TCP connections",
    "grep -qE '[0-9]+'",
    25,
);
