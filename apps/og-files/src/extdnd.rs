//! External (cross-application) drag and drop: files dragged out of the
//! window into other apps, and files dropped in from other apps.
//!
//! iced/winit have no Wayland drag-and-drop support, so with the `og-suite`
//! feature this wraps `og-wayland`, which runs a second Wayland connection
//! on winit's display. Without the feature everything is a harmless stub.
//!
//! Integration notes for the app:
//! * Once [`start_drag`] succeeds the compositor owns the pointer grab, so
//!   iced never sees the matching mouse release — reset in-app drag state
//!   right after calling it.
//! * A drag we started that comes back into our own window arrives as a
//!   normal `Enter`/`Motion`/`Dropped`, so route `Dropped` through the
//!   same drop logic as in-app drops.
//! * Coordinates are surface-local logical pixels (iced's cursor space).

use std::path::PathBuf;

/// Something being dragged into our window from another client.
#[derive(Debug, Clone)]
// standalone builds never construct these; x/y are kept for per-folder
// drop targeting later
#[allow(dead_code)]
pub enum DropEvent {
    Enter { x: f32, y: f32 },
    Motion { x: f32, y: f32 },
    Leave,
    Dropped { paths: Vec<PathBuf>, x: f32, y: f32 },
}

pub use imp::{connect, drops, start_drag, Dnd};

#[cfg(feature = "og-suite")]
mod imp {
    use super::DropEvent;
    use iced::futures::channel::mpsc as fmpsc;
    use iced::Task;
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Handle to the external drag-and-drop connection. Cheap to clone.
    #[derive(Clone)]
    pub struct Dnd {
        inner: Arc<Inner>,
    }

    struct Inner {
        drag_tx: og_wayland::DragSender,
        /// The worker's event receiver; taken by the forwarding thread of
        /// the active [`drops`] stream and put back when that stream ends.
        events: Mutex<Option<Receiver<og_wayland::DropEvent>>>,
    }

    impl std::fmt::Debug for Dnd {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Dnd")
        }
    }

    /// Connect to the compositor for the app's main window. Returns Err with
    /// a short reason when not on Wayland.
    pub fn connect() -> Task<Result<Dnd, String>> {
        iced::window::get_oldest().then(|id| match id {
            None => Task::done(Err("no window".to_string())),
            Some(id) => {
                iced::window::run_with_handle(id, og_wayland::window_ptrs_from_handle).map(|ptrs| {
                    let ptrs = ptrs.ok_or_else(|| "not a Wayland window".to_string())?;
                    let (drag_tx, events) = og_wayland::spawn_for_window(ptrs);
                    Ok(Dnd {
                        inner: Arc::new(Inner {
                            drag_tx,
                            events: Mutex::new(Some(events)),
                        }),
                    })
                })
            }
        })
    }

    /// Stream of drops into our window; ends when the connection closes.
    ///
    /// A blocking thread forwards og-wayland's std receiver into an async
    /// channel, so events arrive immediately with no polling tick. Only one
    /// stream is live at a time; a second concurrent call gets an empty
    /// stream, and a dropped stream hands the receiver back within ~0.5 s.
    pub fn drops(dnd: Dnd) -> impl iced::futures::Stream<Item = DropEvent> + Send + 'static {
        let (tx, rx) = fmpsc::unbounded();
        let taken = dnd
            .inner
            .events
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(events) = taken {
            let inner = dnd.inner.clone();
            let spawned = std::thread::Builder::new()
                .name("og-files-dnd".into())
                .spawn(move || {
                    forward(&events, &tx);
                    if let Ok(mut slot) = inner.events.lock() {
                        *slot = Some(events);
                    }
                });
            if let Err(e) = spawned {
                eprintln!("og-files: dnd forwarding thread failed: {e}");
            }
        }
        rx
    }

    /// Returns when the worker is gone or the stream was dropped.
    fn forward(events: &Receiver<og_wayland::DropEvent>, tx: &fmpsc::UnboundedSender<DropEvent>) {
        loop {
            match events.recv_timeout(Duration::from_millis(500)) {
                Ok(ev) => {
                    let Some(ev) = map_event(ev) else { continue };
                    if tx.unbounded_send(ev).is_err() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if tx.is_closed() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    pub(super) fn map_event(ev: og_wayland::DropEvent) -> Option<DropEvent> {
        use og_wayland::DropEvent as W;
        Some(match ev {
            W::Enter { x, y } => DropEvent::Enter {
                x: x as f32,
                y: y as f32,
            },
            W::Motion { x, y } => DropEvent::Motion {
                x: x as f32,
                y: y as f32,
            },
            W::Leave => DropEvent::Leave,
            W::Dropped { paths, x, y } => DropEvent::Dropped {
                paths,
                x: x as f32,
                y: y as f32,
            },
            // Feedback on our own drag-out (failures are logged by og-wayland).
            W::DragOut(_) => return None,
        })
    }

    /// Start dragging `paths` out of the window (call when an in-app drag
    /// leaves the window or as a direct drag-out). Must be called while the
    /// mouse button is still held. Returns Err if not possible.
    pub fn start_drag(dnd: &Dnd, paths: Vec<PathBuf>) -> Result<(), String> {
        if paths.is_empty() {
            return Err("nothing to drag".into());
        }
        dnd.inner
            .drag_tx
            .send(og_wayland::DragRequest { paths })
            .map_err(|_| "drag-and-drop connection closed".to_string())
    }
}

#[cfg(not(feature = "og-suite"))]
mod imp {
    use super::DropEvent;
    use iced::Task;
    use std::path::PathBuf;

    /// Handle to the external drag-and-drop connection. Cheap to clone.
    #[derive(Clone)]
    pub struct Dnd {
        _private: (),
    }

    impl std::fmt::Debug for Dnd {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Dnd")
        }
    }

    #[cfg(test)]
    impl Dnd {
        pub(in crate::extdnd) fn stub() -> Self {
            Dnd { _private: () }
        }
    }

    /// Without the `og-suite` feature there is no connection.
    pub fn connect() -> Task<Result<Dnd, String>> {
        Task::done(Err("built without og-suite drag-and-drop".into()))
    }

    /// Stream of drops into our window; always empty without `og-suite`.
    pub fn drops(_dnd: Dnd) -> impl iced::futures::Stream<Item = DropEvent> + Send + 'static {
        iced::futures::stream::empty()
    }

    /// Dragging out is unavailable without `og-suite`.
    pub fn start_drag(_dnd: &Dnd, _paths: Vec<PathBuf>) -> Result<(), String> {
        Err("built without og-suite drag-and-drop".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "og-suite"))]
    #[test]
    fn stub_is_inert() {
        use iced::futures::StreamExt;
        let dnd = imp::Dnd::stub();
        assert_eq!(format!("{dnd:?}"), "Dnd");
        assert!(start_drag(&dnd, vec![PathBuf::from("/tmp")]).is_err());
        let collected: Vec<DropEvent> = iced::futures::executor::block_on(drops(dnd).collect());
        assert!(collected.is_empty());
    }

    #[cfg(feature = "og-suite")]
    #[test]
    fn maps_wayland_events() {
        use og_wayland::DropEvent as W;
        assert!(
            matches!(imp::map_event(W::Enter { x: 1.5, y: 2.0 }), Some(DropEvent::Enter { x, y }) if x == 1.5 && y == 2.0)
        );
        assert!(matches!(imp::map_event(W::Leave), Some(DropEvent::Leave)));
        assert!(imp::map_event(W::DragOut(og_wayland::DragOutcome::Cancelled)).is_none());
        match imp::map_event(W::Dropped {
            paths: vec![PathBuf::from("/a b")],
            x: 3.0,
            y: 4.0,
        }) {
            Some(DropEvent::Dropped { paths, x, y }) => {
                assert_eq!(paths, vec![PathBuf::from("/a b")]);
                assert_eq!((x, y), (3.0, 4.0));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
