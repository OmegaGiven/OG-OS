//! macOS "Open with": the app bundles in /Applications, ~/Applications and
//! /System/Applications (plus one level of sub-folders such as Utilities),
//! read from each bundle's `Contents/Info.plist` (XML or binary) into the
//! same [`App`] records the Linux desktop-entry code produces. An app is
//! launched with `open -a <bundle> <files>`; the system default is just
//! `open <files>` (Launch Services decides), so no default is guessed here.
//!
//! Pure file reading: compiled on every OS so the tests run everywhere.

use crate::openwith::App;
use std::path::{Path, PathBuf};

/// Folders searched, most important first (a bundle id seen earlier wins).
#[cfg(target_os = "macos")]
pub fn app_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/Applications"),
        home.join("Applications"),
        PathBuf::from("/System/Applications"),
    ]
}

/// `s` inside a double-quoted Exec argument (desktop-entry quoting, which
/// `openwith::split_exec` undoes): `"` `` ` `` `$` `\` escaped, `%` doubled.
pub fn exec_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Uniform Type Identifiers that stand for a whole MIME family.
fn uti_mimes(uti: &str) -> &'static [&'static str] {
    match uti {
        "public.image" | "public.jpeg" | "public.png" | "public.tiff" | "com.compuserve.gif" => {
            &["image/*"]
        }
        "public.movie"
        | "public.video"
        | "public.audiovisual-content"
        | "public.mpeg-4"
        | "com.apple.quicktime-movie" => &["video/*"],
        "public.audio" | "public.mp3" | "public.mpeg-4-audio" => &["audio/*"],
        "public.text" | "public.source-code" | "public.script" => &["text/*"],
        "public.plain-text" | "public.utf8-plain-text" => &["text/plain"],
        "public.html" => &["text/html"],
        "public.json" => &["application/json"],
        "public.xml" => &["application/xml", "text/xml"],
        "net.daringfireball.markdown" => &["text/markdown"],
        "com.adobe.pdf" => &["application/pdf"],
        "public.rtf" => &["text/rtf"],
        "public.comma-separated-values-text" => &["text/csv"],
        "public.zip-archive" => &["application/zip"],
        "org.openxmlformats.wordprocessingml.document" => {
            &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"]
        }
        "org.openxmlformats.spreadsheetml.sheet" => {
            &["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"]
        }
        _ => &[],
    }
}

/// One bundle → App (None when it has no usable Info.plist).
pub fn parse_bundle(bundle: &Path) -> Option<App> {
    let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    let d = info.as_dictionary()?;
    let s = |k: &str| d.get(k).and_then(plist::Value::as_string).unwrap_or("");
    let stem = bundle.file_stem()?.to_string_lossy().into_owned();
    // background agents / helpers aren't apps you open files with
    let flag = |k: &str| {
        d.get(k)
            .is_some_and(|v| v.as_boolean() == Some(true) || v.as_string() == Some("1"))
    };
    let name = [s("CFBundleDisplayName"), s("CFBundleName")]
        .into_iter()
        .find(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| stem.clone());
    let id = match s("CFBundleIdentifier") {
        "" => stem,
        i => i.to_string(),
    };
    let mut mime_types: Vec<String> = Vec::new();
    let mut add = |m: String| {
        if !mime_types.contains(&m) {
            mime_types.push(m);
        }
    };
    let types = d
        .get("CFBundleDocumentTypes")
        .and_then(plist::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for t in types.iter().filter_map(plist::Value::as_dictionary) {
        let list = |k: &str| -> Vec<String> {
            t.get(k)
                .and_then(plist::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(plist::Value::as_string)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        for ext in list("CFBundleTypeExtensions") {
            let ext = ext.trim_start_matches('.').to_ascii_lowercase();
            if ext.is_empty() || ext == "*" {
                continue; // "opens anything" isn't a recommendation
            }
            for m in mime_guess::from_ext(&ext).iter_raw() {
                add(m.to_string());
            }
        }
        for m in list("CFBundleTypeMIMETypes") {
            add(m.to_ascii_lowercase());
        }
        for uti in list("LSItemContentTypes") {
            for m in uti_mimes(&uti) {
                add(m.to_string());
            }
        }
    }
    Some(App {
        id,
        file: bundle.to_path_buf(),
        exec: format!("open -a {} %F", exec_quote(&bundle.to_string_lossy())),
        no_display: flag("LSUIElement") || flag("LSBackgroundOnly"),
        name,
        mime_types,
        ..Default::default()
    })
}

/// Every app bundle in `dirs` (and their non-bundle sub-folders, one level
/// deep), first id wins, sorted by name.
pub fn scan(dirs: &[PathBuf]) -> Vec<App> {
    fn bundles(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        v.sort();
        for p in v {
            if !p.is_dir() {
                continue;
            }
            if p.extension().is_some_and(|e| e == "app") {
                out.push(p);
            } else if depth == 0 {
                bundles(&p, 1, out);
            }
        }
    }
    let mut found = Vec::new();
    for d in dirs {
        bundles(d, 0, &mut found);
    }
    let mut seen = std::collections::HashSet::new();
    let mut apps: Vec<App> = found
        .iter()
        .filter_map(|b| parse_bundle(b))
        .filter(|a| seen.insert(a.id.clone()))
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openwith::{command_lines, split_exec};

    fn plist_xml(id: &str, name: &str, docs: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{id}</string>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundleDocumentTypes</key><array>{docs}</array>
</dict></plist>"#
        )
    }

    fn bundle(root: &Path, rel: &str, plist: &str) -> PathBuf {
        let b = root.join(rel);
        std::fs::create_dir_all(b.join("Contents")).unwrap();
        std::fs::write(b.join("Contents/Info.plist"), plist).unwrap();
        b
    }

    #[test]
    fn mac_app_bundle_discovery() {
        let root = crate::config::home()
            .join(".cache/ogx-macapps-tests")
            .join(format!("scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let apps_dir = root.join("Applications");
        let preview = bundle(
            &apps_dir,
            "Preview.app",
            &plist_xml(
                "com.apple.Preview",
                "Preview",
                "<dict><key>LSItemContentTypes</key><array><string>com.adobe.pdf</string>\
                 <string>public.image</string></array></dict>",
            ),
        );
        bundle(
            &apps_dir,
            "Utilities/Text $Edit.app",
            &plist_xml(
                "com.example.textedit",
                "Text Edit",
                "<dict><key>CFBundleTypeExtensions</key><array><string>md</string>\
                 <string>*</string></array></dict>",
            ),
        );
        // not an app / no plist / duplicate id in a later folder
        std::fs::create_dir_all(apps_dir.join("Broken.app")).unwrap();
        std::fs::write(apps_dir.join("notes.txt"), "x").unwrap();
        bundle(
            &root.join("home/Applications"),
            "Preview copy.app",
            &plist_xml("com.apple.Preview", "Preview 2", ""),
        );
        // binary plists are read too
        let mut bin = Vec::new();
        plist::Value::from_reader_xml(
            plist_xml("org.videolan.vlc", "VLC", "<dict><key>LSItemContentTypes</key><array><string>public.movie</string></array></dict>").as_bytes(),
        )
        .unwrap()
        .to_writer_binary(&mut bin)
        .unwrap();
        let vlc = apps_dir.join("VLC.app/Contents");
        std::fs::create_dir_all(&vlc).unwrap();
        std::fs::write(vlc.join("Info.plist"), bin).unwrap();

        let apps = scan(&[apps_dir.clone(), root.join("home/Applications")]);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Preview", "Text Edit", "VLC"]);
        let p = &apps[0];
        assert_eq!(p.id, "com.apple.Preview");
        assert_eq!(p.file, preview);
        assert!(p.handles("application/pdf") && p.handles("image/png"));
        assert!(!p.handles("text/plain"));
        let t = &apps[1];
        assert!(t.handles("text/markdown"), "{:?}", t.mime_types);
        assert!(!t.handles("image/png"), "'*' is not a recommendation");
        assert!(apps[2].handles("video/mp4"));

        // the Exec line round-trips the bundle path through spec quoting
        let argv =
            &command_lines(t, &[PathBuf::from("/x/a b.md"), "/y.md".into()], None).unwrap()[0];
        assert_eq!(
            argv[..3],
            [
                "open",
                "-a",
                &*root
                    .join("Applications/Utilities/Text $Edit.app")
                    .to_string_lossy()
            ]
        );
        assert_eq!(argv[3..], ["/x/a b.md", "/y.md"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mac_exec_quoting() {
        for s in ["/A/b c.app", "/q\"uo`te$\\.app", "/100%.app"] {
            let line = format!("open -a {} %F", exec_quote(s));
            let args = split_exec(&line).unwrap();
            assert_eq!(args[2].0.replace("%%", "%"), s, "{line}");
        }
        assert_eq!(exec_quote("50%"), "\"50%%\"");
    }
}
