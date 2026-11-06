fn main() {
    println!("cargo:rerun-if-changed=../loop-analysis/src/deck_loops.cpp");
    println!("cargo:rerun-if-changed=../loop-analysis/include/deck_loops.h");
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .define("DECK_LOOPS_BUILD", None)
        .include("../loop-analysis/include")
        .file("../loop-analysis/src/deck_loops.cpp")
        .compile("deck_loop_kernel");
}
