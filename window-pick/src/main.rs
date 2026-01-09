//! Click on a window to get its ID and metadata.
//!
//! Usage:
//!   window-pick              # JSON output (default)
//!   window-pick --format=id  # Just the window ID
//!   window-pick --format=pretty  # Human-readable

use clap::{Parser, ValueEnum};
use serde::Serialize;

#[derive(Parser)]
#[command(name = "window-pick")]
#[command(about = "Click on a window to get its ID and metadata")]
struct Cli {
    /// Output format
    #[arg(long, short, default_value = "json")]
    format: OutputFormat,

    /// List all windows instead of click-to-select
    #[arg(long)]
    list: bool,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    /// JSON object with all window info
    Json,
    /// Just the window ID (for scripting)
    Id,
    /// Human-readable output
    Pretty,
}

#[derive(Debug, Clone, Serialize)]
struct WindowInfo {
    id: u32,
    title: Option<String>,
    app: Option<String>,
    bounds: WindowBounds,
    layer: i32,
    on_screen: bool,
}

#[derive(Debug, Clone, Serialize)]
struct WindowBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn main() {
    let cli = Cli::parse();

    if cli.list {
        list_all_windows(&cli.format);
    } else {
        click_to_select(&cli.format);
    }
}

fn list_all_windows(format: &OutputFormat) {
    let windows = get_all_windows();

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&windows).unwrap());
        }
        OutputFormat::Id => {
            for w in &windows {
                println!("{}", w.id);
            }
        }
        OutputFormat::Pretty => {
            for w in &windows {
                print_window_pretty(w);
                println!();
            }
        }
    }
}

fn click_to_select(format: &OutputFormat) {
    eprintln!("Click on any window...");

    // Wait for mouse button to be released first (in case already pressed)
    while is_mouse_down() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Wait for mouse click
    while !is_mouse_down() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Get mouse position at click
    let (mouse_x, mouse_y) = get_mouse_position();

    // Find window under cursor
    let windows = get_all_windows();
    let clicked_window = find_window_at_point(&windows, mouse_x, mouse_y);

    match clicked_window {
        Some(window) => {
            output_window(&window, format);
        }
        None => {
            eprintln!("No window found at ({}, {})", mouse_x, mouse_y);
            std::process::exit(1);
        }
    }
}

fn output_window(window: &WindowInfo, format: &OutputFormat) {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(&window).unwrap());
        }
        OutputFormat::Id => {
            println!("{}", window.id);
        }
        OutputFormat::Pretty => {
            print_window_pretty(window);
        }
    }
}

fn print_window_pretty(window: &WindowInfo) {
    println!("Window ID: {}", window.id);
    if let Some(ref title) = window.title {
        println!("Title: {}", title);
    }
    if let Some(ref app) = window.app {
        println!("App: {}", app);
    }
    println!(
        "Bounds: {}x{} at ({}, {})",
        window.bounds.width as i32,
        window.bounds.height as i32,
        window.bounds.x as i32,
        window.bounds.y as i32
    );
    println!("Layer: {}", window.layer);
}

fn find_window_at_point(windows: &[WindowInfo], x: f64, y: f64) -> Option<WindowInfo> {
    // Windows are returned in front-to-back order (lower layer = more in front)
    // We want the topmost window that contains the point
    let mut candidates: Vec<_> = windows
        .iter()
        .filter(|w| {
            w.on_screen
                && x >= w.bounds.x
                && x < w.bounds.x + w.bounds.width
                && y >= w.bounds.y
                && y < w.bounds.y + w.bounds.height
        })
        .collect();

    // Sort by layer (lower layer number = more in front on macOS)
    candidates.sort_by_key(|w| w.layer);

    candidates.first().cloned().cloned()
}

// ============================================================================
// macOS-specific implementations
// ============================================================================

#[cfg(target_os = "macos")]
mod macos {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionaryRef;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::event::CGEvent;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    use super::{WindowBounds, WindowInfo};

    pub fn get_all_windows() -> Vec<WindowInfo> {
        let mut windows = Vec::new();

        unsafe {
            let window_list = CGWindowListCopyWindowInfo(
                kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
                kCGNullWindowID,
            );

            if window_list.is_null() {
                return windows;
            }

            let count = CFArrayGetCount(window_list);

            for i in 0..count {
                let window_dict = CFArrayGetValueAtIndex(window_list, i) as CFDictionaryRef;
                if window_dict.is_null() {
                    continue;
                }

                if let Some(info) = parse_window_dict(window_dict) {
                    windows.push(info);
                }
            }

            CFRelease(window_list as *const _);
        }

        windows
    }

    unsafe fn parse_window_dict(dict: CFDictionaryRef) -> Option<WindowInfo> {
        // Get window ID
        let id_key = CFString::new("kCGWindowNumber");
        let id_ptr = CFDictionaryGetValue(dict, id_key.as_CFTypeRef() as *const _);
        if id_ptr.is_null() {
            return None;
        }
        let id_num = CFNumber::wrap_under_get_rule(id_ptr as _);
        let id: i32 = id_num.to_i32()?;

        // Get window layer
        let layer_key = CFString::new("kCGWindowLayer");
        let layer_ptr = CFDictionaryGetValue(dict, layer_key.as_CFTypeRef() as *const _);
        let layer = if !layer_ptr.is_null() {
            let layer_num = CFNumber::wrap_under_get_rule(layer_ptr as _);
            layer_num.to_i32().unwrap_or(0)
        } else {
            0
        };

        // Get window bounds
        let bounds_key = CFString::new("kCGWindowBounds");
        let bounds_ptr = CFDictionaryGetValue(dict, bounds_key.as_CFTypeRef() as *const _);
        if bounds_ptr.is_null() {
            return None;
        }
        let bounds_dict = bounds_ptr as CFDictionaryRef;

        let x = get_dict_number(bounds_dict, "X").unwrap_or(0.0);
        let y = get_dict_number(bounds_dict, "Y").unwrap_or(0.0);
        let width = get_dict_number(bounds_dict, "Width").unwrap_or(0.0);
        let height = get_dict_number(bounds_dict, "Height").unwrap_or(0.0);

        // Get window title
        let title_key = CFString::new("kCGWindowName");
        let title_ptr = CFDictionaryGetValue(dict, title_key.as_CFTypeRef() as *const _);
        let title = if !title_ptr.is_null() {
            let cf_str = CFString::wrap_under_get_rule(title_ptr as _);
            Some(cf_str.to_string())
        } else {
            None
        };

        // Get owner (app) name
        let owner_key = CFString::new("kCGWindowOwnerName");
        let owner_ptr = CFDictionaryGetValue(dict, owner_key.as_CFTypeRef() as *const _);
        let app = if !owner_ptr.is_null() {
            let cf_str = CFString::wrap_under_get_rule(owner_ptr as _);
            Some(cf_str.to_string())
        } else {
            None
        };

        // Check if on screen
        let onscreen_key = CFString::new("kCGWindowIsOnscreen");
        let onscreen_ptr = CFDictionaryGetValue(dict, onscreen_key.as_CFTypeRef() as *const _);
        let on_screen = if !onscreen_ptr.is_null() {
            let cf_bool = CFBoolean::wrap_under_get_rule(onscreen_ptr as _);
            cf_bool == CFBoolean::true_value()
        } else {
            true // Default to true for on-screen list
        };

        Some(WindowInfo {
            id: id as u32,
            title,
            app,
            bounds: WindowBounds {
                x,
                y,
                width,
                height,
            },
            layer,
            on_screen,
        })
    }

    unsafe fn get_dict_number(dict: CFDictionaryRef, key: &str) -> Option<f64> {
        let cf_key = CFString::new(key);
        let ptr = CFDictionaryGetValue(dict, cf_key.as_CFTypeRef() as *const _);
        if ptr.is_null() {
            return None;
        }
        let num = CFNumber::wrap_under_get_rule(ptr as _);
        num.to_f64()
    }

    pub fn is_mouse_down() -> bool {
        unsafe {
            CGEventSourceButtonState(
                CGEventSourceStateID::CombinedSessionState,
                CGMouseButton::Left,
            )
        }
    }

    pub fn get_mouse_position() -> (f64, f64) {
        if let Ok(source) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) {
            if let Ok(event) = CGEvent::new(source) {
                let location = event.location();
                return (location.x, location.y);
            }
        }
        (0.0, 0.0)
    }

    // FFI declarations for CoreFoundation/CoreGraphics
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(array: CFArrayRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFArrayRef, index: isize) -> *const std::ffi::c_void;
        fn CFDictionaryGetValue(
            dict: CFDictionaryRef,
            key: *const std::ffi::c_void,
        ) -> *const std::ffi::c_void;
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> CFArrayRef;
        fn CGEventSourceButtonState(stateID: CGEventSourceStateID, button: CGMouseButton) -> bool;
    }

    type CFArrayRef = *const std::ffi::c_void;

    // macOS constants - using Apple's naming convention
    #[allow(non_upper_case_globals)]
    const kCGWindowListOptionOnScreenOnly: u32 = 1 << 0;
    #[allow(non_upper_case_globals)]
    const kCGWindowListExcludeDesktopElements: u32 = 1 << 4;
    #[allow(non_upper_case_globals)]
    const kCGNullWindowID: u32 = 0;

    #[repr(u32)]
    #[derive(Clone, Copy)]
    pub enum CGMouseButton {
        Left = 0,
    }
}

#[cfg(target_os = "macos")]
fn get_all_windows() -> Vec<WindowInfo> {
    macos::get_all_windows()
}

#[cfg(target_os = "macos")]
fn is_mouse_down() -> bool {
    macos::is_mouse_down()
}

#[cfg(target_os = "macos")]
fn get_mouse_position() -> (f64, f64) {
    macos::get_mouse_position()
}

// ============================================================================
// Stub implementations for non-macOS platforms
// ============================================================================

#[cfg(not(target_os = "macos"))]
fn get_all_windows() -> Vec<WindowInfo> {
    eprintln!("window-pick currently only supports macOS");
    Vec::new()
}

#[cfg(not(target_os = "macos"))]
fn is_mouse_down() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
fn get_mouse_position() -> (f64, f64) {
    (0.0, 0.0)
}
