/// Wraps platform-specific window and display handles for cross-plugin interop.
/// Used by winit, wgpu, opengl, and wry plugins to share window references.
///
/// Safety: must only be used on the thread that created the window.
/// Single-threaded usage is enforced by the event loop model.
pub struct WindowHandle {
    // Store as raw usize values to avoid depending on raw-window-handle crate
    // at the zolo-plugin level. Each plugin converts to/from its own raw handle types.
    window_handle_data: [u8; 128], // enough space for any RawWindowHandle variant
    display_handle_data: [u8; 128],
    window_handle_len: usize,
    display_handle_len: usize,
}

// Safety: Window handles are only accessed on the main thread via the event loop
unsafe impl Send for WindowHandle {}
unsafe impl Sync for WindowHandle {}

impl WindowHandle {
    /// Create a new WindowHandle from raw bytes.
    /// The bytes are opaque — only the creating plugin knows how to interpret them.
    pub fn new(window_data: &[u8], display_data: &[u8]) -> Self {
        let mut wh = Self {
            window_handle_data: [0u8; 128],
            display_handle_data: [0u8; 128],
            window_handle_len: window_data.len().min(128),
            display_handle_len: display_data.len().min(128),
        };
        wh.window_handle_data[..wh.window_handle_len]
            .copy_from_slice(&window_data[..wh.window_handle_len]);
        wh.display_handle_data[..wh.display_handle_len]
            .copy_from_slice(&display_data[..wh.display_handle_len]);
        wh
    }

    pub fn window_data(&self) -> &[u8] {
        &self.window_handle_data[..self.window_handle_len]
    }

    pub fn display_data(&self) -> &[u8] {
        &self.display_handle_data[..self.display_handle_len]
    }
}
