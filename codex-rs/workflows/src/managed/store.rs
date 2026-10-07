mod fs;

#[cfg(all(test, unix))]
#[path = "store_tests.rs"]
mod tests;
