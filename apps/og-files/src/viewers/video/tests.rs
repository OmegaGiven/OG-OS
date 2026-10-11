use super::ff::tests::{make_video_with, Tmp};
use super::*;
use iced::keyboard::key::Named as N;

#[test]
fn video_supported_extensions() {
    for n in [
        "a.mp4", "b.MKV", "c.webm", "d.mov", "e.mpeg", "f.ts", "g.3gp", "h.ogv",
    ] {
        assert!(supported(Path::new(n)), "{n}");
    }
    for n in ["a.mp3", "b.png", "c", "d.txt"] {
        assert!(!supported(Path::new(n)), "{n}");
    }
}

#[test]
fn video_backend_selection() {
    assert_eq!(select_backend(Ok(()), false), Backend::Mpv);
    assert_eq!(select_backend(Ok(()), true), Backend::Ffmpeg);
    assert_eq!(select_backend(Err("no lib"), false), Backend::Ffmpeg);
    assert_eq!(select_backend(Err("no lib"), true), Backend::Ffmpeg);
    // the real thing agrees with the library's availability
    let want = if mpv::api().is_ok() && std::env::var_os("OGX_NO_MPV").is_none() {
        Backend::Mpv
    } else {
        Backend::Ffmpeg
    };
    assert_eq!(backend(), want);
}

#[test]
fn video_mpv_option_setup() {
    let o = mpv_options(Path::new("/media/clips/my 50% clip.mkv"), false);
    let get = |k: &str| o.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str());
    assert_eq!(get("vo"), Some("libmpv"));
    assert_eq!(get("hwdec"), Some("auto-safe"));
    assert_eq!(get("ao"), Some("pipewire,pulse,alsa"));
    assert_eq!(get("keep-open"), Some("yes"));
    assert_eq!(get("idle"), Some("yes"));
    assert_eq!(get("input-default-bindings"), Some("no"));
    assert_eq!(get("osc"), Some("no"));
    assert_eq!(get("terminal"), Some("no"));
    assert_eq!(get("sub-auto"), Some("fuzzy"));
    assert_eq!(get("audio-file-auto"), Some("fuzzy"));
    assert_eq!(get("video-sync"), Some("audio"));
    assert_eq!(get("interpolation"), Some("no"));
    assert_eq!(get("scale"), Some("spline36"));
    assert_eq!(get("volume-max"), Some("130"));
    assert_eq!(get("screenshot-directory"), Some("/media/clips"));
    assert_eq!(
        get("screenshot-template"),
        Some("my 50%% clip-frame-%wH%wM%wS")
    );
    assert_eq!(get("mute"), None);
    let muted = mpv_options(Path::new("a.mp4"), true);
    assert!(muted.iter().any(|(k, v)| k == "mute" && v == "yes"));
    // tests never get a real audio output
    let v = viewer_options(Path::new("a.mp4"));
    assert!(v.iter().any(|(k, v)| k == "ao" && v == "null"));
    assert_eq!(v.iter().filter(|(k, _)| k == "ao").count(), 1);
    // every option is accepted by the installed mpv
    if mpv::api().is_ok() {
        let m = mpv::Mpv::new(&[]).unwrap();
        for (k, v) in &mpv_options(Path::new("/x/y.mp4"), true) {
            if k != "vo" {
                assert!(m.set_option(k, v).is_ok() || k == "ao", "{k}={v}");
            }
        }
        for a in ASPECTS {
            assert!(m.set_str("video-aspect-override", a).is_ok(), "{a}");
        }
    }
}

fn track(id: i64, kind: &str, extra: Vec<(&str, Node)>) -> Node {
    let mut m = vec![
        ("id".to_string(), Node::Int(id)),
        ("type".to_string(), Node::Str(kind.into())),
    ];
    m.extend(extra.into_iter().map(|(k, v)| (k.to_string(), v)));
    Node::Map(m)
}

fn sample_tracks() -> Node {
    Node::Array(vec![
        track(
            1,
            "video",
            vec![
                ("codec", Node::Str("h264".into())),
                ("selected", Node::Flag(true)),
            ],
        ),
        track(
            1,
            "audio",
            vec![
                ("codec", Node::Str("aac".into())),
                ("lang", Node::Str("eng".into())),
                ("demux-channel-count", Node::Int(2)),
                ("demux-samplerate", Node::Int(48000)),
                ("selected", Node::Flag(true)),
            ],
        ),
        track(
            2,
            "audio",
            vec![
                ("codec", Node::Str("opus".into())),
                ("title", Node::Str("Commentary".into())),
            ],
        ),
        track(
            1,
            "sub",
            vec![
                ("codec", Node::Str("subrip".into())),
                ("external", Node::Flag(true)),
            ],
        ),
        // malformed entries are skipped
        Node::Map(vec![("type".into(), Node::Str("sub".into()))]),
    ])
}

#[test]
fn video_track_list_parsing() {
    let t = parse_tracks(&sample_tracks());
    assert_eq!(t.len(), 4);
    assert_eq!(t[1].lang.as_deref(), Some("eng"));
    assert_eq!(t[1].channels, Some(2));
    assert!(t[1].selected && !t[2].selected);
    assert_eq!(t[1].label(), "#1 eng (aac, 2ch)");
    assert_eq!(t[2].label(), "#2 “Commentary” (opus)");
    assert_eq!(t[3].label(), "#1 (subrip, external)");

    let (a, cur) = track_choices(&t, "audio");
    assert_eq!(a.len(), 2);
    assert_eq!(cur.unwrap().id, Some(1));
    let (s, cur) = track_choices(&t, "sub");
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].id, None);
    assert_eq!(cur.unwrap().id, None, "no sub selected → Off");
    assert_eq!(s[0].to_string(), "Subtitles off");

    let mut l = Live::default();
    l.apply("track-list", &sample_tracks());
    l.apply(
        "video-params",
        &Node::Map(vec![("w".into(), Node::Int(1920)), ("h".into(), Node::Int(1080))]),
    );
    l.apply("container-fps", &Node::Double(29.97));
    l.apply("hwdec-current", &Node::Str("vaapi".into()));
    l.loaded = true;
    assert_eq!(
        l.summary().unwrap(),
        "H264 · 1920×1080 · 29.97 fps · hw: vaapi · AAC 48 kHz 2ch"
    );
    l.apply("hwdec-current", &Node::Str("no".into()));
    assert!(l.summary().unwrap().contains("hw: no"));
    assert!(l.stats((960, 540)).starts_with("1920×1080 → 960×540"));

    // other properties
    l.apply("ab-loop-a", &Node::Double(1.5));
    l.apply("ab-loop-b", &Node::Str("no".into()));
    assert_eq!(l.ab, (Some(1.5), None));
    l.apply("loop-file", &Node::Str("inf".into()));
    assert!(l.loop_file);
    l.apply("loop-file", &Node::Flag(false));
    assert!(!l.loop_file);
    l.apply("pause", &Node::Flag(false));
    assert!(!l.paused);
    l.apply("time-pos", &Node::None); // unavailable: keep the last value
    l.apply("time-pos", &Node::Double(3.25));
    l.apply("time-pos", &Node::None);
    assert_eq!(l.pos, 3.25);
}

#[test]
fn video_chapter_list_parsing() {
    let n = Node::Array(vec![
        Node::Map(vec![
            ("title".into(), Node::Str("Intro".into())),
            ("time".into(), Node::Double(0.0)),
        ]),
        Node::Map(vec![("time".into(), Node::Int(90))]),
        Node::Map(vec![("title".into(), Node::Str("broken".into()))]),
    ]);
    assert_eq!(parse_chapters(&n), vec![0.0, 90.0]);
    assert!(parse_chapters(&Node::None).is_empty());
    assert_eq!(trim_num(30.0), "30");
    assert_eq!(trim_num(23.976_02), "23.976");
}

#[test]
fn video_key_mapping_table() {
    let none = Modifiers::empty();
    let shift = Modifiers::SHIFT;
    let ch = |s: &str| Key::Character(s.into());
    let n = Key::Named;
    use Action as A;
    let table: Vec<(Key, Modifiers, Option<Action>)> = vec![
        (n(N::Space), none, Some(A::TogglePause)),
        (ch("p"), none, Some(A::TogglePause)),
        (n(N::ArrowLeft), none, Some(A::Seek(-5.0, false))),
        (n(N::ArrowRight), none, Some(A::Seek(5.0, false))),
        (n(N::ArrowUp), none, Some(A::Seek(60.0, false))),
        (n(N::ArrowDown), none, Some(A::Seek(-60.0, false))),
        (n(N::ArrowLeft), shift, Some(A::Seek(-1.0, true))),
        (n(N::ArrowRight), shift, Some(A::Seek(1.0, true))),
        (ch("["), none, Some(A::SpeedMul(0.9))),
        (ch("]"), none, Some(A::SpeedMul(1.1))),
        (n(N::Backspace), none, Some(A::SpeedReset)),
        (ch("m"), none, Some(A::Mute)),
        (ch("9"), none, Some(A::Volume(-2.0))),
        (ch("0"), none, Some(A::Volume(2.0))),
        (ch("f"), none, Some(A::Fullscreen)),
        (n(N::Enter), none, Some(A::Fullscreen)),
        (n(N::Escape), none, Some(A::LeaveFullscreen)),
        (ch(","), none, Some(A::FrameBack)),
        (ch("."), none, Some(A::FrameStep)),
        (ch("v"), none, Some(A::SubToggle)),
        (ch("j"), none, Some(A::SubCycle)),
        (ch("#"), shift, Some(A::AudioCycle)),
        (ch("3"), shift, Some(A::AudioCycle)),
        (ch("3"), none, None),
        (ch("s"), none, Some(A::Screenshot(true))),
        (ch("S"), shift, Some(A::Screenshot(false))),
        (ch("l"), none, Some(A::AbLoop)),
        (ch("L"), shift, Some(A::LoopFile)),
        (ch("l"), shift, Some(A::LoopFile)),
        (n(N::PageUp), none, Some(A::Chapter(1))),
        (n(N::PageDown), none, Some(A::Chapter(-1))),
        (ch("<"), shift, Some(A::File(-1))),
        (ch(">"), shift, Some(A::File(1))),
        (ch("i"), none, Some(A::Stats)),
        (ch("e"), none, Some(A::Edge)),
        (ch("z"), none, Some(A::SubDelay(-0.1))),
        (ch("Z"), shift, Some(A::SubDelay(0.1))),
        (ch("+"), Modifiers::CTRL, Some(A::AudioDelay(0.1))),
        (ch("+"), Modifiers::ALT, Some(A::Zoom(0.1))),
        (n(N::Backspace), Modifiers::ALT, Some(A::ZoomReset)),
        // not ours: app shortcuts and unbound keys
        (ch("e"), Modifiers::CTRL, None),
        (ch("w"), Modifiers::CTRL, None),
        (n(N::Tab), none, None),
        (ch("q"), none, None),
        (ch("f"), Modifiers::LOGO, None),
    ];
    for (k, m, want) in table {
        assert_eq!(key_action(&k, m), want, "{k:?} {m:?}");
    }
}

#[test]
fn video_resume_store_round_trip() {
    let t = Tmp::new("resume");
    let store = t.0.join("state/og-files/video-positions.json");
    let v = t.0.join("movie.mkv");
    std::fs::write(&v, b"x").unwrap();
    assert_eq!(lookup_position(&store, &v), None);
    // the beginning isn't worth remembering (and writes nothing)
    record_position(&store, &v, 5.0, 600.0);
    assert!(!store.exists());
    record_position(&store, &v, 123.5, 600.0);
    assert_eq!(lookup_position(&store, &v), Some(123.5));
    // relative/odd spellings of the same file hit the same entry
    let alt = t.0.join("./movie.mkv");
    assert_eq!(lookup_position(&store, &alt), Some(123.5));
    // near the end → forgotten
    record_position(&store, &v, 595.0, 600.0);
    assert_eq!(lookup_position(&store, &v), None);
    assert!(load_positions(&store).is_empty());
    // unknown duration still resumes
    record_position(&store, &v, 42.0, 0.0);
    assert_eq!(lookup_position(&store, &v), Some(42.0));
    assert!(should_resume(11.0, 0.0));
    assert!(!should_resume(10.0, 600.0));
    assert!(!should_resume(580.0, 600.0)); // last 5 %
    // corrupt store → empty, then rewritten
    std::fs::write(&store, b"{not json").unwrap();
    assert_eq!(lookup_position(&store, &v), None);
    record_position(&store, &v, 50.0, 600.0);
    assert_eq!(lookup_position(&store, &v), Some(50.0));
}

#[test]
fn video_playlist_from_folder() {
    let t = Tmp::new("playlist");
    for n in ["ep10.mp4", "ep2.mkv", "Ep1.webm", "notes.txt", "cover.jpg"] {
        std::fs::write(t.0.join(n), b"x").unwrap();
    }
    std::fs::create_dir(t.0.join("dir.mp4")).unwrap(); // not a file
    let pl = folder_playlist(&t.0.join("ep2.mkv"));
    let names: Vec<String> = pl
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["Ep1.webm", "ep2.mkv", "ep10.mp4"]);
    // a file that isn't listed (e.g. vanished) still plays alone
    let lone = t.0.join("gone.mp4");
    assert_eq!(folder_playlist(&lone), vec![lone.clone()]);
    assert_eq!(step_index(0, -1, 3), 2);
    assert_eq!(step_index(2, 1, 3), 0);
    assert_eq!(step_index(1, 1, 3), 2);
    assert_eq!(step_index(0, 1, 0), 0);
}

/// Wait for `f` up to `secs`.
fn wait_until(secs: f64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// libmpv headless: vo=libmpv + SW render into a small buffer, ao=null.
#[test]
fn video_mpv_headless_playback() {
    if let Err(e) = mpv::api() {
        eprintln!("libmpv not available, skipping: {e}");
        return;
    }
    let t = Tmp::new("headless");
    let clip = t.0.join("clip.mp4");
    make_video_with(&clip, true);
    let mut opts = mpv_options(&clip, true);
    opts.retain(|(k, _)| k != "ao" && k != "pause");
    opts.push(("ao".into(), "null".into()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let p = Player::new(&opts, Some(tx)).expect("mpv player");
    p.shared.set_widget_size((160, 120));
    p.cmd(&["loadfile", &clip.to_string_lossy()]);
    assert!(
        wait_until(10.0, || p.mpv.get_f64("time-pos").unwrap_or(0.0) >= 1.0),
        "time-pos never reached 1 s (now {:?})",
        p.mpv.get_f64("time-pos")
    );
    let serial = p.shared.serial.load(Ordering::Acquire);
    assert!(serial > 5, "only {serial} frames rendered");
    {
        let f = p.shared.frame.lock().unwrap();
        // the 160×120 clip fills the 160×120 widget exactly
        assert_eq!((f.w, f.h), (160, 120));
        assert_eq!(f.stride, 768);
        let b = f.buf.bytes();
        let (mut sum, mut n) = (0u64, 0u64);
        for y in 0..f.h as usize {
            for x in 0..f.w as usize {
                let px = &b[y * f.stride + x * 4..][..3];
                sum += px.iter().map(|&v| v as u64).sum::<u64>();
                n += 3;
            }
        }
        let mean = sum / n;
        assert!(mean > 40, "frame is black (mean {mean})");
    }
    // a later position keeps advancing
    let t1 = p.mpv.get_f64("time-pos").unwrap();
    assert!(wait_until(5.0, || p.mpv.get_f64("time-pos").unwrap_or(0.0) > t1 + 0.2));
    // widget resize → re-render at the new letterboxed size
    p.shared.set_widget_size((400, 120));
    assert!(wait_until(5.0, || p.shared.frame.lock().unwrap().w == 160
        && p.shared.frame.lock().unwrap().h == 120));
    p.shared.set_widget_size((320, 480));
    assert!(wait_until(5.0, || {
        let f = p.shared.frame.lock().unwrap();
        (f.w, f.h) == (320, 240)
    }));
    // events reached the UI channel: file loaded and properties
    let mut loaded = false;
    let mut got_duration = None;
    while let Ok(e) = rx.try_recv() {
        match e {
            UiEv::FileLoaded => loaded = true,
            UiEv::Prop("duration", v) => got_duration = v.f64(),
            _ => {}
        }
    }
    assert!(loaded);
    let d = got_duration.expect("duration");
    assert!((d - 2.0).abs() < 0.1, "duration {d}");
    let tracks = parse_tracks(&p.mpv.get_node("track-list").unwrap());
    assert!(tracks.iter().any(|t| t.kind == "video" && t.selected));
    assert!(tracks.iter().any(|t| t.kind == "audio"));
    eprintln!(
        "hwdec-current: {:?}",
        p.mpv.get_str("hwdec-current").unwrap_or_default()
    );
    drop(p); // stops playback; mpv is destroyed in the background
}

/// The viewer state machine on libmpv (or the FFmpeg fallback).
#[tokio::test]
async fn video_state_controls_mpv() {
    let t = Tmp::new("state-mpv");
    let clip = t.0.join("a.mp4");
    make_video_with(&clip, true);
    std::fs::copy(&clip, t.0.join("b.mp4")).unwrap();
    let o = open(clip.clone()).await.unwrap();
    assert_eq!(o.playlist.len(), 2);
    let (mut s, _events) = State::with_store(o, None);
    let _ = s.view();
    assert!(!s.is_dirty());
    assert_eq!(s.is_editing(), Some(false));

    // fullscreen contract
    assert!(!s.fullscreen());
    assert!(s.key(&Key::Named(N::Escape), Modifiers::empty()).is_none());
    assert!(s.key(&Key::Character("f".into()), Modifiers::empty()).is_some());
    assert!(s.fullscreen());
    let _ = s.view();
    assert!(s.key(&Key::Named(N::Escape), Modifiers::empty()).is_some());
    assert!(!s.fullscreen());
    s.set_fullscreen(true);
    assert!(s.fullscreen());
    let _ = s.update(Msg::ToggleFullscreen);
    assert!(!s.fullscreen());
    // app chords aren't swallowed
    assert!(s.key(&Key::Character("e".into()), Modifiers::CTRL).is_none());
    assert!(s.key(&Key::Named(N::Space), Modifiers::empty()).is_some());

    let Inner::Mpv(m) = &mut s.inner else {
        eprintln!("libmpv not available: fallback player in use");
        return;
    };
    // properties from mpv update the view state
    let _ = m.update(Msg::Ev(UiEv::Prop("track-list", sample_tracks())));
    let _ = m.update(Msg::Ev(UiEv::Prop("duration", Node::Double(2.0))));
    let _ = m.update(Msg::Ev(UiEv::Prop("time-pos", Node::Double(1.25))));
    let _ = m.update(Msg::Ev(UiEv::FileLoaded));
    assert_eq!(m.live.tracks.len(), 4);
    assert_eq!(m.live.pos, 1.25);
    // while dragging the seek bar, mpv's time-pos doesn't fight the cursor
    let _ = m.update(Msg::SeekDrag(0.5));
    let _ = m.update(Msg::Ev(UiEv::Prop("time-pos", Node::Double(1.9))));
    assert_eq!(m.live.pos, 0.5);
    let _ = m.update(Msg::SeekRelease);
    assert!(m.drag.is_none());
    for msg in [
        Msg::Volume(140.0),
        Msg::VolumeBy(-20.0),
        Msg::SpeedMul(10.0),
        Msg::ToggleMute,
        Msg::SeekBy(1.0, true),
        Msg::FrameStep,
        Msg::FrameBack,
        Msg::ToggleSubs,
        Msg::CycleSub,
        Msg::CycleAudio,
        Msg::SubDelay(0.1),
        Msg::AudioDelay(-0.1),
        Msg::AbLoop,
        Msg::LoopFile,
        Msg::CycleAspect,
        Msg::Rotate,
        Msg::Zoom(0.5),
        Msg::Pan(0.1, 0.1),
        Msg::ZoomReset,
        Msg::Wheel(1.0, false),
        Msg::Wheel(1.0, true),
        Msg::Chapter(1),
        Msg::ToggleTimeMode,
        Msg::ToggleStats,
        Msg::ToggleEdge,
        Msg::Motion,
    ] {
        let _ = m.update(msg);
    }
    assert_eq!(m.live.volume, 112.0); // clamped to 130, −20, wheel +2
    assert_eq!(m.live.speed, 4.0);
    assert_eq!(m.live.rotate, 90);
    m.live.meta_rotate = 270;
    assert_eq!(m.live.display_rotate(), 0);
    assert_eq!(quarter_turns(-90), 3);
    assert_eq!(quarter_turns(450), 1);
    assert_eq!(m.aspect_ix, 1);
    assert!(m.remaining && m.stats && !m.edge);
    let _ = m.view(false);
    let _ = m.view(true);

    // edit mode marks + refusals
    let _ = m.update(Msg::ToggleEdit);
    m.live.pos = 1.5;
    let _ = m.update(Msg::SetOut);
    m.live.pos = 0.5;
    let _ = m.update(Msg::SetIn);
    assert_eq!((m.mark_in, m.mark_out), (Some(0.5), Some(1.5)));
    let _ = m.view(false);
    let _ = m.update(Msg::ClearMarks);
    m.live.pos = 0.0;
    let _ = m.update(Msg::SetOut);
    let (_, e) = m.update(Msg::Export);
    assert!(matches!(e, Some(Event::Status(s)) if s.contains("Out must be after In")));
    let (_, e) = m.update(Msg::ScreenshotDone(Ok("/x/a-frame-000001.jpg".into())));
    assert!(matches!(e, Some(Event::Status(s)) if s.contains("a-frame-000001")));
    let (_, e) = m.update(Msg::Ev(UiEv::EndFile(Some("unknown format".into()))));
    assert!(matches!(e, Some(Event::Status(s)) if s.contains("unknown format")));

    // playlist: next file inside the viewer
    let (_, e) = m.update(Msg::File(1));
    assert!(matches!(e, Some(Event::Status(s)) if s.contains("b.mp4 (2 / 2)")));
    assert_eq!(m.path, t.0.join("b.mp4"));
    assert!(m.mark_out.is_none() && m.error.is_none());
    let _ = m.update(Msg::File(1));
    assert_eq!(m.path, clip);

    // a snapshot through mpv lands beside the video
    let m2 = m.player.mpv.clone();
    assert!(wait_until(10.0, || m2.get_f64("duration").unwrap_or(0.0) > 0.0));
    let (task, _) = m.update(Msg::Snapshot);
    drop(task);
    let dst = t.0.join("a-frame-00m00s.png");
    let r = tokio::task::spawn_blocking(move || {
        // the reload just started: retry until a frame is decoded
        let mut r = Err(String::new());
        wait_until(10.0, || {
            r = m2.command(&["screenshot-to-file", &dst.to_string_lossy(), "video"]);
            r.is_ok()
        });
        r.map(|()| dst)
    })
    .await
    .unwrap();
    let dst = r.expect("screenshot-to-file");
    let im = image::open(&dst).unwrap();
    // the 16:9 aspect override set above widens the 160×120 frame
    assert_eq!(im.height(), 120);
    assert!(im.width() >= 160, "{}", im.width());
    drop(s); // saves (no store here) and stops mpv
}

/// The render buffer is exactly the fitted video rect (no padding inside),
/// drawn 1:1 and centred in the widget; the rest shows the app background.
#[test]
fn video_fit_has_no_padding_and_is_centred() {
    for (widget, aspect) in [
        ((1000u32, 600u32), 16.0 / 9.0), // width-limited → space above/below
        ((1000, 400), 16.0 / 9.0),       // height-limited → space left/right
        ((640, 360), 16.0 / 9.0),        // exact fit
        ((500, 900), 2.35),
        ((900, 900), 3.0 / 4.0),
    ] {
        let (bw, bh) = mpv::target_size(widget, Some(aspect));
        // buffer has the video's aspect (to rounding) and fits the widget
        assert!(bw <= widget.0 && bh <= widget.1);
        assert!(bw == widget.0 || bh == widget.1, "touches one side");
        assert!(((bw as f64 / bh as f64) - aspect).abs() < 0.01);
        let (x, y) = (10.0, 20.0);
        let r = screen::letterbox((bw, bh), [x, y, widget.0 as f32, widget.1 as f32]);
        // drawn at buffer size: no scaling, no padding
        assert!((r[2] - bw as f32).abs() < 0.51 && (r[3] - bh as f32).abs() < 0.51);
        // centred: equal margins on both sides
        let (ml, mr) = (r[0] - x, x + widget.0 as f32 - (r[0] + r[2]));
        let (mt, mb) = (r[1] - y, y + widget.1 as f32 - (r[1] + r[3]));
        assert!((ml - mr).abs() < 0.01 && (mt - mb).abs() < 0.01, "{widget:?}");
    }
}
