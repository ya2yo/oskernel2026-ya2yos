use super::Case;

pub static CASE: Case = Case::new(
    "fs-readwrite",
    "Create test_input.txt with numbers 1 to 5, then read it and sum the numbers",
    "grep -qE '15|fifteen'",
    30,
);
