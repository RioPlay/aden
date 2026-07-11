
pub fn helper_fn(word: &str) -> String {
    format!("{word}!")
}

pub fn caller_fn() -> String {
    helper_fn("hello")
}
