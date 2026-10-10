//! 连接编辑器「测试连接 / 检查端口 / 检查串口」。
//!
//! - 主机连接（TcpClient）：真实发起一次 TCP 连接，验证远端可达性并测往返时延；
//! - 从机连接（TcpServer / Serial）：不测试“连接”，只检查本地端口是否被占用
//!   （能否完成一次 bind / 串口 open），供启动前预判。

use std::time::{Duration, Instant};

use tokio::net::{TcpListener, TcpStream};

use crate::model::Transport;

/// 检测结果（ok=false 时 message 给出原因）
#[derive(Debug, Clone, serde::Serialize)]
pub struct TestConnResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: Option<u64>,
}

impl TestConnResult {
    fn ok(message: String) -> Self {
        Self { ok: true, message, latency_ms: None }
    }

    fn fail(message: String) -> Self {
        Self { ok: false, message, latency_ms: None }
    }
}

/// 按传输方式执行对应检测
pub async fn test_transport(t: &Transport) -> TestConnResult {
    match t {
        Transport::TcpClient { host, port, timeout_ms } => {
            test_tcp_client(host, *port, *timeout_ms).await
        }
        Transport::TcpServer { listen_addr, port } => test_tcp_server(listen_addr, *port).await,
        // 主机/从机串口共用占用检测：能否打开一次
        Transport::Serial { serial_port, baud, .. } | Transport::SerialMaster { serial_port, baud, .. } => {
            test_serial(serial_port, *baud)
        }
        // slcan 后端检测串口占用；zlgcan/candle 未实现直接给出说明
        Transport::UsbCan { backend, serial_port, serial_baud, .. } => {
            if backend != "slcan" {
                return TestConnResult::fail(format!(
                    "CAN 后端「{backend}」尚未实现（zlgcan/candle 待硬件到货，当前可用：slcan）"
                ));
            }
            match serial_port {
                Some(name) if !name.is_empty() => test_serial(name, *serial_baud),
                _ => TestConnResult::fail("slcan 后端需要先配置串口".to_string()),
            }
        }
        // CANET 网关检测与 Modbus TCP 主机一致：真实连一次远端
        Transport::CanNet { host, port, timeout_ms, .. } => test_tcp_client(host, *port, *timeout_ms).await,
    }
}

/// 主机连接：尝试连一次远端，带配置的超时（限制在 200ms..5s 内防呆）
async fn test_tcp_client(host: &str, port: u16, timeout_ms: u32) -> TestConnResult {
    let timeout = Duration::from_millis(timeout_ms.clamp(200, 5000) as u64);
    let t0 = Instant::now();
    match tokio::time::timeout(timeout, TcpStream::connect((host, port))).await {
        Ok(Ok(_)) => TestConnResult {
            ok: true,
            message: format!("连接成功：{host}:{port} 可达"),
            latency_ms: Some(t0.elapsed().as_millis() as u64),
        },
        Ok(Err(e)) => TestConnResult::fail(format!("连接失败：{host}:{port} → {e}")),
        Err(_) => TestConnResult::fail(format!(
            "连接超时：{}ms 内未能建立连接（远端未监听或网络不通）",
            timeout.as_millis()
        )),
    }
}

/// 从机 TCP：试绑定一次监听端口，bind 失败即端口被占用/不可用
async fn test_tcp_server(listen_addr: &str, port: u16) -> TestConnResult {
    match TcpListener::bind((listen_addr, port)).await {
        Ok(_) => TestConnResult::ok(format!("端口可用：{listen_addr}:{port} 未被占用，可监听")),
        Err(e) => {
            let reason = if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("端口 {port} 已被占用（其他程序或本应用正在监听）")
            } else {
                format!("无法绑定 {listen_addr}:{port}：{e}")
            };
            TestConnResult::fail(reason)
        }
    }
}

/// 从机串口：试打开一次串口，open 失败即被占用/不存在
fn test_serial(name: &str, baud: u32) -> TestConnResult {
    match serialport::new(name, baud).open() {
        Ok(_) => TestConnResult::ok(format!("串口可用：{name} 可打开（未被占用）")),
        Err(e) => TestConnResult::fail(format!("串口不可用：{name} → {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_free_port_ok() {
        // 让 OS 分配一个空闲端口后立即释放，再对该端口做占用检测
        let port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            l.local_addr().unwrap().port()
        };
        let r = test_tcp_server("127.0.0.1", port).await;
        assert!(r.ok, "{}", r.message);
    }

    #[tokio::test]
    async fn bind_occupied_port_fail() {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let r = test_tcp_server("127.0.0.1", port).await;
        assert!(!r.ok);
        assert!(r.message.contains("占用"), "{}", r.message);
    }

    #[tokio::test]
    async fn connect_unreachable_timeout() {
        // 203.0.113.x 为文档保留网段，不可路由
        let r = test_tcp_client("203.0.113.1", 9999, 200).await;
        assert!(!r.ok);
    }

    #[tokio::test]
    async fn connect_local_listener_ok() {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let r = test_tcp_client("127.0.0.1", port, 1000).await;
        assert!(r.ok, "{}", r.message);
        assert!(r.latency_ms.is_some());
    }
}
