use super::Case;

pub static CASE: Case = Case::new(
    "date",
    "What day was it 100 days ago?",
    "grep -qE '(Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday)'",
    20,
);
