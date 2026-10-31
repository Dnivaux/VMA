/// Errors of the compositor crate.
#[derive(Debug, thiserror::Error)]
pub enum CompositorError {
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(#[from] wgpu::RequestAdapterError),
    #[error("could not open the GPU device: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),
    #[error("GPU buffer mapping failed: {0}")]
    BufferMap(#[from] wgpu::BufferAsyncError),
    #[error("GPU poll failed: {0}")]
    Poll(#[from] wgpu::PollError),
    #[error("GPU buffer view failed: {0}")]
    MapRange(#[from] wgpu::MapRangeError),
    #[error("GPU readback channel closed before the buffer was mapped")]
    ReadbackAborted,
    #[error("this texture format cannot be read back")]
    UnsupportedReadback,
}
