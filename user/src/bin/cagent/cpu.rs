use super::Case;

pub static CASE: Case = Case::new("cpu", "How many CPU cores?", "grep -qE '[0-9]+'", 20);
