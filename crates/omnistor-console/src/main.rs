//! omnistor-console 可执行入口。
//!
//! 用法：omnistor-console [addr]（默认 127.0.0.1:8090），
//! 以演示数据启动（4 个池 + 2 个租户）。

use omnistor_console::{server, Console};

fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8090".into());
    server::serve(Console::with_demo_data(), &addr)
}
