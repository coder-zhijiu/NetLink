use std::time::Duration;

use netlink_core::model::{Connection, ProjectSpec, Transport};
use netlink_core::runtime::{ConnStateKind, RuntimeManager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn connection(port: u16) -> Connection {
    serde_json::from_value(serde_json::json!({
        "id": 10, "name": "模拟网关",
        "transport": { "kind": "dlt645_tcp", "host": "127.0.0.1", "port": port, "timeout_ms": 150 },
        "dlt645_meters": [
            { "id": 20, "name": "表一", "address": "000000000001", "poll": false, "poll_period_ms": 200,
              "points": [
                { "id": 101, "name": "电压", "di": "02010100", "format": "bcd", "bytes": 2, "decimals": 1, "scale": 2.0 },
                { "id": 102, "name": "功率", "di": "02030000", "format": "signed_bcd", "bytes": 3, "decimals": 4, "poll": false },
                { "id": 103, "name": "不支持", "di": "01020304", "format": "unsigned", "bytes": 2, "decimals": 0, "poll": false }
              ] },
            { "id": 21, "name": "表二", "address": "000000000002", "poll": false, "poll_period_ms": 200,
              "points": [{ "id": 201, "name": "电压", "di": "02010100", "format": "bcd", "bytes": 2, "decimals": 1 }] }
        ]
    })).unwrap()
}

fn response(address: u8, code: u8, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x68, address, 0, 0, 0, 0, 0, 0x68, code, data.len() as u8];
    bytes.extend(data.iter().map(|b| b.wrapping_add(0x33)));
    bytes.push(bytes.iter().copied().fold(0, u8::wrapping_add));
    bytes.push(0x16);
    bytes
}

async fn request(socket: &mut TcpStream) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut header = [0; 14];
        socket.read_exact(&mut header).await.unwrap();
        assert_eq!(&header[..5], &[0xfe, 0xfe, 0xfe, 0xfe, 0x68]);
        assert_eq!(header[11], 0x68);
        let mut rest = vec![0; header[13] as usize + 2];
        socket.read_exact(&mut rest).await.unwrap();
        let frame = [&header[4..], &rest].concat();
        assert_eq!(frame[frame.len() - 1], 0x16);
        assert_eq!(
            frame[..frame.len() - 2]
                .iter()
                .copied()
                .fold(0, u8::wrapping_add),
            frame[frame.len() - 2]
        );
        frame
    })
    .await
    .expect("request timed out")
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("runtime state timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_gateway_two_meters_and_recovery() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let conn = connection(listener.local_addr().unwrap().port());
    let runtime = RuntimeManager::new();
    let mut frames = runtime.subscribe_frames();
    runtime.start_connection(&conn).await.unwrap();
    let (mut socket, _) = listener.accept().await.unwrap();
    let state_is = |kind| {
        runtime
            .snapshot_conn_states()
            .iter()
            .any(|(id, state, _)| *id == 10 && *state == kind)
    };
    wait_until(|| state_is(ConnStateKind::Connected)).await;

    let (address, ()) = tokio::join!(runtime.read_dlt645_address(10), async {
        assert_eq!(
            request(&mut socket).await,
            [0x68, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0x68, 0x13, 0, 0xdf, 0x16]
        );
        socket
            .write_all(&response(1, 0x93, &[1, 0, 0, 0, 0, 0]))
            .await
            .unwrap();
    });
    assert_eq!(address.unwrap(), "000000000001");
    assert!(runtime.read_dlt645_point(10, 21, 101).await.is_err());

    let (manual, ()) = tokio::join!(runtime.read_dlt645_point(10, 20, 102), async {
        assert_eq!(
            request(&mut socket).await,
            [0x68, 1, 0, 0, 0, 0, 0, 0x68, 0x11, 4, 0x33, 0x33, 0x36, 0x35, 0xb7, 0x16]
        );
        socket
            .write_all(&response(1, 0x91, &[0, 0, 3, 2, 0x45, 0x23, 0x81]))
            .await
            .unwrap();
    });
    manual.unwrap();
    assert_eq!(runtime.snapshot_dlt645(20)[&102].value, Some(-1.2345));

    let (unsupported, ()) = tokio::join!(runtime.read_dlt645_point(10, 20, 103), async {
        assert_eq!(
            &request(&mut socket).await[10..14],
            &[0x37, 0x36, 0x35, 0x34]
        );
        socket.write_all(&response(1, 0xd1, &[2])).await.unwrap();
    });
    assert!(unsupported.unwrap_err().contains("不支持"));

    runtime.set_device_enabled(20, true).unwrap();
    runtime.set_device_enabled(21, true).unwrap();
    let di = [0, 1, 1, 2];
    for address in [1, 2] {
        let tx = request(&mut socket).await;
        assert_eq!(tx[1], address);
        assert_eq!(&tx[10..14], &[0x33, 0x34, 0x34, 0x35]);
        let data = [di.as_slice(), &[0x12, 0x20 + address]].concat();
        let good = response(address, 0x91, &data);
        let mut bad = good.clone();
        let checksum = bad.len() - 2;
        bad[checksum] ^= 1;
        let wrong_di = response(address, 0x91, &[0, 1, 2, 2, 0x11, 0x11]);
        let first = [bad, wrong_di, good[..5].to_vec()].concat();
        socket.write_all(&first).await.unwrap();
        tokio::time::sleep(Duration::from_millis(12)).await;
        socket.write_all(&good[5..]).await.unwrap();
    }
    wait_until(|| runtime.snapshot_dlt645(21).contains_key(&201)).await;
    assert_eq!(runtime.snapshot_dlt645(20)[&101].raw, Some(211.2));
    assert_eq!(runtime.snapshot_dlt645(20)[&101].value, Some(422.4));
    assert_eq!(runtime.snapshot_dlt645(21)[&201].value, Some(221.2));
    runtime.set_device_enabled(20, false).unwrap();
    runtime.set_device_enabled(21, false).unwrap();

    let (timeout, ()) = tokio::join!(runtime.read_dlt645_point(10, 20, 101), async {
        assert_eq!(request(&mut socket).await[1], 1);
    });
    assert!(timeout.unwrap_err().contains("超时"));
    assert_eq!(runtime.snapshot_dlt645(20)[&101].value, Some(422.4));
    assert!(runtime.snapshot_dlt645(20)[&101].last_ok_ms.is_some());
    assert!(runtime.snapshot_dlt645(20)[&101].error.is_some());

    let (manual, ()) = tokio::join!(runtime.read_dlt645_point(10, 21, 201), async {
        assert_eq!(request(&mut socket).await[1], 2);
        socket
            .write_all(&response(2, 0x91, &[0, 1, 1, 2, 0x23, 0x22]))
            .await
            .unwrap();
    });
    manual.unwrap();
    assert_eq!(runtime.snapshot_dlt645(21)[&201].value, Some(222.3));

    drop(socket);
    wait_until(|| state_is(ConnStateKind::Reconnecting)).await;
    let (mut socket, _) = listener.accept().await.unwrap();
    wait_until(|| state_is(ConnStateKind::Connected)).await;
    let (manual, ()) = tokio::join!(runtime.read_dlt645_point(10, 21, 201), async {
        assert_eq!(request(&mut socket).await[1], 2);
        socket
            .write_all(&response(2, 0x91, &[0, 1, 1, 2, 0x23, 0x21]))
            .await
            .unwrap();
    });
    manual.unwrap();
    assert_eq!(runtime.snapshot_dlt645(21)[&201].value, Some(212.3));
    let mut logged = Vec::new();
    while let Ok(frame) = frames.try_recv() {
        logged.push(frame);
    }
    for id in [20, 21] {
        assert!(logged
            .iter()
            .any(|f| f.device_id == Some(id) && f.dir == "out"));
        assert!(logged
            .iter()
            .any(|f| f.device_id == Some(id) && f.dir == "in"));
    }
    assert!(logged
        .iter()
        .any(|f| f.note.as_deref().unwrap_or("").contains("未匹配")));
    let (cancelled, ()) = tokio::join!(runtime.read_dlt645_point(10, 20, 101), async {
        assert_eq!(request(&mut socket).await[1], 1);
        assert!(runtime.stop_connection(10));
    });
    assert!(cancelled.unwrap_err().contains("已停止"));
    assert!(runtime.read_dlt645_address(10).await.is_err());
}

#[test]
fn project_roundtrip_and_validation() {
    let mut project = ProjectSpec::demo();
    let original = serde_json::to_value(&project).unwrap();
    assert!(original["connections"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c.get("dlt645_meters").is_none()));
    let mut conn = connection(5020);
    conn.transport =
        serde_json::from_str(r#"{"kind":"dlt645_serial","serial_port":"COM8"}"#).unwrap();
    assert!(matches!(
        conn.transport,
        Transport::Dlt645Serial {
            baud: 2400,
            parity: netlink_core::model::Parity::Even,
            stop_bits: 1,
            ..
        }
    ));
    netlink_core::protocol::dlt645::validate(&conn).unwrap();
    project.connections.push(conn);
    let mut reopened: ProjectSpec =
        serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
    let conn = reopened.connections.last_mut().unwrap();
    assert_eq!(conn.dlt645_meters[0].address, "000000000001");
    conn.dlt645_meters[1].address = conn.dlt645_meters[0].address.clone();
    assert!(netlink_core::protocol::dlt645::validate(conn).is_err());
    reopened.connections.pop();
    assert_eq!(serde_json::to_value(reopened).unwrap(), original);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merges_selected_members_and_remembers_block_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut conn = connection(listener.local_addr().unwrap().port());
    conn.dlt645_meters.truncate(1);
    let meter = &mut conn.dlt645_meters[0];
    meter.points.truncate(1);
    meter.poll = true;
    meter.poll_period_ms = 80;
    for (id, di, bytes, poll) in [
        (102, "02010200", 2, false),
        (103, "02010300", 2, true),
        (104, "02020100", 3, true),
        (105, "02020200", 3, true),
    ] {
        let mut point = meter.points[0].clone();
        point.id = id;
        point.di = di.into();
        point.bytes = bytes;
        point.poll = poll;
        point.scale = 1.0;
        meter.points.push(point);
    }
    let runtime = RuntimeManager::new();
    runtime.start_connection(&conn).await.unwrap();
    let (mut socket, _) = listener.accept().await.unwrap();

    for round in 0..2 {
        let voltage_block = [0, 0xff, 1, 2];
        assert_eq!(
            &request(&mut socket).await[10..14],
            &[0x33, 0x32, 0x34, 0x35]
        );
        socket
            .write_all(&response(
                1,
                0x91,
                &[&voltage_block[..], &[0x12, 0x21, 0, 0, 0x34, 0x22]].concat(),
            ))
            .await
            .unwrap();

        if round == 0 {
            assert_eq!(
                &request(&mut socket).await[10..14],
                &[0x33, 0x32, 0x35, 0x35]
            );
            socket.write_all(&response(1, 0xd1, &[2])).await.unwrap();
        }
        for phase in 1..=2 {
            assert_eq!(
                &request(&mut socket).await[10..14],
                &[0x33, 0x33 + phase, 0x35, 0x35]
            );
            socket
                .write_all(&response(1, 0x91, &[0, phase, 2, 2, 0x10 + phase, 0, 0]))
                .await
                .unwrap();
        }
    }
    wait_until(|| runtime.snapshot_dlt645(20).contains_key(&105)).await;
    let points = runtime.snapshot_dlt645(20);
    assert_eq!(points[&101].value, Some(422.4));
    assert_eq!(points[&103].value, Some(223.4));
    assert_eq!(points[&101].last_ok_ms, points[&103].last_ok_ms);
    assert!(!points.contains_key(&102));
    assert_eq!(points[&105].value, Some(1.2));
    assert!(runtime.stop_connection(10));
}
