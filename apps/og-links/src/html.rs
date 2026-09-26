use crate::config::Config;
use crate::store::Alias;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn home_page(aliases: &[Alias], config: &Config, notice: Option<&str>, prefill_key: &str) -> String {
    let rows: String = if aliases.is_empty() {
        r#"<tr><td colspan="3" class="empty">No shortcuts yet — add one below.</td></tr>"#.to_string()
    } else {
        aliases.iter().map(|a| {
            format!(
                r#"<tr>
                    <td><code>g/{key}</code></td>
                    <td class="url"><a href="{url}">{url}</a></td>
                    <td>
                        <form action="/delete" method="POST" onsubmit="return confirm('Delete g/{key}?');">
                            <input type="hidden" name="key" value="{key}">
                            <button type="submit" class="danger">Delete</button>
                        </form>
                    </td>
                </tr>"#,
                key = escape(&a.key),
                url = escape(&a.url),
            )
        }).collect()
    };

    let notice_html = notice.map(|n| format!(
        r#"<div class="notice">{}</div>"#,
        escape(n),
    )).unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>g/ — Shortcuts</title>
<style>
:root {{
    --bg: {bar_bg};
    --card-bg: {sec_bg};
    --text: {bar_text};
    --accent: {accent};
    --danger: {urgent};
}}
* {{ box-sizing: border-box; }}
body {{
    margin: 0;
    padding: 40px 24px;
    background: var(--bg);
    color: var(--text);
    font-family: system-ui, sans-serif;
    display: flex;
    justify-content: center;
}}
.wrap {{ width: 100%; max-width: 720px; }}
h1 {{
    font-size: 28px;
    margin: 0 0 4px 0;
}}
h1 .g {{ color: var(--accent); }}
.subtitle {{ color: var(--text); opacity: 0.6; margin: 0 0 24px 0; font-size: 14px; }}
.card {{
    background: var(--card-bg);
    border-radius: 10px;
    padding: 20px 24px;
    margin-bottom: 20px;
}}
.notice {{
    background: var(--card-bg);
    border: 1px solid var(--accent);
    color: var(--text);
    border-radius: 8px;
    padding: 14px 18px;
    margin-bottom: 20px;
}}
table {{ width: 100%; border-collapse: collapse; }}
th {{
    text-align: left;
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    opacity: 0.5;
    padding: 6px 8px;
    border-bottom: 1px solid rgba(255,255,255,0.1);
}}
td {{
    padding: 10px 8px;
    border-bottom: 1px solid rgba(255,255,255,0.06);
    vertical-align: middle;
}}
td.empty {{ opacity: 0.5; text-align: center; padding: 24px 8px; }}
code {{
    background: rgba(255,255,255,0.08);
    padding: 2px 6px;
    border-radius: 4px;
    color: var(--accent);
}}
.url a {{ color: var(--text); opacity: 0.75; text-decoration: none; word-break: break-all; }}
.url a:hover {{ opacity: 1; text-decoration: underline; }}
form.add {{ display: flex; gap: 10px; flex-wrap: wrap; }}
input {{
    background: var(--bg);
    border: 1px solid rgba(255,255,255,0.15);
    color: var(--text);
    border-radius: 6px;
    padding: 10px 12px;
    font-size: 14px;
}}
input[name="key"] {{ width: 140px; }}
input[name="url"] {{ flex: 1; min-width: 200px; }}
button {{
    background: var(--accent);
    color: var(--bg);
    border: none;
    border-radius: 6px;
    padding: 10px 18px;
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
}}
button:hover {{ filter: brightness(1.1); }}
button.danger {{
    background: transparent;
    color: var(--danger);
    border: 1px solid var(--danger);
    padding: 6px 12px;
    font-weight: 400;
}}
button.danger:hover {{ background: var(--danger); color: var(--bg); }}
</style>
</head>
<body>
<div class="wrap">
    <h1><span class="g">g</span>/</h1>
    <p class="subtitle">Local shortcut links for this machine. Type <code>g/&lt;name&gt;</code> in your address bar.</p>
    {notice_html}
    <div class="card">
        <table>
            <tr><th>Shortcut</th><th>Destination</th><th></th></tr>
            {rows}
        </table>
    </div>
    <div class="card">
        <form class="add" action="/add" method="POST">
            <input type="text" name="key" placeholder="name" value="{prefill_key}" required>
            <input type="text" name="url" placeholder="https://destination.example.com" required>
            <button type="submit">Add Shortcut</button>
        </form>
    </div>
</div>
</body>
</html>"#,
        bar_bg = config.bar_bg,
        sec_bg = config.sec_bg,
        bar_text = config.bar_text,
        accent = config.accent,
        urgent = config.urgent_color,
        notice_html = notice_html,
        rows = rows,
        prefill_key = escape(prefill_key),
    )
}
