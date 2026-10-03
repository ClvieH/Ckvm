// File-transfer domain: wire packets, the send path (chunking, resume
// negotiation, retries, cancel), the receive path (chunk ordering, .part
// staging, adoption of interrupted transfers, SHA-256 finish check) and the
// target/sanitizer helpers. Extracted from lib.rs as a pure move — all
// signatures unchanged; lib.rs keeps the Tauri command glue, the progress
// reporter and the pending-queue/history persistence.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use tauri::{AppHandle, Manager};

use crate::quic_transport;
use crate::{
    client_log_dir, decode_wire_packet, encode_wire_packet, format_bytes,
    host_candidates, now_ms, normalize_quic_port, random_hex,
    record_transfer_history, role_receives_from_peers, split_host_port,
    touch_paired_controller_usage, FileTransferProgressReporter,
    TransferHistoryEntry, LanPeer, LayoutState,
};

pub(crate) const FILE_TRANSFER_PROTOCOL: &str = "mykvm.file-transfer.v1";
pub(crate) const FILE_TRANSFER_CHUNK_BYTES: usize = 256 * 1024;
pub(crate) const FILE_TRANSFER_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
// Send retries per transfer packet: one lost ACK must not abort a multi-GB
// transfer. The receiver tolerates duplicated chunks, which makes resends safe.
pub(crate) const FILE_TRANSFER_SEND_ATTEMPTS: u32 = 3;
pub(crate) const FILE_TRANSFER_RETRY_DELAY_MS: u64 = 250;
// Cap on simultaneously staged incoming transfers, so a burst of "start"
// packets cannot pile up unbounded .part files on disk.
pub(crate) const MAX_CONCURRENT_INCOMING_TRANSFERS: usize = 4;
// Progress is emitted at most this often per file while sending (plus always on
// completion) so a multi-GB transfer doesn't flood the webview with events.
pub(crate) const FILE_TRANSFER_PROGRESS_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone)]
pub(crate) struct FileTransferTarget {
    pub(crate) device_id: String,
    pub(crate) name: String,
    pub(crate) addr: String,
    pub(crate) transport_public_key: String,
    pub(crate) protocol_version: u16,
    pub(crate) cluster_id: String,
    pub(crate) pair_secret: String,
}

#[derive(Debug, Clone)]
pub(crate) struct TransferFile {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) total_bytes: u64,
}

impl std::fmt::Debug for IncomingFileTransfer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IncomingFileTransfer")
            .field("origin_id", &self.origin_id)
            .field("target_id", &self.target_id)
            .field("file_name", &self.file_name)
            .field("total_bytes", &self.total_bytes)
            .field("received_bytes", &self.received_bytes)
            .field("next_chunk_index", &self.next_chunk_index)
            .field("temp_path", &self.temp_path)
            .field("final_path", &self.final_path)
            .field("staged", &self.staged)
            .field("sha256_active", &self.sha256.is_some())
            .finish()
    }
}

pub(crate) struct IncomingFileTransfer {
    pub(crate) origin_id: String,
    pub(crate) target_id: String,
    pub(crate) file_name: String,
    pub(crate) total_bytes: u64,
    pub(crate) received_bytes: u64,
    pub(crate) next_chunk_index: u64,
    pub(crate) temp_path: PathBuf,
    pub(crate) final_path: PathBuf,
    // ShareMouse-style drag: on finish, hand the completed file to the drag
    // placer (which drops it into the folder under the cursor on release)
    // instead of leaving it at final_path.
    pub(crate) staged: bool,
    // Running SHA-256 over the received bytes; compared against the sender's
    // digest on the finish packet. None only if the hasher itself failed.
    pub(crate) sha256: Option<ring::digest::Context>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileTransferSummary {
    pub(crate) target_name: String,
    pub(crate) file_count: usize,
    pub(crate) byte_count: u64,
}

// One progress update for the sending-side toast. `file_index` is 1-based.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileTransferProgress {
    pub(crate) transfer_id: String,
    pub(crate) file_name: String,
    pub(crate) target_name: String,
    pub(crate) sent_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) file_index: usize,
    pub(crate) file_count: usize,
    pub(crate) done: bool,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileTransferPacket {
    pub(crate) protocol: String,
    pub(crate) kind: String,
    pub(crate) transfer_id: String,
    pub(crate) origin_id: String,
    pub(crate) target_id: String,
    pub(crate) cluster_id: String,
    pub(crate) pair_secret: String,
    pub(crate) file_name: String,
    pub(crate) total_bytes: u64,
    pub(crate) chunk_index: u64,
    pub(crate) offset: u64,
    #[serde(default)]
    pub(crate) data: Vec<u8>,
    // Edge drag-drop: land the file on the receiver's Desktop instead of the
    // transfers folder. Named-field msgpack, so old peers just ignore it (and
    // an old sender leaves it false here).
    #[serde(default)]
    pub(crate) drop_to_desktop: bool,
    // ShareMouse-style drag: the receiver stages the file and, when the drag is
    // released over it, drops it into the folder under the cursor (else Desktop)
    // instead of placing it immediately.
    #[serde(default)]
    pub(crate) drag_drop: bool,
    // A device's log fetched by its peer: land it in the "MyKVM Remote Logs"
    // folder instead of the transfers folder.
    #[serde(default)]
    pub(crate) client_log: bool,
    // SHA-256 of the whole file, attached to the "finish" packet by new
    // senders. Absent (empty) from older senders — the receiver then skips
    // verification, so mixed-version pairs keep working.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) file_sha256: Vec<u8>,
    // Resume support (new senders only): a "start" whose ACK carries
    // "ok:<offset>" continues from that byte offset into a matching .part the
    // receiver kept. Absent on non-start packets and from older senders.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub(crate) resume_from: u64,
}

pub(crate) fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

pub(crate) fn file_transfer_target_for_device(
    layout: &LayoutState,
    peers: &[LanPeer],
    device_id: &str,
) -> Result<FileTransferTarget, String> {
    if layout.cluster_id.trim().is_empty() || layout.pair_secret.trim().is_empty() {
        return Err("当前设备尚未完成配对，无法传输文件。".into());
    }

    if let Some(device) = layout
        .devices
        .iter()
        .find(|device| device.id == device_id && device.role != "local")
    {
        if !device.online || !device.input_ready {
            return Err(format!("{} 当前不在线，无法传输文件。", device.name));
        }
        if device.protocol_version != quic_transport::PROTOCOL_VERSION
            || device.transport_public_key.trim().is_empty()
        {
            return Err(format!("{} 版本过旧，请先升级 MyKVM。", device.name));
        }
        let quic_port = normalize_quic_port(device.transport_port, device.quic_port);
        let host = file_transfer_host(&device.host)
            .ok_or_else(|| format!("{} 缺少可用地址。", device.name))?;

        return Ok(FileTransferTarget {
            device_id: device.id.clone(),
            name: device.name.clone(),
            addr: format!("{host}:{quic_port}"),
            transport_public_key: device.transport_public_key.clone(),
            protocol_version: device.protocol_version,
            cluster_id: layout.cluster_id.clone(),
            pair_secret: layout.pair_secret.clone(),
        });
    }

    if role_receives_from_peers(&layout.machine_role) {
        if let Some(controller) = layout
            .paired_controllers
            .iter()
            .find(|controller| controller.id == device_id)
        {
            let peer = peers
                .iter()
                .find(|peer| {
                    peer.id == controller.id
                        || (!controller.transport_public_key.trim().is_empty()
                            && peer.transport_public_key == controller.transport_public_key)
                })
                .ok_or_else(|| format!("{} 当前不在线，无法传输文件。", controller.name))?;
            if peer.protocol_version != quic_transport::PROTOCOL_VERSION
                || peer.transport_public_key.trim().is_empty()
                || peer.quic_port == 0
            {
                return Err(format!("{} 版本过旧，请先升级 MyKVM。", controller.name));
            }
            let host = if !peer.ip.trim().is_empty() {
                peer.ip.clone()
            } else {
                file_transfer_host(&peer.host)
                    .or_else(|| file_transfer_host(&controller.ip))
                    .or_else(|| file_transfer_host(&controller.host))
                    .ok_or_else(|| format!("{} 缺少可用地址。", controller.name))?
            };

            return Ok(FileTransferTarget {
                // The controller's id as it announces itself now, not as stored
                // at pairing: it is host + IP, so it moves with the IP (a TUN
                // proxy flipped it), and the receiver matches target_id exactly.
                device_id: peer.id.clone(),
                name: controller.name.clone(),
                addr: format!("{}:{}", host, peer.quic_port),
                transport_public_key: peer.transport_public_key.clone(),
                protocol_version: peer.protocol_version,
                cluster_id: layout.cluster_id.clone(),
                pair_secret: layout.pair_secret.clone(),
            });
        }
    }

    Err("没有找到可传输的目标设备。".into())
}

pub(crate) fn file_transfer_host(host_value: &str) -> Option<String> {
    host_candidates(host_value)
        .into_iter()
        .find_map(|candidate| {
            let (host, _) = split_host_port(&candidate);
            (!host.trim().is_empty()).then_some(host)
        })
}

pub(crate) fn collect_transfer_files(paths: &[String]) -> Result<Vec<TransferFile>, String> {
    let mut files = Vec::new();
    for path_value in paths {
        let path_value = path_value.trim();
        if path_value.is_empty() {
            continue;
        }
        let path = PathBuf::from(path_value);
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("无法读取文件 {}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("暂不支持传输文件夹或特殊文件：{}", path.display()));
        }
        if metadata.len() > FILE_TRANSFER_MAX_FILE_BYTES {
            return Err(format!(
                "{} 超过单文件上限 {}。",
                path.display(),
                format_bytes(FILE_TRANSFER_MAX_FILE_BYTES)
            ));
        }
        let name = transfer_file_name(&path)?;
        files.push(TransferFile {
            path,
            name,
            total_bytes: metadata.len(),
        });
    }

    if files.is_empty() {
        Err("请选择要传输的文件。".into())
    } else {
        Ok(files)
    }
}

pub(crate) fn transfer_file_name(path: &Path) -> Result<String, String> {
    let raw_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    sanitize_transfer_file_name(&raw_name).ok_or_else(|| {
        format!(
            "文件名不可用于传输：{}",
            if raw_name.is_empty() {
                path.display().to_string()
            } else {
                raw_name
            }
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn send_transfer_file(
    quic_transport: &quic_transport::TransportHandle,
    origin_id: &str,
    target: &FileTransferTarget,
    file: &TransferFile,
    transfer_id: &str,
    drop_mode: DropMode,
    reporter: Option<&FileTransferProgressReporter>,
    cancel: Option<&AtomicBool>,
) -> Result<u64, String> {
    if let Some(reporter) = reporter {
        reporter.emit(transfer_id, file, 0, false, None);
    }

    let outcome = send_transfer_file_bytes(
        quic_transport,
        origin_id,
        target,
        file,
        drop_mode,
        transfer_id,
        reporter,
        cancel,
    );

    if let Some(reporter) = reporter {
        match &outcome {
            Ok(_) => reporter.emit(transfer_id, file, file.total_bytes, true, None),
            Err(error) => reporter.emit(transfer_id, file, 0, true, Some(error.clone())),
        }
    }

    outcome
}

pub(crate) fn new_transfer_id(prefix: &str) -> String {
    format!("{prefix}-{}-{}", now_ms(), random_hex(8))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn send_transfer_file_bytes(
    quic_transport: &quic_transport::TransportHandle,
    origin_id: &str,
    target: &FileTransferTarget,
    file: &TransferFile,
    drop_mode: DropMode,
    transfer_id: &str,
    reporter: Option<&FileTransferProgressReporter>,
    cancel: Option<&AtomicBool>,
) -> Result<u64, String> {
    let mut packet_count = 0_u64;

    // Start negotiation: the receiver answers "ok" (fresh) or "ok:<offset>"
    // (it kept a .part from an interrupted attempt of this file). Retries
    // cover a lost ACK; the receiver tolerates duplicated starts.
    let start_packet = file_transfer_packet(
        "start",
        transfer_id,
        origin_id,
        target,
        &file.name,
        file.total_bytes,
        0,
        0,
        Vec::new(),
        drop_mode,
    );
    let resume_from =
        send_file_transfer_start(quic_transport, target, start_packet, cancel)?;

    let mut file_handle = fs::File::open(&file.path)
        .map_err(|error| format!("无法打开文件 {}: {error}", file.path.display()))?;
    let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
    // Resume: hash the source prefix [0, resume_from) so the finish digest
    // still covers the whole file, then continue reading from there.
    if resume_from > 0 {
        file_handle
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("续传定位失败: {error}"))?;
        let mut fed = 0_u64;
        let mut prefix = vec![0_u8; FILE_TRANSFER_CHUNK_BYTES];
        while fed < resume_from {
            let want = ((resume_from - fed) as usize).min(prefix.len());
            let read = file_handle
                .read(&mut prefix[..want])
                .map_err(|error| format!("续传读取失败: {error}"))?;
            if read == 0 {
                return Err("源文件比已接收部分短，无法续传。".into());
            }
            hasher.update(&prefix[..read]);
            fed += read as u64;
        }
        file_handle
            .seek(SeekFrom::Start(resume_from))
            .map_err(|error| format!("续传定位失败: {error}"))?;
    }
    let mut buffer = vec![0_u8; FILE_TRANSFER_CHUNK_BYTES];
    let mut offset = resume_from;
    let mut chunk_index = resume_from / FILE_TRANSFER_CHUNK_BYTES as u64;
    let mut last_progress = Instant::now();
    loop {
        let read = file_handle
            .read(&mut buffer)
            .map_err(|error| format!("读取文件 {} 失败: {error}", file.path.display()))?;
        if read == 0 {
            break;
        }
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err("用户已取消传输".into());
        }
        let data = buffer[..read].to_vec();
        hasher.update(&data);
        send_file_transfer_packet(
            quic_transport,
            target,
            file_transfer_packet(
                "chunk",
                transfer_id,
                origin_id,
                target,
                &file.name,
                file.total_bytes,
                chunk_index,
                offset,
                data,
                drop_mode,
            ),
            cancel,
        )?;
        packet_count += 1;
        offset = offset.saturating_add(read as u64);
        chunk_index = chunk_index.saturating_add(1);
        if let Some(reporter) = reporter {
            if last_progress.elapsed() >= Duration::from_millis(FILE_TRANSFER_PROGRESS_INTERVAL_MS) {
                reporter.emit(transfer_id, file, offset, false, None);
                last_progress = Instant::now();
            }
        }
    }

    // The finish packet carries the whole-file digest so the receiver can
    // verify integrity before moving the file into place.
    let mut finish_packet = file_transfer_packet(
        "finish",
        transfer_id,
        origin_id,
        target,
        &file.name,
        file.total_bytes,
        chunk_index,
        offset,
        Vec::new(),
        drop_mode,
    );
    finish_packet.file_sha256 = hasher.finish().as_ref().to_vec();
    send_file_transfer_packet(quic_transport, target, finish_packet, cancel)?;
    packet_count += 1;

    Ok(packet_count)
}

// Where a transfer should land on the receiver.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DropMode {
    /// The MyKVM Transfers folder (the manual "send files" button).
    TransfersFolder,
    /// Straight onto the Desktop (edge drag-drop onto a non-Windows machine).
    Desktop,
    /// ShareMouse-style: stage, then drop into the folder under the cursor when
    /// the drag is released (else Desktop).
    DragDrop,
    /// A device's log fetched on request; lands in the "MyKVM Remote Logs" folder.
    ClientLog,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn file_transfer_packet(
    kind: &str,
    transfer_id: &str,
    origin_id: &str,
    target: &FileTransferTarget,
    file_name: &str,
    total_bytes: u64,
    chunk_index: u64,
    offset: u64,
    data: Vec<u8>,
    drop_mode: DropMode,
) -> FileTransferPacket {
    FileTransferPacket {
        protocol: FILE_TRANSFER_PROTOCOL.into(),
        kind: kind.into(),
        transfer_id: transfer_id.into(),
        origin_id: origin_id.into(),
        target_id: target.device_id.clone(),
        cluster_id: target.cluster_id.clone(),
        pair_secret: target.pair_secret.clone(),
        file_name: file_name.into(),
        total_bytes,
        chunk_index,
        offset,
        data,
        drop_to_desktop: drop_mode == DropMode::Desktop,
        drag_drop: drop_mode == DropMode::DragDrop,
        client_log: drop_mode == DropMode::ClientLog,
        file_sha256: Vec::new(),
        resume_from: 0,
    }
}

/// Send the "start" packet and parse the receiver's reply: "ok" (fresh) or
/// "ok:<offset>" (the receiver kept a .part from an interrupted attempt).
/// Retries cover a lost ACK — the receiver tolerates duplicated starts.
pub(crate) fn send_file_transfer_start(
    quic_transport: &quic_transport::TransportHandle,
    target: &FileTransferTarget,
    start_packet: FileTransferPacket,
    cancel: Option<&AtomicBool>,
) -> Result<u64, String> {
    let payload = encode_wire_packet(&start_packet)?;
    let peer = quic_transport.peer(
        target.addr.clone(),
        target.transport_public_key.clone(),
        target.protocol_version,
    );
    let cancelled = || cancel.is_some_and(|flag| flag.load(Ordering::Relaxed));
    let mut last_error = String::new();
    for attempt in 0..FILE_TRANSFER_SEND_ATTEMPTS {
        if cancelled() {
            return Err("用户已取消传输".into());
        }
        if attempt > 0 {
            thread::sleep(Duration::from_millis(FILE_TRANSFER_RETRY_DELAY_MS));
        }
        match quic_transport.send_stream_expect_ack_reply(peer.clone(), payload.clone()) {
            Ok(reply) => {
                let reply_str = String::from_utf8_lossy(&reply);
                if let Some(offset) = reply_str
                    .strip_prefix("ok:")
                    .and_then(|offset| offset.trim().parse::<u64>().ok())
                {
                    return Ok(offset);
                }
                return Ok(0);
            }
            Err(error) => last_error = error,
        }
    }
    Err(format!("文件传输失败: {last_error}"))
}

pub(crate) fn send_file_transfer_packet(
    quic_transport: &quic_transport::TransportHandle,
    target: &FileTransferTarget,
    packet: FileTransferPacket,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    let payload = encode_wire_packet(&packet)?;
    let peer = quic_transport.peer(
        target.addr.clone(),
        target.transport_public_key.clone(),
        target.protocol_version,
    );
    // A packet that actually landed but whose ACK was lost is safe to resend:
    // the receiver accepts duplicated chunks as success, and the finish packet
    // carries a SHA-256 that catches any content divergence.
    let cancelled = || cancel.is_some_and(|flag| flag.load(Ordering::Relaxed));
    let mut last_error = String::new();
    for attempt in 0..FILE_TRANSFER_SEND_ATTEMPTS {
        if cancelled() {
            return Err("用户已取消传输".into());
        }
        if attempt > 0 {
            thread::sleep(Duration::from_millis(FILE_TRANSFER_RETRY_DELAY_MS));
        }
        match quic_transport.send_stream_expect_ack(peer.clone(), payload.clone()) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    Err(format!("文件传输失败: {last_error}"))
}

pub(crate) fn handle_file_transfer_packet(
    payload: &[u8],
    layout: &LayoutState,
    local_peer_id: &str,
    transfers: &Arc<Mutex<HashMap<String, IncomingFileTransfer>>>,
    app: &AppHandle,
) -> bool {
    let Some(packet) = decode_wire_packet::<FileTransferPacket>(payload) else {
        return false;
    };
    let Ok(receive_root) = file_transfer_receive_root(app) else {
        log::warn!("file transfer receive failed: could not resolve receive directory");
        return false;
    };
    // Where the file finalizes:
    //  - drag_drop → a hidden staging dir; the real placement (into the folder
    //    under the cursor, or Desktop) happens when the drag is released.
    //  - drop_to_desktop → straight onto the Desktop.
    //  - otherwise → the transfers folder (None here).
    let drop_root = if packet.client_log {
        client_log_dir(app).ok()
    } else if packet.drag_drop {
        Some(receive_root.join(".mykvm-drag-staging"))
    } else if packet.drop_to_desktop {
        app.path().desktop_dir().ok()
    } else {
        None
    };
    handle_decoded_file_transfer_packet(
        packet,
        layout,
        local_peer_id,
        transfers,
        &receive_root,
        drop_root.as_deref(),
    )
}

pub(crate) fn file_transfer_receive_root(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(downloads) = app.path().download_dir() {
        return Ok(downloads.join("MyKVM Transfers"));
    }

    app.path()
        .app_data_dir()
        .map(|directory| directory.join("MyKVM Transfers"))
        .map_err(|error| format!("failed to resolve file transfer receive directory: {error}"))
}

#[cfg(test)]
pub(crate) fn handle_file_transfer_packet_with_root(
    payload: &[u8],
    layout: &LayoutState,
    local_peer_id: &str,
    transfers: &Arc<Mutex<HashMap<String, IncomingFileTransfer>>>,
    receive_root: &Path,
) -> bool {
    let Some(packet) = decode_wire_packet::<FileTransferPacket>(payload) else {
        return false;
    };
    handle_decoded_file_transfer_packet(packet, layout, local_peer_id, transfers, receive_root, None)
}

pub(crate) fn handle_decoded_file_transfer_packet(
    packet: FileTransferPacket,
    layout: &LayoutState,
    local_peer_id: &str,
    transfers: &Arc<Mutex<HashMap<String, IncomingFileTransfer>>>,
    receive_root: &Path,
    drop_root: Option<&Path>,
) -> bool {
    if packet.protocol != FILE_TRANSFER_PROTOCOL {
        return false;
    }
    if !layout.file_transfer_enabled {
        return false;
    }
    if !file_transfer_packet_authorized(layout, &packet) {
        return false;
    }
    if !file_transfer_packet_targets_local(layout, &packet, local_peer_id) {
        return false;
    }
    if packet.origin_id == local_peer_id {
        return true;
    }

    // A file that belongs to an active native drag session feeds that session's
    // in-memory stream (read by the OLE drop target) instead of landing on disk.
    #[cfg(target_os = "windows")]
    if crate::windows_drag::session_wants(&packet.transfer_id) {
        return match packet.kind.as_str() {
            "start" => true,
            "chunk" => {
                crate::windows_drag::feed_chunk(&packet.transfer_id, packet.offset, &packet.data)
            }
            "finish" => crate::windows_drag::finish_file(&packet.transfer_id),
            _ => false,
        };
    }

    match packet.kind.as_str() {
        "start" => start_incoming_file_transfer(packet, transfers, receive_root, drop_root),
        "chunk" => append_incoming_file_transfer_chunk(packet, transfers),
        "finish" => finish_incoming_file_transfer(packet, transfers),
        _ => false,
    }
}

pub(crate) fn start_incoming_file_transfer(
    packet: FileTransferPacket,
    transfers: &Arc<Mutex<HashMap<String, IncomingFileTransfer>>>,
    receive_root: &Path,
    drop_root: Option<&Path>,
) -> bool {
    let resumed = start_incoming_file_transfer_resumed(
        packet,
        transfers,
        receive_root,
        drop_root,
    );
    // Resume negotiation is read by the stream arm right after a "start" is
    // accepted: Some(offset) answers "ok:<offset>", None answers "ok". Every
    // accepted start refreshes the slot so a stale offset can never leak into
    // the next transfer.
    *FILE_RESUME_OFFER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = resumed;
    resumed.is_some()
}

/// The offset a just-accepted "start" will resume from (None = fresh/0).
static FILE_RESUME_OFFER: Mutex<Option<u64>> = Mutex::new(None);

/// The offset a just-accepted "start" will resume from (None = fresh/0);
/// consumed by the stream arm right after the packet is accepted.
pub(crate) fn take_file_resume_offer() -> Option<u64> {
    FILE_RESUME_OFFER
        .lock()
        .ok()
        .and_then(|mut offer| offer.take())
}

pub(crate) fn start_incoming_file_transfer_resumed(
    packet: FileTransferPacket,
    transfers: &Arc<Mutex<HashMap<String, IncomingFileTransfer>>>,
    receive_root: &Path,
    drop_root: Option<&Path>,
) -> Option<u64> {
    if packet.transfer_id.trim().is_empty()
        || packet.origin_id.trim().is_empty()
        || packet.total_bytes > FILE_TRANSFER_MAX_FILE_BYTES
        || !packet.data.is_empty()
    {
        return None;
    }

    let Some(file_name) = sanitize_transfer_file_name(&packet.file_name) else {
        return None;
    };

    if let Err(error) = fs::create_dir_all(receive_root) {
        log::warn!(
            "file transfer receive failed: could not create {}: {error}",
            receive_root.display()
        );
        return None;
    }

    // The finalize root overrides the transfers folder (Desktop, or the hidden
    // drag-staging dir). The .part staging file always stays in the transfers
    // folder so it never flashes at the final location.
    let final_root = drop_root.unwrap_or(receive_root);
    if drop_root.is_some() {
        let _ = fs::create_dir_all(final_root);
    }
    let final_path = unique_transfer_destination(final_root, &file_name);
    let temp_path = receive_root.join(format!(
        ".mykvm-{}-{}.part",
        sanitize_transfer_id(&packet.transfer_id),
        file_name
    ));

    // Interrupted-Transfer recovery: a .part left by an earlier attempt of the
    // SAME file (name + size match, at least one chunk, not overfull) is
    // adopted and the transfer continues from its length instead of starting
    // over. Other leftovers stay put: adopting a stale prefix can only ever
    // end in the finish-time SHA-256 rejecting the transfer, and deleting
    // eagerly risks racing a concurrent transfer of the same file name.
    let mut resume_from = 0_u64;
    let adoptable = find_resumable_part(receive_root, &file_name, packet.total_bytes);
    if let Some(old_part) = adoptable {
        if fs::rename(&old_part, &temp_path).is_err() {
            if fs::copy(&old_part, &temp_path).is_err() {
                return None;
            }
            let _ = fs::remove_file(&old_part);
        }
        resume_from = fs::metadata(&temp_path).map(|meta| meta.len()).unwrap_or(0);
        log::info!(
            "file transfer resuming {} at offset {} ({} bytes kept)",
            packet.transfer_id,
            resume_from,
            resume_from
        );
    }

    if let Ok(mut transfers) = transfers.lock() {
        if !transfers.contains_key(&packet.transfer_id)
            && transfers.len() >= MAX_CONCURRENT_INCOMING_TRANSFERS
        {
            log::warn!(
                "file transfer start rejected: {} transfers already in flight",
                transfers.len()
            );
            return None;
        }
        if let Some(previous) = transfers.remove(&packet.transfer_id) {
            let _ = fs::remove_file(previous.temp_path);
        }
    } else {
        return None;
    }

    if resume_from == 0 && fs::File::create(&temp_path).is_err() {
        return None;
    }

    // A ShareMouse-style drag: hold the file for release-time placement.
    #[cfg(target_os = "macos")]
    if packet.drag_drop {
        crate::drag_place::begin(&packet.file_name);
    }

    let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
    // Feed the adopted prefix so the finish-time digest covers the whole file.
    if resume_from > 0 {
        let Ok(mut part) = fs::File::open(&temp_path) else {
            let _ = fs::remove_file(&temp_path);
            return None;
        };
        let mut fed = 0_u64;
        let mut feed = vec![0_u8; FILE_TRANSFER_CHUNK_BYTES];
        while fed < resume_from {
            let want = ((resume_from - fed) as usize).min(feed.len());
            match part.read(&mut feed[..want]) {
                Ok(0) => break,
                Ok(read) => {
                    hasher.update(&feed[..read]);
                    fed += read as u64;
                }
                Err(_) => {
                    let _ = fs::remove_file(&temp_path);
                    return None;
                }
            }
        }
        if fed != resume_from {
            let _ = fs::remove_file(&temp_path);
            return None;
        }
    }

    let transfer = IncomingFileTransfer {
        origin_id: packet.origin_id.clone(),
        target_id: packet.target_id.clone(),
        file_name,
        total_bytes: packet.total_bytes,
        received_bytes: resume_from,
        next_chunk_index: resume_from / FILE_TRANSFER_CHUNK_BYTES as u64,
        temp_path,
        final_path,
        staged: packet.drag_drop,
        sha256: Some(hasher),
    };

    transfers
        .lock()
        .map(|mut transfers| {
            transfers.insert(packet.transfer_id, transfer);
            Some(resume_from)
        })
        .unwrap_or(None)
}

/// A kept .part from an interrupted attempt of the same file: name suffix and
/// total size must match, at least one chunk received, never overfull.
pub(crate) fn find_resumable_part(
    receive_root: &Path,
    file_name: &str,
    total_bytes: u64,
) -> Option<PathBuf> {
    let suffix = format!("-{}.part", file_name);
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in fs::read_dir(receive_root).ok()? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy().to_string();
        if !name.starts_with(".mykvm-") || !name.ends_with(&suffix) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let len = meta.len();
        if len == 0 || len > total_bytes {
            continue;
        }
        if best.as_ref().is_none_or(|(best_len, _)| len > *best_len) {
            best = Some((len, path));
        }
    }
    best.map(|(_, path)| path)
}

pub(crate) fn append_incoming_file_transfer_chunk(
    packet: FileTransferPacket,
    transfers: &Arc<Mutex<HashMap<String, IncomingFileTransfer>>>,
) -> bool {
    if packet.data.is_empty() || packet.data.len() > FILE_TRANSFER_CHUNK_BYTES {
        return false;
    }
    let Ok(mut transfers) = transfers.lock() else {
        return false;
    };
    let Some(transfer) = transfers.get_mut(&packet.transfer_id) else {
        return false;
    };
    if packet.origin_id != transfer.origin_id
        || packet.target_id != transfer.target_id
        || packet.file_name != transfer.file_name
        || packet.total_bytes != transfer.total_bytes
    {
        return false;
    }
    if packet.chunk_index < transfer.next_chunk_index
        && packet
            .offset
            .saturating_add(packet.data.len() as u64)
            <= transfer.received_bytes
    {
        // A retried packet whose original already landed (its ACK was lost).
        // Accepting the duplicate as success is safe: bytes for that range are
        // already on disk, and the finish packet's SHA-256 catches any content
        // divergence between the copies.
        return true;
    }
    if packet.chunk_index != transfer.next_chunk_index
        || packet.offset != transfer.received_bytes
        || transfer
            .received_bytes
            .saturating_add(packet.data.len() as u64)
            > transfer.total_bytes
    {
        return false;
    }

    let write_result = fs::OpenOptions::new()
        .append(true)
        .open(&transfer.temp_path)
        .and_then(|mut file| file.write_all(&packet.data));
    if let Err(error) = write_result {
        log::warn!("file transfer chunk write failed: {error}");
        return false;
    }
    if let Some(hasher) = transfer.sha256.as_mut() {
        hasher.update(&packet.data);
    }

    transfer.received_bytes = transfer
        .received_bytes
        .saturating_add(packet.data.len() as u64);
    transfer.next_chunk_index = transfer.next_chunk_index.saturating_add(1);
    true
}

pub(crate) fn finish_incoming_file_transfer(
    packet: FileTransferPacket,
    transfers: &Arc<Mutex<HashMap<String, IncomingFileTransfer>>>,
) -> bool {
    if !packet.data.is_empty() {
        return false;
    }
    let (temp_path, final_path, file_name, total_bytes, staged, received_digest) = {
        let Ok(mut transfers) = transfers.lock() else {
            return false;
        };
        let Some(transfer) = transfers.get_mut(&packet.transfer_id) else {
            return false;
        };
        if packet.origin_id != transfer.origin_id
            || packet.target_id != transfer.target_id
            || packet.file_name != transfer.file_name
            || packet.total_bytes != transfer.total_bytes
            || packet.offset != transfer.received_bytes
            || packet.chunk_index != transfer.next_chunk_index
            || transfer.received_bytes != transfer.total_bytes
        {
            return false;
        }
        (
            transfer.temp_path.clone(),
            transfer.final_path.clone(),
            transfer.file_name.clone(),
            transfer.total_bytes,
            transfer.staged,
            transfer.sha256.take().map(|hasher| hasher.finish()),
        )
    };

    // New senders attach the whole-file digest; verify it before the file is
    // placed. A mismatch means the transfer is corrupt — drop it entirely
    // rather than landing a broken file at the final location.
    if !packet.file_sha256.is_empty()
        && received_digest
            .map(|digest| digest.as_ref() != packet.file_sha256.as_slice())
            .unwrap_or(true)
    {
        log::error!(
            "file transfer {} integrity check failed: dropping {}",
            packet.transfer_id,
            file_name
        );
        let _ = fs::remove_file(&temp_path);
        if let Ok(mut transfers) = transfers.lock() {
            transfers.remove(&packet.transfer_id);
        }
        record_transfer_history(TransferHistoryEntry {
            id: 0,
            direction: "receive".into(),
            device_id: packet.origin_id.clone(),
            device_name: packet.origin_id.clone(),
            file_name,
            file_count: 1,
            total_bytes,
            ok: false,
            error: Some("SHA-256 校验失败，已丢弃".into()),
            at_ms: 0,
            paths: Vec::new(),
        });
        return false;
    }

    // The staging dir and the final dir can sit on different volumes (e.g. a
    // redirected Desktop), where rename fails — fall back to copy + delete.
    let finalize = fs::rename(&temp_path, &final_path).or_else(|_| {
        fs::copy(&temp_path, &final_path).map(|_| {
            let _ = fs::remove_file(&temp_path);
        })
    });
    match finalize {
        Ok(()) => {
            if let Ok(mut transfers) = transfers.lock() {
                transfers.remove(&packet.transfer_id);
            }
            // ShareMouse-style drag: the file is complete in the staging dir;
            // hand it to the placer, which drops it into the folder under the
            // cursor when the drag is released (or right away if already).
            #[cfg(target_os = "macos")]
            if staged {
                crate::drag_place::stage(final_path.clone());
                return true;
            }
            // Windows: a staged file that nobody will place (DragDrop mode is
            // only staged here when no native drag session consumed it — e.g.
            // an edge-drag from a Windows controller) must not rot in the
            // hidden staging dir; move it up into the visible transfers root.
            #[cfg(target_os = "windows")]
            if staged {
                let visible = final_path
                    .parent()
                    .and_then(|staging| staging.parent())
                    .map(|root| unique_transfer_destination(root, &file_name));
                if let Some(visible) = visible {
                    match fs::rename(&final_path, &visible).or_else(|_| {
                        fs::copy(&final_path, &visible).map(|_| {
                            let _ = fs::remove_file(&final_path);
                        })
                    }) {
                        Ok(()) => {
                            log::info!(
                                "received file transfer {} bytes={} path={}",
                                file_name,
                                total_bytes,
                                visible.display()
                            );
                            return true;
                        }
                        Err(error) => {
                            log::warn!("failed to move staged drag file: {error}");
                        }
                    }
                }
            }
            let _ = staged;
            log::info!(
                "received file transfer {} bytes={} path={}",
                file_name,
                total_bytes,
                final_path.display()
            );
            record_transfer_history(TransferHistoryEntry {
                id: 0,
                direction: "receive".into(),
                device_id: packet.origin_id.clone(),
                device_name: packet.origin_id.clone(),
                file_name,
                file_count: 1,
                total_bytes,
                ok: true,
                error: None,
                at_ms: 0,
                paths: vec![final_path.display().to_string()],
            });
            true
        }
        Err(error) => {
            log::warn!("file transfer finalize failed: {error}");
            record_transfer_history(TransferHistoryEntry {
                id: 0,
                direction: "receive".into(),
                device_id: packet.origin_id.clone(),
                device_name: packet.origin_id.clone(),
                file_name,
                file_count: 1,
                total_bytes,
                ok: false,
                error: Some(error.to_string()),
                at_ms: 0,
                paths: Vec::new(),
            });
            false
        }
    }
}

pub(crate) fn file_transfer_packet_authorized(layout: &LayoutState, packet: &FileTransferPacket) -> bool {
    if layout.cluster_id.trim().is_empty()
        || layout.pair_secret.trim().is_empty()
        || packet.cluster_id != layout.cluster_id
    {
        return false;
    }

    if role_receives_from_peers(&layout.machine_role) {
        let matched = layout
            .paired_controllers
            .iter()
            .find(|controller| controller.id == packet.origin_id);
        if let Some(controller) = matched {
            // LRU bookkeeping for the whitelist cap: authorization is a use.
            touch_paired_controller_usage(&controller.transport_public_key, &controller.id);
            return true;
        }
        return packet.pair_secret == layout.pair_secret;
    }

    true
}

pub(crate) fn file_transfer_packet_targets_local(
    layout: &LayoutState,
    packet: &FileTransferPacket,
    local_peer_id: &str,
) -> bool {
    if packet.target_id.trim().is_empty() || packet.target_id == local_peer_id {
        return true;
    }

    layout
        .devices
        .iter()
        .any(|device| device.role == "local" && device.id == packet.target_id)
}

pub(crate) fn sanitize_transfer_file_name(name: &str) -> Option<String> {
    let mut output = String::with_capacity(name.len().min(180));
    for character in name.trim().chars() {
        let safe = match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        };
        output.push(safe);
        if output.len() >= 180 {
            break;
        }
    }
    let output = output.trim().trim_matches('.').trim().to_string();
    if output.is_empty() || output == "." || output == ".." {
        None
    } else {
        Some(output)
    }
}

pub(crate) fn sanitize_transfer_id(transfer_id: &str) -> String {
    let id = transfer_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .take(80)
        .collect::<String>();
    if id.is_empty() {
        random_hex(8)
    } else {
        id
    }
}

pub(crate) fn unique_transfer_destination(directory: &Path, file_name: &str) -> PathBuf {
    let first = directory.join(file_name);
    if !first.exists() {
        return first;
    }

    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "file".into());
    let extension = path
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy()))
        .unwrap_or_default();

    for index in 1..10_000 {
        let candidate = directory.join(format!("{stem} ({index}){extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }

    directory.join(format!("{stem}-{}{extension}", random_hex(4)))
}
