//! Headless GPU device creation, for tests, soak runs and CI (software adapters included).

use crate::CompositorError;

/// A device opened without any window.
#[derive(Debug)]
pub struct HeadlessGpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_info: wgpu::AdapterInfo,
}

/// Opens the default adapter without a surface. Honours the `WGPU_BACKEND`,
/// `WGPU_ADAPTER_NAME` and `WGPU_POWER_PREF` environment variables, so CI can force a
/// software adapter (lavapipe on Linux, WARP on Windows).
pub async fn request_gpu() -> Result<HeadlessGpu, CompositorError> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = wgpu::util::initialize_adapter_from_env_or_default(&instance, None).await?;
    let adapter_info = adapter.get_info();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("aiguillage.headless"),
            ..Default::default()
        })
        .await?;
    Ok(HeadlessGpu {
        device,
        queue,
        adapter_info,
    })
}
