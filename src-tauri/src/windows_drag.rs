//! Native OLE drag-drop on the controlled Windows client.
//!
//! When a file drag on the controlling Mac crosses onto this machine, the Mac
//! sends a drag-start control message plus the file bytes; this module runs a
//! real `DoDragDrop` session so Windows renders the native drag image and any
//! drop target (an Explorer folder, WeChat, a mail client, …) accepts the drop.
//! The virtual files are exposed through an `IDataObject`; each file's bytes are
//! served by an `IStream` backed by a buffer the transfer path fills, so the
//! drop target reads each file as it streams in.
//!
//! DROP TIMING is decided by our own `IDropSource` from flags this module sets
//! (`signal_drop` / `cancel_session`), not from the physical button — the drag
//! is driven by injected input, so there is no real button to watch. A synthetic
//! left-button down/up pair brackets the session so `DoDragDrop` treats it as a
//! real drag and so the terminal button-up wakes `QueryContinueDrag`.
//!
//! NOTE: this cannot be exercised on the macOS build host. It type-checks for
//! the Windows target (cargo xwin) but its runtime behavior — the modal
//! DoDragDrop loop driven by injected input and the streaming IStream read by
//! the drop target — needs verification on real Windows hardware.
#![cfg(target_os = "windows")]

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{implement, PCWSTR};
use windows::Win32::Foundation::{
    DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
    DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED, E_ABORT, E_NOTIMPL, HGLOBAL, OLE_E_ADVISENOTSUPPORTED,
    S_OK, STG_E_ACCESSDENIED,
};
use windows::Win32::System::Com::{
    IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, ISequentialStream_Impl,
    IStream, IStream_Impl, DVASPECT_CONTENT, FORMATETC, LOCKTYPE, STATFLAG, STATSTG, STGC, STGMEDIUM,
    STGMEDIUM_0, STGTY_STREAM, STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET,
    TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, DROPEFFECT,
    DROPEFFECT_COPY,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{
    IDataObjectAsyncCapability, IDataObjectAsyncCapability_Impl, SHCreateStdEnumFmtEtc,
    FD_FILESIZE, FILEDESCRIPTORW,
};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEINPUT, VK_LBUTTON,
};

/// Files whose transfer feeds a drag session. `transfer_id` matches the id the
/// controller stamps on the file-transfer packets it streams for this drag.
pub struct DragFileMeta {
    pub transfer_id: String,
    pub name: String,
    pub size: u64,
}

// One in-flight drag at a time (there is one cursor), so a single global slot.
static ACTIVE_SESSION: OnceLock<Mutex<Option<Arc<DragSession>>>> = OnceLock::new();

fn session_slot() -> &'static Mutex<Option<Arc<DragSession>>> {
    ACTIVE_SESSION.get_or_init(|| Mutex::new(None))
}

fn active_session() -> Option<Arc<DragSession>> {
    session_slot().lock().ok().and_then(|slot| slot.clone())
}

/// Dropped sessions whose target still extracts asynchronously: their files
/// keep streaming into these buffers after `DoDragDrop` returned.
static FINISHING: Mutex<Vec<Arc<DragSession>>> = Mutex::new(Vec::new());

/// The dragging or finishing session that owns `transfer_id`.
fn session_for(transfer_id: &str) -> Option<Arc<DragSession>> {
    active_session()
        .filter(|session| session.by_transfer_id.contains_key(transfer_id))
        .or_else(|| {
            FINISHING.lock().ok().and_then(|finishing| {
                finishing
                    .iter()
                    .find(|session| session.by_transfer_id.contains_key(transfer_id))
                    .cloned()
            })
        })
}

/// Clears the slot only if it still holds `session` (a newer drag may own it).
fn clear_session(session: &Arc<DragSession>) {
    if let Ok(mut slot) = session_slot().lock() {
        if slot.as_ref().is_some_and(|active| Arc::ptr_eq(active, session)) {
            *slot = None;
        }
    }
}

struct DragSession {
    order: Vec<Arc<FileBuffer>>,
    by_transfer_id: HashMap<String, Arc<FileBuffer>>,
    // Drop/cancel decided here and read by the IDropSource.
    released: Mutex<bool>,
    cancelled: Mutex<bool>,
    // The drag rides a real physical button (this machine is the controller and
    // the file came from the far side): QueryContinueDrag honors the physical
    // Escape/button and no synthetic button bracket is injected. False = the
    // original injected/remote drag driven by signal_drop/cancel_session.
    physical: bool,
    // The drop target extracts on its own thread (StartOperation) and reports
    // the end (EndOperation), after DoDragDrop has already returned.
    async_started: AtomicBool,
    operation_done: AtomicBool,
    // Start or last file bytes; a drag with nothing happening is abandoned.
    last_activity: Mutex<Instant>,
}

impl DragSession {
    fn touch(&self) {
        if let Ok(mut last) = self.last_activity.lock() {
            *last = Instant::now();
        }
    }

    fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .map(|last| last.elapsed())
            .unwrap_or_default()
    }

    /// Fraction of all file bytes received so far.
    fn progress(&self) -> f32 {
        let total: u64 = self.order.iter().map(|file| file.size).sum();
        if total == 0 {
            return 1.0;
        }
        let received: u64 = self.order.iter().map(|file| file.received()).sum();
        (received as f64 / total as f64).min(1.0) as f32
    }

    fn is_released(&self) -> bool {
        self.released.lock().map(|v| *v).unwrap_or(true)
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.lock().map(|v| *v).unwrap_or(true)
    }
}

/// Growable buffer backing one file's `IStream`. The transfer thread appends
/// bytes and marks completion; the drop target's stream read blocks here until
/// enough bytes have arrived (or the session is aborted).
struct FileBuffer {
    name: String,
    size: u64,
    state: Mutex<FileBufferState>,
    cond: Condvar,
}

struct FileBufferState {
    data: Vec<u8>,
    // Contiguous bytes accepted so far. Chunks are strictly ordered; a resend
    // whose range is already covered is dropped as a duplicate, and a gap is
    // rejected so the transfer aborts instead of corrupting the stream.
    received_bytes: u64,
    complete: bool,
    aborted: bool,
}

impl FileBuffer {
    fn new(name: String, size: u64) -> Self {
        Self {
            name,
            size,
            state: Mutex::new(FileBufferState {
                data: Vec::new(),
                received_bytes: 0,
                complete: false,
                aborted: false,
            }),
            cond: Condvar::new(),
        }
    }

    /// Append the next chunk at `offset`. Returns `false` for a gap (the
    /// transfer is corrupt — the sender gives up). Duplicates (a resend whose
    /// ACK was lost) are accepted as success without writing a second copy.
    fn append_at(&self, offset: u64, bytes: &[u8]) -> bool {
        let accepted = if let Ok(mut state) = self.state.lock() {
            let end = offset.saturating_add(bytes.len() as u64);
            if offset < state.received_bytes && end <= state.received_bytes {
                // Duplicate: already on the stream.
                true
            } else if offset == state.received_bytes {
                state.data.extend_from_slice(bytes);
                state.received_bytes = end;
                true
            } else {
                // Gap or overlap beyond the received prefix.
                false
            }
        } else {
            false
        };
        if accepted {
            self.cond.notify_all();
        }
        accepted
    }

    fn finish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.complete = true;
        }
        self.cond.notify_all();
    }

    fn abort(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.aborted = true;
        }
        self.cond.notify_all();
    }

    fn received(&self) -> u64 {
        self.state
            .lock()
            .map(|state| if state.complete { self.size } else { state.data.len() as u64 })
            .unwrap_or(0)
    }

    /// Copy up to `out.len()` bytes starting at `pos`, blocking until that many
    /// bytes exist, the file is complete, or the session aborts. Returns the
    /// number of bytes copied (0 = end of stream). `Err` means aborted/stalled.
    fn read_at(&self, pos: u64, out: &mut [u8]) -> Result<usize, ()> {
        let Ok(mut state) = self.state.lock() else {
            return Err(());
        };
        loop {
            if state.aborted {
                return Err(());
            }
            if pos >= self.size {
                return Ok(0);
            }
            let available = state.data.len() as u64;
            if pos < available {
                let start = pos as usize;
                let n = out.len().min((available - pos) as usize);
                out[..n].copy_from_slice(&state.data[start..start + n]);
                return Ok(n);
            }
            // No bytes yet at this position.
            if state.complete {
                return Ok(0);
            }
            // Bounded wait so a wedged transfer can never hang the drop target.
            let (next, timeout) = self
                .cond
                .wait_timeout(state, Duration::from_secs(30))
                .map_err(|_| ())?;
            state = next;
            if timeout.timed_out() && state.data.len() as u64 <= pos && !state.complete {
                return Err(());
            }
        }
    }
}

// --- public API called by the transfer/receive path -----------------------

/// True while `transfer_id` belongs to the active drag session, so the transfer
/// handler feeds its bytes here instead of writing them to disk.
pub fn session_wants(transfer_id: &str) -> bool {
    session_for(transfer_id).is_some()
}

pub fn feed_chunk(transfer_id: &str, offset: u64, data: &[u8]) -> bool {
    let Some(session) = session_for(transfer_id) else {
        return false;
    };
    let Some(buffer) = session.by_transfer_id.get(transfer_id) else {
        return false;
    };
    let accepted = buffer.append_at(offset, data);
    if accepted {
        session.touch();
    }
    accepted
}

pub fn finish_file(transfer_id: &str) -> bool {
    let Some(session) = session_for(transfer_id) else {
        return false;
    };
    let Some(buffer) = session.by_transfer_id.get(transfer_id) else {
        return false;
    };
    buffer.finish();
    session.touch();
    true
}

/// The controller released the drag over this machine: perform the drop.
pub fn signal_drop() {
    if let Some(session) = active_session() {
        if let Ok(mut released) = session.released.lock() {
            *released = true;
        }
    } else {
        log::warn!("native drag drop signalled with no session in flight");
    }
    // The synthetic button-up both balances the start-of-drag down and wakes
    // DoDragDrop's QueryContinueDrag so it observes the released flag.
    inject_left_button(false);
}

/// The controller left without dropping (or capture stopped): abort the drag.
pub fn cancel_session() {
    if let Some(session) = active_session() {
        if let Ok(mut cancelled) = session.cancelled.lock() {
            *cancelled = true;
        }
        for buffer in &session.order {
            buffer.abort();
        }
    }
    inject_left_button(false);
}

/// Begin a drag session and spawn the DoDragDrop thread. Returns false if a
/// drag is already in flight or `files` is empty.
pub fn start_drag_session(files: Vec<DragFileMeta>) -> bool {
    if files.is_empty() {
        return false;
    }
    // One drag at a time, and the controller starts one only after the last
    // ended, so a session still here is stale (its drop or cancel never came).
    // Refusing the new drag kept the stale one's synthetic button down, which
    // broke this machine's own drags and sent every later file to the
    // transfers folder. End it instead.
    if active_session().is_some() {
        log::warn!("native drag: ending a session that never finished");
        cancel_session();
        let deadline = Instant::now() + Duration::from_secs(2);
        while active_session().is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let Ok(mut slot) = session_slot().lock() else {
        return false;
    };
    if slot.is_some() {
        log::warn!("native drag: the previous session did not end; refusing a new one");
        return false;
    }

    let mut order = Vec::with_capacity(files.len());
    let mut by_transfer_id = HashMap::with_capacity(files.len());
    for meta in files {
        let buffer = Arc::new(FileBuffer::new(meta.name, meta.size));
        by_transfer_id.insert(meta.transfer_id, Arc::clone(&buffer));
        order.push(buffer);
    }
    // If the physical left button is down, this machine is the controller and
    // the drag rides that real button — DoDragDrop tracks it to the physical
    // drop. Otherwise the drag is remote/injected and needs a synthetic button
    // bracket to start it and to end it (via signal_drop / cancel_session).
    let physical = unsafe { (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0 };

    let session = Arc::new(DragSession {
        order,
        by_transfer_id,
        released: Mutex::new(false),
        cancelled: Mutex::new(false),
        physical,
        async_started: AtomicBool::new(false),
        operation_done: AtomicBool::new(false),
        last_activity: Mutex::new(Instant::now()),
    });
    *slot = Some(Arc::clone(&session));
    drop(slot);

    // Nothing for a minute while still dragging (the controller vanished, or
    // its drop/cancel was lost): cancel, which also lifts the synthetic button.
    let watched = Arc::clone(&session);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let dragging = active_session().is_some_and(|active| Arc::ptr_eq(&active, &watched))
            && !watched.is_released()
            && !watched.is_cancelled();
        if !dragging {
            break;
        }
        if watched.idle_for() > Duration::from_secs(60) {
            log::warn!("native drag: no drop or cancel for a minute; cancelling");
            cancel_session();
            break;
        }
    });
    log::info!(
        "native drag session started ({} button)",
        if physical { "physical" } else { "synthetic" }
    );

    std::thread::spawn(move || {
        run_drag_thread(session);
    });
    true
}

// --- the DoDragDrop thread -------------------------------------------------

fn run_drag_thread(session: Arc<DragSession>) {
    unsafe {
        // DoDragDrop requires an OLE-initialized STA thread.
        let _ = OleInitialize(None);

        let streams: Vec<IStream> = session
            .order
            .iter()
            .map(|buffer| FileReadStream::new(Arc::clone(buffer)))
            .collect();

        let data_object: IDataObject = DragDataObject {
            files: session.order.clone(),
            streams,
            session: Arc::clone(&session),
        }
        .into();
        let drop_source: IDropSource = DragDropSource {
            session: Arc::clone(&session),
        }
        .into();

        let progress_session = Arc::clone(&session);
        let overlay = crate::windows_drag_overlay::start(
            session.order.first().map(|file| file.name.as_str()).unwrap_or(""),
            Arc::new(move || progress_session.progress()),
        );

        let mut effect = DROPEFFECT::default();
        let result = if session.physical {
            DoDragDrop(&data_object, &drop_source, DROPEFFECT_COPY, &mut effect)
        } else {
            drag_from_window_under_cursor(&data_object, &drop_source, &mut effect)
        };
        // The single most useful line when a drag "does nothing" on real
        // hardware: DRAGDROP_S_DROP means a target took it, DRAGDROP_S_CANCEL
        // means the loop ended without one, anything else is an OLE failure.
        log::info!(
            "native drag session ended: {} (hr=0x{:08X}, effect=0x{:X})",
            if result == DRAGDROP_S_DROP {
                "dropped"
            } else if result == DRAGDROP_S_CANCEL {
                "cancelled"
            } else {
                "failed"
            },
            result.0,
            effect.0
        );

        // Release the synthetic button (injected drags only — a physical drag's
        // button belongs to the user and is already up, which is why DoDragDrop
        // returned).
        if !session.physical {
            inject_left_button(false);
        }
        // An asynchronous target (Explorer) extracts after DoDragDrop returns.
        // Aborting the buffers here failed that copy, and the bytes still in
        // flight found no session and landed in the transfers folder: keep
        // feeding it until it reports the end, showing progress meanwhile.
        if result == DRAGDROP_S_DROP && session.async_started.load(Ordering::Relaxed) {
            overlay.dock();
            finish_async_drop(&session);
        }
        for buffer in &session.order {
            buffer.abort();
        }
        clear_session(&session);
        drop(overlay);
        OleUninitialize();
    }
}

/// Park a dropped session in FINISHING (freeing the slot for the next drag)
/// until its target reports EndOperation, or nothing has moved for a minute.
fn finish_async_drop(session: &Arc<DragSession>) {
    clear_session(session);
    if let Ok(mut finishing) = FINISHING.lock() {
        finishing.push(Arc::clone(session));
    }
    let started = Instant::now();
    while !session.operation_done.load(Ordering::Relaxed)
        && !session.is_cancelled()
        && session.idle_for() < Duration::from_secs(60)
        && started.elapsed() < Duration::from_secs(30 * 60)
    {
        std::thread::sleep(Duration::from_millis(200));
    }
    if let Ok(mut finishing) = FINISHING.lock() {
        finishing.retain(|other| !Arc::ptr_eq(other, session));
    }
}

/// OLE's drag loop follows the mouse through capture, which a thread only gets
/// for a press that lands on one of its own windows. Called from a thread with
/// no window under the cursor, DoDragDrop never followed the injected pointer
/// and the drag went nowhere. Put a tiny window of this thread under the
/// cursor, let the synthetic press land on it (which also keeps the press off
/// whatever app sits at the crossing edge), then start the drag.
unsafe fn drag_from_window_under_cursor(
    data_object: &IDataObject,
    drop_source: &IDropSource,
    effect: &mut DROPEFFECT,
) -> windows::core::HRESULT {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
        MsgWaitForMultipleObjects, PeekMessageW, RegisterClassW, ShowWindow, MSG, PM_REMOVE,
        QS_ALLINPUT, SW_HIDE, SW_SHOWNOACTIVATE, WM_LBUTTONDOWN, WNDCLASSW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    let instance = GetModuleHandleW(std::ptr::null());
    let class: Vec<u16> = "MyKVMDragSource".encode_utf16().chain(Some(0)).collect();
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(DefWindowProcW),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        ..std::mem::zeroed()
    };
    RegisterClassW(&window_class); // Fails harmlessly once already registered.
    let mut cursor = POINT { x: 0, y: 0 };
    GetCursorPos(&mut cursor);
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        class.as_ptr(),
        std::ptr::null(),
        WS_POPUP,
        cursor.x - 4,
        cursor.y - 4,
        8,
        8,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        instance,
        std::ptr::null(),
    );
    if !hwnd.is_null() {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    inject_left_button(true);

    // Wait (pumping this thread's queue) for the press to reach the window.
    let mut pressed = hwnd.is_null();
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut message: MSG = std::mem::zeroed();
    while !pressed && Instant::now() < deadline {
        while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            if message.message == WM_LBUTTONDOWN && message.hwnd == hwnd {
                pressed = true;
            }
            DispatchMessageW(&message);
        }
        if !pressed {
            MsgWaitForMultipleObjects(0, std::ptr::null(), 0, 10, QS_ALLINPUT);
        }
    }
    if !pressed {
        log::warn!("native drag: the synthetic press missed the drag window");
    }
    // Hidden, it can no longer shadow the drop target under the cursor.
    if !hwnd.is_null() {
        ShowWindow(hwnd, SW_HIDE);
    }
    let result = DoDragDrop(data_object, drop_source, DROPEFFECT_COPY, effect);
    if !hwnd.is_null() {
        DestroyWindow(hwnd);
    }
    result
}

// --- clipboard formats -----------------------------------------------------

struct DragFormats {
    file_descriptor: u16,
    file_contents: u16,
}

fn drag_formats() -> &'static DragFormats {
    static FORMATS: OnceLock<DragFormats> = OnceLock::new();
    FORMATS.get_or_init(|| unsafe {
        DragFormats {
            file_descriptor: RegisterClipboardFormatW(w("FileGroupDescriptorW")) as u16,
            file_contents: RegisterClipboardFormatW(w("FileContents")) as u16,
        }
    })
}

// PCWSTR from a &str literal — leaks a small NUL-terminated UTF-16 buffer once
// per distinct string; only called for the two fixed clipboard-format names.
fn w(value: &str) -> PCWSTR {
    let mut units: Vec<u16> = value.encode_utf16().collect();
    units.push(0);
    let boxed = units.into_boxed_slice();
    let ptr = boxed.as_ptr();
    std::mem::forget(boxed);
    PCWSTR(ptr)
}

// --- IStream over a FileBuffer --------------------------------------------

#[implement(IStream)]
struct FileReadStream {
    buffer: Arc<FileBuffer>,
    pos: Mutex<u64>,
}

impl FileReadStream {
    fn new(buffer: Arc<FileBuffer>) -> IStream {
        FileReadStream {
            buffer,
            pos: Mutex::new(0),
        }
        .into()
    }
}

impl ISequentialStream_Impl for FileReadStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> windows::core::HRESULT {
        let mut pos = match self.pos.lock() {
            Ok(pos) => pos,
            Err(_) => return E_NOTIMPL,
        };
        let out = unsafe { std::slice::from_raw_parts_mut(pv as *mut u8, cb as usize) };
        match self.buffer.read_at(*pos, out) {
            Ok(n) => {
                *pos += n as u64;
                if !pcbread.is_null() {
                    unsafe { *pcbread = n as u32 };
                }
                S_OK
            }
            Err(()) => {
                if !pcbread.is_null() {
                    unsafe { *pcbread = 0 };
                }
                E_ABORT
            }
        }
    }

    fn Write(
        &self,
        _pv: *const core::ffi::c_void,
        _cb: u32,
        _pcbwritten: *mut u32,
    ) -> windows::core::HRESULT {
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for FileReadStream_Impl {
    fn Seek(
        &self,
        dlibmove: i64,
        dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> windows::core::Result<()> {
        let mut pos = self.pos.lock().map_err(|_| windows::core::Error::from(E_NOTIMPL))?;
        let base = match dworigin {
            STREAM_SEEK_SET => 0i64,
            STREAM_SEEK_CUR => *pos as i64,
            STREAM_SEEK_END => self.buffer.size as i64,
            _ => return Err(windows::core::Error::from(E_NOTIMPL)),
        };
        let next = (base + dlibmove).max(0) as u64;
        *pos = next;
        if !plibnewposition.is_null() {
            unsafe { *plibnewposition = next };
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> windows::core::Result<()> {
        Err(windows::core::Error::from(STG_E_ACCESSDENIED))
    }

    fn CopyTo(
        &self,
        pstm: windows::core::Ref<'_, IStream>,
        cb: u64,
        pcbread: *mut u64,
        pcbwritten: *mut u64,
    ) -> windows::core::Result<()> {
        let target = pstm.ok()?;
        let mut remaining = cb;
        let mut total_read = 0u64;
        let mut total_written = 0u64;
        let mut chunk = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let want = remaining.min(chunk.len() as u64) as usize;
            let pos = *self.pos.lock().map_err(|_| windows::core::Error::from(E_NOTIMPL))?;
            let n = match self.buffer.read_at(pos, &mut chunk[..want]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(()) => return Err(windows::core::Error::from(E_ABORT)),
            };
            if let Ok(mut p) = self.pos.lock() {
                *p += n as u64;
            }
            total_read += n as u64;
            let mut written = 0u32;
            unsafe {
                target
                    .Write(chunk.as_ptr() as *const _, n as u32, Some(&mut written))
                    .ok()?;
            }
            total_written += written as u64;
            remaining -= n as u64;
        }
        if !pcbread.is_null() {
            unsafe { *pcbread = total_read };
        }
        if !pcbwritten.is_null() {
            unsafe { *pcbwritten = total_written };
        }
        Ok(())
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn LockRegion(
        &self,
        _liboffset: u64,
        _cb: u64,
        _dwlocktype: &LOCKTYPE,
    ) -> windows::core::Result<()> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> windows::core::Result<()> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn Stat(&self, pstatstg: *mut STATSTG, _grfstatflag: &STATFLAG) -> windows::core::Result<()> {
        if pstatstg.is_null() {
            return Err(windows::core::Error::from(E_NOTIMPL));
        }
        let mut stat = STATSTG::default();
        stat.r#type = STGTY_STREAM.0 as u32;
        stat.cbSize = self.buffer.size;
        unsafe { *pstatstg = stat };
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        let pos = *self.pos.lock().map_err(|_| windows::core::Error::from(E_NOTIMPL))?;
        let stream: IStream = FileReadStream {
            buffer: Arc::clone(&self.buffer),
            pos: Mutex::new(pos),
        }
        .into();
        Ok(stream)
    }
}

// --- IDataObject exposing the virtual files --------------------------------

#[implement(IDataObject, IDataObjectAsyncCapability)]
struct DragDataObject {
    files: Vec<Arc<FileBuffer>>,
    streams: Vec<IStream>,
    session: Arc<DragSession>,
}

impl DragDataObject_Impl {
    fn supported(&self, format: &FORMATETC) -> bool {
        let formats = drag_formats();
        if format.dwAspect != DVASPECT_CONTENT.0 {
            return false;
        }
        if format.cfFormat == formats.file_descriptor {
            format.tymed & TYMED_HGLOBAL.0 as u32 != 0
        } else if format.cfFormat == formats.file_contents {
            format.tymed & TYMED_ISTREAM.0 as u32 != 0
        } else {
            false
        }
    }
}

impl IDataObject_Impl for DragDataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let format = unsafe { pformatetcin.as_ref() }
            .ok_or_else(|| windows::core::Error::from(DV_E_FORMATETC))?;
        let formats = drag_formats();
        if format.dwAspect != DVASPECT_CONTENT.0 {
            return Err(windows::core::Error::from(DV_E_FORMATETC));
        }

        if format.cfFormat == formats.file_descriptor {
            if format.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
                return Err(windows::core::Error::from(DV_E_TYMED));
            }
            let hglobal = build_file_group_descriptor(&self.files)?;
            return Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: hglobal },
                pUnkForRelease: ManuallyDrop::new(None),
            });
        }

        if format.cfFormat == formats.file_contents {
            if format.tymed & TYMED_ISTREAM.0 as u32 == 0 {
                return Err(windows::core::Error::from(DV_E_TYMED));
            }
            let index = format.lindex;
            if index < 0 || index as usize >= self.streams.len() {
                return Err(windows::core::Error::from(DV_E_LINDEX));
            }
            let stream = self.streams[index as usize].clone();
            return Ok(STGMEDIUM {
                tymed: TYMED_ISTREAM.0 as u32,
                u: STGMEDIUM_0 {
                    pstm: ManuallyDrop::new(Some(stream)),
                },
                pUnkForRelease: ManuallyDrop::new(None),
            });
        }

        Err(windows::core::Error::from(DV_E_FORMATETC))
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> windows::core::HRESULT {
        match unsafe { pformatetc.as_ref() } {
            Some(format) if self.supported(format) => S_OK,
            _ => DV_E_FORMATETC,
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> windows::core::HRESULT {
        if !pformatetcout.is_null() {
            unsafe {
                (*pformatetcout).ptd = std::ptr::null_mut();
            }
        }
        DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: windows::core::BOOL,
    ) -> windows::core::Result<()> {
        // Drop targets may push a preferred-effect blob; accept and ignore it.
        Ok(())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        const DATADIR_GET: u32 = 1;
        if dwdirection != DATADIR_GET {
            return Err(windows::core::Error::from(E_NOTIMPL));
        }
        let formats = drag_formats();
        let entries = [
            FORMATETC {
                cfFormat: formats.file_descriptor,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            },
            FORMATETC {
                cfFormat: formats.file_contents,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_ISTREAM.0 as u32,
            },
        ];
        unsafe { SHCreateStdEnumFmtEtc(&entries) }
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: windows::core::Ref<'_, IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(windows::core::Error::from(
            OLE_E_ADVISENOTSUPPORTED,
        ))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(windows::core::Error::from(
            OLE_E_ADVISENOTSUPPORTED,
        ))
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(windows::core::Error::from(
            OLE_E_ADVISENOTSUPPORTED,
        ))
    }
}

impl IDataObjectAsyncCapability_Impl for DragDataObject_Impl {
    fn SetAsyncMode(&self, _fdoopasync: windows::core::BOOL) -> windows::core::Result<()> {
        Ok(())
    }

    fn GetAsyncMode(&self) -> windows::core::Result<windows::core::BOOL> {
        // Always async: our stream reads block on the network, so the drop
        // target must extract on a worker thread, not its UI thread.
        Ok(true.into())
    }

    fn StartOperation(
        &self,
        _pbcreserved: windows::core::Ref<'_, IBindCtx>,
    ) -> windows::core::Result<()> {
        self.session.async_started.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn InOperation(&self) -> windows::core::Result<windows::core::BOOL> {
        let running = self.session.async_started.load(Ordering::Relaxed)
            && !self.session.operation_done.load(Ordering::Relaxed);
        Ok(running.into())
    }

    fn EndOperation(
        &self,
        hresult: windows::core::HRESULT,
        _pbcreserved: windows::core::Ref<'_, IBindCtx>,
        _dweffects: u32,
    ) -> windows::core::Result<()> {
        log::info!("native drag: drop target finished extracting (hr=0x{:08X})", hresult.0);
        self.session.operation_done.store(true, Ordering::Relaxed);
        Ok(())
    }
}

fn build_file_group_descriptor(files: &[Arc<FileBuffer>]) -> windows::core::Result<HGLOBAL> {
    let count = files.len();
    let header = std::mem::size_of::<u32>();
    let each = std::mem::size_of::<FILEDESCRIPTORW>();
    // FILEGROUPDESCRIPTORW already contains one descriptor; add the rest.
    let bytes = header + each * count;

    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes)? };
    let ptr = unsafe { GlobalLock(hglobal) } as *mut u8;
    if ptr.is_null() {
        return Err(windows::core::Error::from(E_NOTIMPL));
    }
    unsafe {
        std::ptr::write(ptr as *mut u32, count as u32);
        let descriptors = ptr.add(header) as *mut FILEDESCRIPTORW;
        for (i, file) in files.iter().enumerate() {
            let mut descriptor = FILEDESCRIPTORW {
                dwFlags: FD_FILESIZE.0 as u32,
                nFileSizeHigh: (file.size >> 32) as u32,
                nFileSizeLow: (file.size & 0xFFFF_FFFF) as u32,
                ..Default::default()
            };
            let name: Vec<u16> = file.name.encode_utf16().take(259).collect();
            for (j, unit) in name.iter().enumerate() {
                descriptor.cFileName[j] = *unit;
            }
            std::ptr::write(descriptors.add(i), descriptor);
        }
        let _ = GlobalUnlock(hglobal);
    }
    Ok(hglobal)
}

// --- IDropSource: decides drop/cancel from our flags -----------------------

#[implement(IDropSource)]
struct DragDropSource {
    session: Arc<DragSession>,
}

impl IDropSource_Impl for DragDropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: windows::core::BOOL,
        grfkeystate: MODIFIERKEYS_FLAGS,
    ) -> windows::core::HRESULT {
        // Physical drag (controller side): no signal_drop/cancel arrives for it,
        // so end it from the real inputs — Escape cancels, releasing the left
        // button (MK_LBUTTON = 0x0001 clears) drops.
        if self.session.physical {
            if fescapepressed.as_bool() {
                return DRAGDROP_S_CANCEL;
            }
            if (grfkeystate.0 & 0x0001) == 0 {
                return DRAGDROP_S_DROP;
            }
        }
        if self.session.is_cancelled() {
            return DRAGDROP_S_CANCEL;
        }
        if self.session.is_released() {
            return DRAGDROP_S_DROP;
        }
        S_OK
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows::core::HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

// --- synthetic left button (brackets the drag) -----------------------------

fn inject_left_button(down: bool) {
    let flag = if down {
        MOUSEEVENTF_LEFTDOWN
    } else {
        MOUSEEVENTF_LEFTUP
    };
    let mut input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flag,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_buffer_deduplicates_retried_chunks_and_rejects_gaps() {
        let buffer = FileBuffer::new("note.txt".into(), 11);

        assert!(buffer.append_at(0, b"hello "));
        // Resend of the same chunk (lost ACK): accepted as success, no second
        // copy written.
        assert!(buffer.append_at(0, b"hello "));
        // A gap: bytes at offset 9 while only 6 are received.
        assert!(!buffer.append_at(9, b"wo"));
        // The missing chunk in order.
        assert!(buffer.append_at(6, b"world"));

        buffer.finish();
        assert_eq!(buffer.received(), 11);
        let mut out = [0_u8; 11];
        let read = buffer.read_at(0, &mut out).expect("stream read");
        assert_eq!(read, 11);
        assert_eq!(&out, b"hello world");
    }

    #[test]
    fn file_buffer_rejects_overlapping_resend_beyond_the_received_prefix() {
        let buffer = FileBuffer::new("note.txt".into(), 8);

        assert!(buffer.append_at(0, b"abc"));
        // Offset 2 overlaps the received prefix but extends past it — corrupt.
        assert!(!buffer.append_at(2, b"XY"));
    }
}
