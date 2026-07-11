
pub trait Greeter {
    fn greet(&self) -> String;
}

pub struct English;

impl Greeter for English {
    fn greet(&self) -> String {
        make_greeting("hello")
    }
}

pub struct French;

impl Greeter for French {
    fn greet(&self) -> String {
        make_greeting("bonjour")
    }
}

pub fn greet_all(g: &dyn Greeter) -> String {
    g.greet()
}

pub fn make_greeting(word: &str) -> String {
    format!("{word}!")
}
