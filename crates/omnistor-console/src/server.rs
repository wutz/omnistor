//! 纯 std 的最小 HTTP/1.1 服务器：够控台使用（单机、短连接）。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use crate::{Console, Response};

/// 在 addr 上启动阻塞服务；每连接一个线程。
pub fn serve(console: Console, addr: &str) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    let console = Arc::new(console);
    eprintln!(
        "omnistor-console listening on http://{}",
        listener.local_addr()?
    );
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let console = Arc::clone(&console);
        std::thread::spawn(move || {
            let _ = handle_conn(&console, stream);
        });
    }
    Ok(())
}

fn handle_conn(console: &Console, mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Ok(());
    };
    let (method, path) = (method.to_string(), path.to_string());

    // 读头部，取 Content-Length
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    // 读 body（上限 1 MiB，防滥用）
    let mut body = vec![0u8; content_length.min(1 << 20)];
    if !body.is_empty() {
        reader.read_exact(&mut body)?;
    }
    let body = String::from_utf8_lossy(&body).into_owned();

    let resp = console.handle(&method, &path, &body);
    write_response(&mut stream, &resp)
}

fn write_response(stream: &mut TcpStream, resp: &Response) -> std::io::Result<()> {
    let reason = match resp.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        429 => "Too Many Requests",
        507 => "Insufficient Storage",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        resp.status,
        reason,
        resp.content_type,
        resp.body.len(),
        resp.body
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 起真实 TCP 服务，用原生 socket 走一次完整 HTTP 往返。
    #[test]
    fn http_roundtrip_over_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let console = Arc::new(Console::with_demo_data());
        let c2 = Arc::clone(&console);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let c = Arc::clone(&c2);
                std::thread::spawn(move || {
                    let _ = handle_conn(&c, stream);
                });
            }
        });

        // GET /api/cluster
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "GET /api/cluster HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        assert!(buf.starts_with("HTTP/1.1 200 OK"), "{buf}");
        assert!(buf.contains("\"tenants\":2"), "{buf}");

        // POST 带 body：写入模拟
        let body = r#"{"count": 3, "size_bytes": 100}"#;
        let mut s = TcpStream::connect(addr).unwrap();
        write!(
            s,
            "POST /api/tenants/1/objects HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        assert!(buf.contains("\"ok\":3"), "{buf}");
    }
}
