//! The migrations are embedded at compile time (`sqlx::migrate!`): a changed `.sql` file must
//! rebuild the crate, or a test binary would carry the previous schema.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
