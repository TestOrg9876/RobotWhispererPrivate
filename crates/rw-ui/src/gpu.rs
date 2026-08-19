//! The one graphics device, opened once and shared by every 3D pane.
//!
//! Opening it is asynchronous and can fail — a machine with no usable adapter
//! is a real machine — so the result is held in an entity that panes observe.
//! Until it arrives they say they are starting up; if it never does they say
//! why, which is more use than an empty rectangle.

use std::sync::Arc;

use gpui::{App, AppContext as _, Entity};
use rw_render::Renderer;

#[derive(Default)]
pub struct Gpu {
    renderer: Option<Arc<Renderer>>,
    error: Option<String>,
}

impl Gpu {
    /// Opens the device in the background and hands back the entity that will
    /// hold it.
    pub fn spawn(cx: &mut App) -> Entity<Self> {
        let gpu = cx.new(|_| Self::default());
        cx.spawn({
            let gpu = gpu.clone();
            async move |cx| {
                let opened = Renderer::new().await;
                gpu.update(cx, |gpu, cx| {
                    match opened {
                        Ok(renderer) => {
                            tracing::info!("graphics: {}", renderer.adapter);
                            gpu.renderer = Some(Arc::new(renderer));
                        }
                        Err(reason) => {
                            tracing::warn!("no 3D: {reason}");
                            gpu.error = Some(reason);
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        gpu
    }

    pub fn renderer(&self) -> Option<Arc<Renderer>> {
        self.renderer.clone()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the device is still being opened, as opposed to unavailable.
    pub fn starting(&self) -> bool {
        self.renderer.is_none() && self.error.is_none()
    }

    #[cfg(test)]
    pub fn failed(reason: &str) -> Self {
        Self {
            renderer: None,
            error: Some(reason.into()),
        }
    }
}
