fn main() {
    // The engine allocates and zeroes the whole initial memory of a dex on every call,
    // and the default 1 MiB stack is most of it.
    println!("cargo:rustc-link-arg=-zstack-size=65536");
}
