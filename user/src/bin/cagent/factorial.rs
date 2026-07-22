use super::Case;

pub static CASE: Case = Case::new(
    "factorial",
    "Calculate factorial of 10 using bash",
    "grep -q '3628800'",
    20,
);
