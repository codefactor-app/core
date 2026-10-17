trait Repository { fn query(&self, sql: &str) -> String; }
struct Store { url: String }
impl Repository for Store {
    fn query(&self, sql: &str) -> String { sql.to_string() }
}
impl Store {
    fn new(url: String) -> Self { Self { url } }
    fn run(&self, items: Vec<String>) {
        for item in items {
            if !item.is_empty() { self.query(&item); }
            else { std::process::Command::new(item); }
        }
    }
}
fn factory() -> Store { Store::new(String::new()) }
