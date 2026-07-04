#![warn(uncommented_anonymous_literal_argument)]

struct Options;

impl Options {
    fn enabled(self, enabled: bool, retry_count: usize) -> Self {
        let _ = (enabled, retry_count);
        self
    }
}

fn create_openai_url(base_url: Option<String>, retry_count: usize) -> String {
    let _ = (base_url, retry_count);
    String::new()
}

fn main() {
    let _ = create_openai_url(None, 3);
    let _ = Options.enabled(false, /*retry_count*/ 3);
}
