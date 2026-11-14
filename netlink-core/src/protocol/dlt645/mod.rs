use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::model::{Connection, Dlt645Format, Dlt645Meter, Dlt645Point, Transport};
use crate::protocol::FrameEvent;
use crate::runtime::{now_ms, ConnStateKind, RuntimeManager};

mod wire;
use wire::{Frame, Request};

const IO_TICK: Duration = Duration::from_millis(50);

pub fn validate(conn: &Connection) -> Result<(), String> {
    match &conn.transport {
        Transport::Dlt645Serial {
            serial_port,
            baud,
            stop_bits,
            timeout_ms,
            ..
        } => {
            if serial_port.trim().is_empty() || *baud == 0 || !matches!(stop_bits, 1 | 2) {
                return Err("645 串口、波特率或停止位无效".into());
            }
            validate_timeout(*timeout_ms)?;
        }
        Transport::Dlt645Tcp {
            host,
            port,
            timeout_ms,
        } => {
            if host.trim().is_empty() || *port == 0 {
                return Err("645 TCP 主机和端口不能为空".into());
            }
            validate_timeout(*timeout_ms)?;
        }
        _ if conn.dlt645_meters.is_empty() => return Ok(()),
        _ => return Err("645 电表须配置在 645 连接下".into()),
    }
    if !conn.devices.is_empty() || !conn.can_nodes.is_empty() {
        return Err("645 连接仅支持配置 645 电表".into());
    }

    let mut addresses = HashSet::new();
    let mut meter_ids = HashSet::new();
    let mut point_ids = HashSet::new();
    for meter in &conn.dlt645_meters {
        wire::address_bytes(&meter.address)?;
        if !addresses.insert(&meter.address) || !meter_ids.insert(meter.id) {
            return Err(format!("电表地址或 ID 重复：{}", meter.address));
        }
        if meter.name.trim().is_empty() || meter.poll_period_ms == 0 {
            return Err("电表名称和正数轮询周期必填".into());
        }
        let mut dis = HashSet::new();
        for point in &meter.points {
            if point.di.len() != 8 || !point.di.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("DI 必须是 8 位十六进制字符".into());
            }
            if !dis.insert(point.di.to_ascii_uppercase()) || !point_ids.insert(point.id) {
                return Err(format!("点位 DI 或 ID 重复：{}", point.di));
            }
            let max_bytes = match point.format {
                Dlt645Format::Bcd | Dlt645Format::SignedBcd => 6,
                Dlt645Format::Unsigned | Dlt645Format::Signed => 4,
            };
            if point.name.trim().is_empty()
                || !(1..=max_bytes).contains(&point.bytes)
                || point.decimals > 12
                || !point.scale.is_finite()
            {
                return Err(format!(
                    "点位「{}」的名称、长度、小数位或倍率无效",
                    point.name
                ));
            }
        }
    }
    Ok(())
}

fn validate_timeout(ms: u32) -> Result<(), String> {
    (50..=60_000)
        .contains(&ms)
        .then_some(())
        .ok_or_else(|| "645 应答超时须在 50–60000 ms 内".into())
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct Sample {
    pub raw: Option<f64>,
    pub value: Option<f64>,
    pub last_ok_ms: Option<u64>,
    pub error: Option<String>,
}

impl Sample {
    fn success(raw: f64, value: f64, time: u64) -> Self {
        Self {
            raw: Some(raw),
            value: Some(value),
            last_ok_ms: Some(time),
            error: None,
        }
    }
}

pub(crate) struct MeterState {
    pub spec: Dlt645Meter,
    samples: HashMap<u64, Sample>,
    due: Instant,
    pending: VecDeque<PollRead>,
    rejected_blocks: HashSet<u32>,
}

impl MeterState {
    pub fn new(mut spec: Dlt645Meter, enabled: Option<&[u64]>) -> Self {
        if let Some(ids) = enabled {
            spec.poll = ids.contains(&spec.id);
        }
        Self {
            spec,
            samples: HashMap::new(),
            due: Instant::now(),
            pending: VecDeque::new(),
            rejected_blocks: HashSet::new(),
        }
    }

    pub fn polling(&self) -> bool {
        self.spec.enabled && self.spec.poll
    }

    pub fn set_polling(&mut self, enabled: bool) {
        // 树上的启停控制轮询，暂停后仍可手动抄读。
        self.spec.enabled = true;
        self.spec.poll = enabled;
        self.pending.clear();
        self.due = Instant::now();
    }

    pub fn samples(&self) -> HashMap<u64, Sample> {
        self.samples.clone()
    }

    fn next_poll(&mut self, now: Instant) -> Option<PollRead> {
        if !self.polling() || self.due > now {
            return None;
        }
        if self.pending.is_empty() {
            self.pending = plan_reads(&self.spec.points, &self.rejected_blocks);
        }
        let read = self.pending.pop_front()?;
        if self.pending.is_empty() {
            self.due = now + Duration::from_millis(self.spec.poll_period_ms.into());
        }
        Some(read)
    }

    fn reject_block(&mut self, di: u32, points: Vec<(usize, Dlt645Point)>) {
        self.rejected_blocks.insert(di);
        if self.polling() {
            for (_, point) in points.into_iter().rev() {
                self.pending.push_front(PollRead::Point(point));
            }
            self.due = Instant::now();
        }
    }
}

enum PollRead {
    Point(Dlt645Point),
    Block {
        di: u32,
        len: usize,
        points: Vec<(usize, Dlt645Point)>,
    },
}

// 附录 A.3 定义的定长数据块。偏移包含所有成员，即使用户只勾选部分指标。
const VARIABLE_BLOCKS: [(u32, std::ops::RangeInclusive<u32>, usize); 4] = [
    (0x02010000, 1..=3, 2),
    (0x02020000, 1..=3, 3),
    (0x02030000, 0..=3, 3),
    (0x02060000, 0..=3, 2),
];

fn plan_reads(points: &[Dlt645Point], rejected: &HashSet<u32>) -> VecDeque<PollRead> {
    let points: Vec<_> = points
        .iter()
        .filter(|p| p.poll)
        .map(|p| (u32::from_str_radix(&p.di, 16).expect("validated DI"), p))
        .collect();
    let mut consumed = HashSet::new();
    let mut plan = VecDeque::new();
    for &(di, point) in &points {
        if consumed.contains(&di) {
            continue;
        }
        let group = VARIABLE_BLOCKS.iter().find(|(base, members, width)| {
            di & 0xffff00ff == *base
                && members.contains(&((di >> 8) & 0xff))
                && usize::from(point.bytes) == *width
                && !rejected.contains(&(base | 0xff00))
        });
        if let Some((base, members, width)) = group {
            let mut selected = Vec::new();
            for member in members.clone() {
                let member_di = base | (member << 8);
                if let Some((_, p)) = points
                    .iter()
                    .find(|(id, p)| *id == member_di && usize::from(p.bytes) == *width)
                {
                    let offset = (member - members.start()) as usize * width;
                    selected.push((member_di, offset, (*p).clone()));
                }
            }
            if selected.len() > 1 {
                consumed.extend(selected.iter().map(|(di, _, _)| *di));
                plan.push_back(PollRead::Block {
                    di: base | 0xff00,
                    len: members.clone().count() * width,
                    points: selected
                        .into_iter()
                        .map(|(_, offset, p)| (offset, p))
                        .collect(),
                });
                continue;
            }
        }
        plan.push_back(PollRead::Point(point.clone()));
    }
    plan
}

pub(crate) enum Command {
    Address(oneshot::Sender<Result<String, String>>),
    Read {
        meter_id: u64,
        point_id: u64,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

pub(crate) type Meters = Arc<Mutex<Vec<MeterState>>>;

#[derive(Clone)]
pub(crate) struct Worker {
    pub conn_id: u64,
    pub meters: Meters,
    pub manager: RuntimeManager,
    pub cancel: CancellationToken,
    pub timeout: Duration,
}

pub(crate) fn serve_serial(
    worker: Worker,
    mut port: impl Read + Write + Send + 'static,
    mut commands: mpsc::Receiver<Command>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        worker.state(ConnStateKind::Connected, None);
        if let Err(error) = worker.run(&mut port, &mut commands) {
            worker.state(
                ConnStateKind::Disconnected,
                Some(format!("串口 I/O 失败，请重启连接：{error}")),
            );
        }
    })
}

pub(crate) fn serve_tcp(
    worker: Worker,
    host: String,
    port: u16,
    mut commands: mpsc::Receiver<Command>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        worker.state(ConnStateKind::Connecting, None);
        let mut delay = 1;
        while !worker.cancel.is_cancelled() {
            let connection = tokio::select! {
                _ = worker.cancel.cancelled() => return,
                result = tokio::time::timeout(worker.timeout, tokio::net::TcpStream::connect((host.as_str(), port))) => {
                    result.map_err(io::Error::from).and_then(|stream| stream?.into_std())
                }
            };
            let result = match connection {
                Ok(mut stream) => {
                    let current = worker.clone();
                    let task = tokio::task::spawn_blocking(move || {
                        let result = (|| {
                            stream.set_nonblocking(false)?;
                            stream.set_nodelay(true)?;
                            stream.set_read_timeout(Some(IO_TICK.min(current.timeout)))?;
                            stream.set_write_timeout(Some(IO_TICK.min(current.timeout)))?;
                            current.state(ConnStateKind::Connected, None);
                            current.run(&mut stream, &mut commands)
                        })();
                        (commands, result)
                    });
                    let Ok((receiver, result)) = task.await else {
                        return;
                    };
                    commands = receiver;
                    delay = 1;
                    result
                }
                Err(error) => Err(error),
            };
            let Err(error) = result else { return };
            worker.state(ConnStateKind::Reconnecting, Some(error.to_string()));
            while let Ok(cmd) = commands.try_recv() {
                cmd.reject("TCP 正在重连");
            }
            tokio::select! {
                _ = worker.cancel.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(delay)) => {},
            }
            delay = (delay * 2).min(30);
        }
    })
}

impl Command {
    fn reject(self, reason: &str) {
        match self {
            Self::Address(reply) => {
                let _ = reply.send(Err(reason.into()));
            }
            Self::Read { reply, .. } => {
                let _ = reply.send(Err(reason.into()));
            }
        }
    }
}

struct Poll {
    id: u64,
    address: String,
    point: Dlt645Point,
}

enum TransactionError {
    Read(String),
    Io(io::Error),
}

impl Worker {
    fn state(&self, kind: ConnStateKind, detail: Option<String>) {
        self.manager
            .set_dlt645_conn_state(self.conn_id, &self.cancel, kind, detail);
    }

    fn run(
        &self,
        io: &mut (impl Read + Write),
        commands: &mut mpsc::Receiver<Command>,
    ) -> io::Result<()> {
        let mut incoming = Vec::new();
        let mut cursor = 0;
        while !self.cancel.is_cancelled() && !commands.is_closed() {
            match commands.try_recv() {
                Ok(Command::Address(reply)) => {
                    let result = self.transact(io, &mut incoming, Request::Address, None);
                    let result = match result {
                        Ok(frame) => Ok(wire::address_text(frame.address)),
                        Err(TransactionError::Read(reason)) => Err(reason),
                        Err(TransactionError::Io(error)) => {
                            let _ = reply.send(Err(error.to_string()));
                            return Err(error);
                        }
                    };
                    let _ = reply.send(result);
                    continue;
                }
                Ok(Command::Read {
                    meter_id,
                    point_id,
                    reply,
                }) => {
                    let point = self
                        .meters
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|m| m.spec.id == meter_id)
                        .and_then(|m| {
                            m.spec
                                .points
                                .iter()
                                .find(|p| p.id == point_id)
                                .map(|point| Poll {
                                    id: meter_id,
                                    address: m.spec.address.clone(),
                                    point: point.clone(),
                                })
                        });
                    let Some(point) = point else {
                        let _ = reply.send(Err("点位不在该连接下".into()));
                        continue;
                    };
                    match self.read_point(io, &mut incoming, point) {
                        Ok(result) => {
                            let _ = reply.send(result);
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error.to_string()));
                            return Err(error);
                        }
                    }
                    continue;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => break,
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            let poll = {
                let mut meters = self.meters.lock().unwrap();
                let len = meters.len();
                (0..len).find_map(|offset| {
                    let index = (cursor + offset) % len;
                    let meter = &mut meters[index];
                    let read = meter.next_poll(Instant::now())?;
                    cursor = (index + 1) % len;
                    Some((meter.spec.id, meter.spec.address.clone(), read))
                })
            };
            if let Some((id, address, read)) = poll {
                match read {
                    PollRead::Point(point) => {
                        let _ = self.read_point(io, &mut incoming, Poll { id, address, point })?;
                    }
                    PollRead::Block { di, len, points } => {
                        self.read_block(io, &mut incoming, id, &address, di, len, points)?;
                    }
                }
            } else {
                // 顺便清理迟到的应答并检测 TCP EOF，手动操作最多等待一个 I/O tick。
                self.read_chunk(io, &mut incoming)?;
                while let Some(frame) = Frame::take(&mut incoming) {
                    self.event(None, "in", &frame.bytes, "未匹配的应答".into());
                }
            }
        }
        Ok(())
    }

    fn read_block(
        &self,
        io: &mut (impl Read + Write),
        incoming: &mut Vec<u8>,
        id: u64,
        address: &str,
        di: u32,
        len: usize,
        points: Vec<(usize, Dlt645Point)>,
    ) -> io::Result<()> {
        let request = Request::Data {
            address: wire::address_bytes(address).expect("validated meter address"),
            di,
        };
        let result = match self.transact(io, incoming, request, Some(id)) {
            Ok(frame) if frame.data.len() == 4 + len => points
                .iter()
                .map(|(offset, point)| {
                    let start = 4 + offset;
                    let (raw, value) = wire::decode_value(
                        point,
                        &frame.data[start..start + usize::from(point.bytes)],
                    )?;
                    Ok((point.id, raw, value))
                })
                .collect::<Result<Vec<_>, String>>(),
            Ok(_) => Err(format!("数据块长度与标准不符（预期 {len} 字节）")),
            Err(TransactionError::Read(reason)) => Err(reason),
            Err(TransactionError::Io(error)) => return Err(error),
        };
        let mut meters = self.meters.lock().unwrap();
        let meter = meters
            .iter_mut()
            .find(|m| m.spec.id == id)
            .expect("running meter");
        match result {
            Ok(samples) => {
                let time = now_ms();
                meter.samples.extend(
                    samples
                        .into_iter()
                        .map(|(id, raw, value)| (id, Sample::success(raw, value, time))),
                );
            }
            Err(reason) if !self.cancel.is_cancelled() => {
                // ponytail: 连接存续期间记住失败，重开连接后再探测一次。
                meter.reject_block(di, points);
                self.event(
                    Some(id),
                    "in",
                    &[],
                    format!("数据块 {di:08X}：{reason}，改为逐项读取"),
                );
            }
            Err(_) => {}
        }
        Ok(())
    }

    fn read_point(
        &self,
        io: &mut (impl Read + Write),
        incoming: &mut Vec<u8>,
        poll: Poll,
    ) -> io::Result<Result<(), String>> {
        let request = Request::Data {
            address: wire::address_bytes(&poll.address).expect("validated meter address"),
            di: u32::from_str_radix(&poll.point.di, 16).expect("validated DI"),
        };
        let result = match self.transact(io, incoming, request, Some(poll.id)) {
            Ok(frame) => wire::decode_value(&poll.point, &frame.data[4..]).inspect_err(|reason| {
                self.event(
                    Some(poll.id),
                    "in",
                    &[],
                    format!("{} · {reason}", request.description()),
                );
            }),
            Err(TransactionError::Read(reason)) => Err(reason),
            Err(TransactionError::Io(error)) => return Err(error),
        };
        let mut meters = self.meters.lock().unwrap();
        let meter = meters
            .iter_mut()
            .find(|m| m.spec.id == poll.id)
            .expect("running meter");
        let sample = meter.samples.entry(poll.point.id).or_default();
        match result {
            Ok((raw, value)) => {
                *sample = Sample::success(raw, value, now_ms());
                Ok(Ok(()))
            }
            Err(reason) => {
                sample.error = Some(reason.clone());
                Ok(Err(reason))
            }
        }
    }

    fn transact(
        &self,
        io: &mut (impl Read + Write),
        incoming: &mut Vec<u8>,
        request: Request,
        meter_id: Option<u64>,
    ) -> Result<Frame, TransactionError> {
        incoming.clear();
        let tx = request.bytes();
        io.write_all(&tx).map_err(TransactionError::Io)?;
        self.event(meter_id, "out", &tx, request.description());
        let deadline = Instant::now() + self.timeout;
        let mut failure = None;
        while !self.cancel.is_cancelled() && Instant::now() < deadline {
            while let Some(frame) = Frame::take(incoming) {
                let note = format!("{} · 控制码 {:02X}", request.description(), frame.control);
                if !frame.valid {
                    self.event(
                        None,
                        "in",
                        &frame.bytes,
                        format!("{note} · 校验或结束符错误"),
                    );
                    if request.address_matches(frame.address) {
                        failure = Some("应答校验或结束符错误".to_string());
                    }
                    continue;
                }
                let Some(result) = request.response(&frame) else {
                    self.event(
                        None,
                        "in",
                        &frame.bytes,
                        format!(
                            "{note} · 未匹配，来源 {}",
                            wire::address_text(frame.address)
                        ),
                    );
                    continue;
                };
                let detail = result
                    .as_ref()
                    .err()
                    .map_or(note.clone(), |e| format!("{note} · {e}"));
                self.event(meter_id, "in", &frame.bytes, detail);
                while let Some(extra) = Frame::take(incoming) {
                    self.event(None, "in", &extra.bytes, "事务结束后的额外应答".into());
                }
                return result.map(|_| frame).map_err(TransactionError::Read);
            }
            self.read_chunk(io, incoming)
                .map_err(TransactionError::Io)?;
        }
        if self.cancel.is_cancelled() {
            return Err(TransactionError::Read("连接已停止".into()));
        }
        let reason = failure.unwrap_or_else(|| "应答超时".into());
        self.event(
            meter_id,
            "in",
            &[],
            format!("{} · {reason}", request.description()),
        );
        // ponytail: 超时后有界地排空到一个安静读周期。极晚的同地址同 DI 应答无法区分。
        let drain_until = Instant::now() + self.timeout.min(Duration::from_millis(200));
        incoming.clear();
        while !self.cancel.is_cancelled() && Instant::now() < drain_until {
            if !self
                .read_chunk(io, incoming)
                .map_err(TransactionError::Io)?
            {
                break;
            }
            while let Some(frame) = Frame::take(incoming) {
                self.event(None, "in", &frame.bytes, "超时后的迟到应答".into());
            }
        }
        Err(TransactionError::Read(reason))
    }

    fn read_chunk(&self, io: &mut impl Read, incoming: &mut Vec<u8>) -> io::Result<bool> {
        let mut chunk = [0; 512];
        match io.read(&mut chunk) {
            Ok(0) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "连接已断开")),
            Ok(n) => {
                incoming.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    fn event(&self, meter_id: Option<u64>, dir: &'static str, bytes: &[u8], note: String) {
        self.manager.emit_frame(FrameEvent::new(
            self.conn_id,
            meter_id,
            dir,
            bytes,
            Some(note),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SimulatedSerial {
        response: io::Cursor<Vec<u8>>,
        disconnected: bool,
    }

    impl Read for SimulatedSerial {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.disconnected {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let max = buf.len().min(3);
            let n = self.response.read(&mut buf[..max])?;
            if n == 0 {
                std::thread::sleep(Duration::from_millis(5));
                return Err(io::ErrorKind::TimedOut.into());
            }
            Ok(n)
        }
    }

    impl Write for SimulatedSerial {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let frame = match &buf[12..14] {
                [0x13, 0] => vec![
                    0x68, 0x01, 0, 0, 0, 0, 0, 0x68, 0x93, 6, 0x34, 0x33, 0x33, 0x33, 0x33, 0x33,
                    0x9d, 0x16,
                ],
                [0x11, 4] if buf[14] == 0x34 => {
                    // DI 00000001, signed 16-bit integer -1234, one decimal place.
                    vec![
                        0x68, 1, 0, 0, 0, 0, 0, 0x68, 0x91, 6, 0x34, 0x33, 0x33, 0x33, 0x61, 0x2e,
                        0xc4, 0x16,
                    ]
                }
                [0x11, 4] => {
                    self.disconnected = true;
                    vec![]
                }
                _ => panic!("unexpected request: {buf:?}"),
            };
            self.response = io::Cursor::new(frame);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn serial_worker_reads_fragmented_data_and_reports_io_failure() {
        let spec = serde_json::from_value(serde_json::json!({
            "id": 2, "name": "模拟表", "address": "000000000001", "poll": false,
            "points": [
                {"id": 3, "name": "数值", "di": "00000001", "format": "signed", "bytes": 2, "decimals": 1},
                {"id": 4, "name": "断开", "di": "00000002", "format": "bcd", "bytes": 2, "decimals": 0}
            ]
        })).unwrap();
        let meters = Arc::new(Mutex::new(vec![MeterState::new(spec, None)]));
        let manager = RuntimeManager::new();
        let worker = Worker {
            conn_id: 1,
            meters: meters.clone(),
            manager: manager.clone(),
            cancel: CancellationToken::new(),
            timeout: Duration::from_millis(150),
        };
        let (send, receive) = mpsc::channel(4);
        let task = serve_serial(
            worker,
            SimulatedSerial {
                response: io::Cursor::new(vec![]),
                disconnected: false,
            },
            receive,
        );

        let (reply, result) = oneshot::channel();
        send.send(Command::Address(reply)).await.unwrap();
        assert_eq!(result.await.unwrap().unwrap(), "000000000001");

        let (reply, result) = oneshot::channel();
        send.send(Command::Read {
            meter_id: 2,
            point_id: 3,
            reply,
        })
        .await
        .unwrap();
        result.await.unwrap().unwrap();
        assert_eq!(meters.lock().unwrap()[0].samples[&3].value, Some(-123.4));

        let (reply, result) = oneshot::channel();
        send.send(Command::Read {
            meter_id: 2,
            point_id: 4,
            reply,
        })
        .await
        .unwrap();
        assert!(result.await.unwrap().is_err());
        task.await.unwrap();
        assert_eq!(
            manager.snapshot_conn_states()[0].1,
            ConnStateKind::Disconnected
        );
    }
}
