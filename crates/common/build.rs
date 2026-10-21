fn main() {
    // include_dir 在稳定版 Rust 上不会跟踪被嵌入的文件，迁移文件变化后要手动触发重新编译。
    println!("cargo:rerun-if-changed=../../migrations");
}
