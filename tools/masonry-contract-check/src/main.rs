mod checks;
mod fixture;
#[cfg(feature = "experimental")]
mod presentation;
fn main() {
    checks::run();
}
