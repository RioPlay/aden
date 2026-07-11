
pub fn helper_fn(word: &str) -> String {
    format!("{word}!")
}

pub fn caller_fn() -> String {
    helper_fn("hello")
}

pub fn dupe_name() -> u8 { 1 }

pub fn shr() -> u8 { 2 }
