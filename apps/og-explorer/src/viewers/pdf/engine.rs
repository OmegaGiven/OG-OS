//! PDF page rasterizer: one worker thread per open document, owning the
//! parsed `hayro` document and its render cache (fonts, images) so repeated
//! renders don't re-parse anything. Requests come in over a channel and each
//! one replies on its own oneshot, so callers get a plain Future per page.
//!
//! hayro is pure Rust (MIT/Apache), CPU-only, no system libraries.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, RenderCache, RenderSettings};
use iced::widget::image;
use std::sync::{mpsc, Arc};
use tokio::sync::oneshot;

/// Largest bitmap we'll rasterize (keeps 800% zoom on A0 from eating RAM).
const MAX_PIXELS: f32 = 40_000_000.0;

struct Req {
    page: usize,
    scale: f32,
    reply: oneshot::Sender<Result<image::Handle, String>>,
}

#[derive(Clone)]
pub struct Engine {
    tx: mpsc::Sender<Req>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Engine")
    }
}

/// Parse `bytes` on a new worker thread. Returns the engine plus each page's
/// display size in PDF points (rotation from the file already applied).
pub async fn open(bytes: Arc<Vec<u8>>) -> Result<(Engine, Vec<(f32, f32)>), String> {
    let (tx, rx) = mpsc::channel::<Req>();
    let (info_tx, info_rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("og-explorer-pdf".into())
        .spawn(move || worker(bytes, rx, info_tx))
        .map_err(|e| e.to_string())?;
    let sizes = info_rx
        .await
        .map_err(|_| "PDF renderer crashed".to_string())??;
    Ok((Engine { tx }, sizes))
}

fn worker(
    bytes: Arc<Vec<u8>>,
    rx: mpsc::Receiver<Req>,
    info: oneshot::Sender<Result<Vec<(f32, f32)>, String>>,
) {
    let pdf = match Pdf::new(bytes) {
        Ok(p) => p,
        Err(e) => {
            let _ = info.send(Err(format!("can't read PDF ({e:?})")));
            return;
        }
    };
    let sizes = pdf.pages().iter().map(|p| p.render_dimensions()).collect();
    let _ = info.send(Ok(sizes));
    let cache = RenderCache::new();
    let settings = InterpreterSettings::default();
    // ends when every Engine clone is dropped (tab closed)
    while let Ok(req) = rx.recv() {
        if req.reply.is_closed() {
            continue; // caller gave up (navigated away)
        }
        let result = match pdf.pages().get(req.page) {
            None => Err(format!("no page {}", req.page + 1)),
            Some(page) => {
                let (w, h) = page.render_dimensions();
                let max_scale = (MAX_PIXELS / (w * h).max(1.0)).sqrt();
                let scale = req.scale.min(max_scale);
                // a malformed page must not take the whole worker down
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let pm = render(
                        page,
                        &cache,
                        &settings,
                        &RenderSettings {
                            x_scale: scale,
                            y_scale: scale,
                            bg_color: WHITE,
                            ..Default::default()
                        },
                    );
                    // opaque white background ⇒ premultiplied == straight RGBA
                    image::Handle::from_rgba(
                        pm.width() as u32,
                        pm.height() as u32,
                        pm.data_as_u8_slice().to_vec(),
                    )
                }))
                .map_err(|_| format!("page {} failed to render", req.page + 1))
            }
        };
        let _ = req.reply.send(result);
    }
}

impl Engine {
    pub fn render(
        &self,
        page: usize,
        scale: f32,
    ) -> impl std::future::Future<Output = Result<image::Handle, String>> {
        let (reply, rx) = oneshot::channel();
        let sent = self.tx.send(Req { page, scale, reply });
        async move {
            sent.map_err(|_| "PDF renderer stopped".to_string())?;
            rx.await.map_err(|_| "PDF renderer stopped".to_string())?
        }
    }
}
