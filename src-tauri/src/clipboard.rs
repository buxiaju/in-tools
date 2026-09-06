//! 剪贴板历史监控：后台轮询系统剪贴板变化，维护最近 N 条文本记录。
//!
//! 监控线程在 app setup 时启动，每 500ms 检查一次剪贴板内容哈希，
//! 变化时追加到环形缓冲区并落盘到 `~/.intools/clipboard-history.json`。
//! 前端通过 `get_clipboard_history` 命令读取历史列表。

use std::hash::{DefaultHasher, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ─── Windows FFI（直接声明，避免 windows-sys feature 依赖） ───

#[cfg(windows)]
mod win {
    /// 轻量句柄类型，避免依赖 windows-sys feature。
    #[repr(transparent)]
    pub struct HWND(pub *mut core::ffi::c_void);
    impl Default for HWND {
        fn default() -> Self {
            Self(core::ptr::null_mut())
        }
    }
    /// HANDLE 在 Win32 FFI 中等价于 void*。
    pub type HANDLE = *mut core::ffi::c_void;

    extern "system" {
        pub fn OpenClipboard(hWndNewOwner: HWND) -> i32;
        pub fn CloseClipboard() -> i32;
        pub fn EmptyClipboard() -> i32;
        pub fn GetClipboardData(format: u32) -> HANDLE;
        pub fn SetClipboardData(format: u32, hMem: HANDLE) -> HANDLE;
        pub fn GlobalLock(hMem: HANDLE) -> *mut core::ffi::c_void;
        pub fn GlobalUnlock(hMem: HANDLE) -> i32;
        pub fn GlobalAlloc(uFlags: u32, dwBytes: usize) -> HANDLE;
        pub fn GlobalFree(hMem: HANDLE) -> HANDLE;
    }

    pub const CF_UNICODETEXT: u32 = 13;
    pub const GMEM_MOVEABLE: u32 = 0x0002;
}

// ─── 数据结构 ───

/// 单条剪贴板记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardEntry {
    /// 文本内容（可能被截断）。
    pub text: String,
    /// 原始文本长度。
    pub length: usize,
    /// Unix 时间戳（秒）。
    pub timestamp: u64,
}

/// 剪贴板历史状态。
struct ClipboardState {
    entries: Vec<ClipboardEntry>,
    last_hash: u64,
}

/// 全局单例，供监控线程写入、Tauri command 读取。
static STATE: OnceLock<Arc<Mutex<ClipboardState>>> = OnceLock::new();

/// 最大保留条数。
const MAX_ENTRIES: usize = 50;
/// 截断显示的长度上限。
const MAX_TEXT_LEN: usize = 500;
/// 轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 初始化全局状态。在 main.rs setup 中调用一次。
pub fn init() {
    let state = Arc::new(Mutex::new(ClipboardState {
        entries: load_from_disk(),
        last_hash: 0,
    }));
    let _ = STATE.set(state);
}

/// 启动后台监控线程。应在 app setup 完成后调用。
pub fn start_monitoring() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(1));
        loop {
            poll_clipboard();
            std::thread::sleep(POLL_INTERVAL);
        }
    });
}

/// 获取当前历史列表（newest first）。
pub fn get_history() -> Vec<ClipboardEntry> {
    STATE
        .get()
        .and_then(|s| s.lock().ok())
        .map(|s| s.entries.iter().rev().cloned().collect())
        .unwrap_or_default()
}

/// 将指定条目复制到系统剪贴板（点击回填）。
#[cfg(windows)]
pub fn copy_entry_to_clipboard(text: &str) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let byte_len = wide.len() * 2;

    unsafe {
        if win::OpenClipboard(win::HWND::default()) == 0 {
            return Err("无法打开剪贴板（可能被其他程序占用）".into());
        }
        win::EmptyClipboard();

        let h = win::GlobalAlloc(win::GMEM_MOVEABLE, byte_len);
        if h.is_null() {
            win::CloseClipboard();
            return Err("GlobalAlloc 失败".into());
        }
        let ptr = win::GlobalLock(h) as *mut u16;
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        win::GlobalUnlock(h);

        if win::SetClipboardData(win::CF_UNICODETEXT, h).is_null() {
            win::GlobalFree(h);
            win::CloseClipboard();
            return Err("SetClipboardData 失败".into());
        }
        win::CloseClipboard();
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn copy_entry_to_clipboard(_text: &str) -> Result<(), String> {
    Err("剪贴板操作仅支持 Windows".into())
}

/// 后台轮询一次剪贴板。
fn poll_clipboard() {
    let text = read_clipboard_text();
    let text = match text {
        Some(t) if !t.is_empty() => t,
        _ => return,
    };

    let hash = hash_text(&text);

    let Some(mut state) = STATE
        .get()
        .and_then(|s| s.lock().ok())
    else {
        return;
    };

    // 去重：内容相同则跳过
    if hash == state.last_hash {
        return;
    }
    state.last_hash = hash;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let display_text = if text.len() > MAX_TEXT_LEN {
        // 找到不超过 MAX_TEXT_LEN 字节的最大 char boundary，避免在多字节字符
        // （如中文 3 字节 UTF-8）内部切断导致 panic。
        let mut end = MAX_TEXT_LEN;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    } else {
        text.clone()
    };

    state.entries.push(ClipboardEntry {
        text: display_text,
        length: text.len(),
        timestamp: now,
    });

    if state.entries.len() > MAX_ENTRIES {
        let excess = state.entries.len() - MAX_ENTRIES;
        state.entries.drain(..excess);
    }

    save_to_disk(&state.entries);
}

/// 读取系统剪贴板文本内容。
#[cfg(windows)]
fn read_clipboard_text() -> Option<String> {
    unsafe {
        if win::OpenClipboard(win::HWND::default()) == 0 {
            return None;
        }

        let handle = win::GetClipboardData(win::CF_UNICODETEXT);
        if handle.is_null() {
            win::CloseClipboard();
            return None;
        }

        let ptr = win::GlobalLock(handle) as *const u16;
        if ptr.is_null() {
            win::CloseClipboard();
            return None;
        }

        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);

        win::GlobalUnlock(handle);
        win::CloseClipboard();
        Some(text)
    }
}

#[cfg(not(windows))]
fn read_clipboard_text() -> Option<String> {
    None
}

/// 文本哈希（用于去重）。
fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(text.as_bytes());
    hasher.finish()
}

/// 历史文件路径。
fn history_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".intools").join("clipboard-history.json")
}

/// 从磁盘加载历史。
fn load_from_disk() -> Vec<ClipboardEntry> {
    let path = history_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 保存历史到磁盘（原子写入：tmp → rename）。
///
/// 复用 config 层的「先写临时文件再原子覆盖」策略：直写会在宿主崩溃时
/// 留下半截 JSON，下次加载全部丢失。
fn save_to_disk(entries: &[ClipboardEntry]) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_string(entries) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}
