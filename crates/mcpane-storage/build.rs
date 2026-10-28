// `sqlx::migrate!("./migrations")` 在宏展开时把目录内容内联进二进制，但只增删
// .sql 文件、不动本 crate 的 .rs 代码时，cargo 的增量编译不会感知到目录变化——
// 编出来的产物可能悄悄嵌入一份过期的迁移集合（本地已跑过的库反而比它「新」，
// 启动时报 `migration N was previously applied but is missing in the resolved
// migrations`）。显式声明依赖，任何迁移文件增删都强制重新编译本 crate。
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
