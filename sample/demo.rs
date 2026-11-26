// 代码视图演示:注释、字符串、数字、关键字应分别着色
use std::path::Path;

fn main() {
    let path = "/tmp/big.log";
    let n = 42usize;
    println!("open {path} with {n} lines"); // 行内注释
    if Path::new(path).exists() {
        return;
    }
}
