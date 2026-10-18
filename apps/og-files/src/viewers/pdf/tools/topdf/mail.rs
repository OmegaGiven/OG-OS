//! E-mail (.eml): a header block (Subject, From, To, Cc, Date), the HTML
//! body (or the plain-text one), then the attachments listed by name and
//! size, with picture attachments shown. `cid:` images referenced by the
//! HTML body are resolved from the message's own parts.

use super::html;
use super::ir::{Block, Cell, Doc, Inline, Page, Style, Table};
use mail_parser::{Address, MessageParser, MimeHeaders};

fn addresses(a: Option<&Address<'_>>) -> Option<String> {
    let list: Vec<String> = a?
        .iter()
        .map(|x| match (x.name(), x.address()) {
            (Some(n), Some(e)) if !n.trim().is_empty() => format!("{} <{e}>", n.trim()),
            (_, Some(e)) => e.to_string(),
            (Some(n), None) => n.to_string(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect();
    (!list.is_empty()).then(|| list.join(", "))
}

fn human_size(n: usize) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

/// Plain-text body: blank lines split paragraphs, line breaks stay.
pub fn text_blocks(s: &str) -> Vec<Block> {
    let s = s.replace("\r\n", "\n");
    let mut out = Vec::new();
    for para in s.split("\n\n") {
        let para = para.trim_matches('\n');
        if para.trim().is_empty() {
            continue;
        }
        let mut inl = Vec::new();
        for (k, line) in para.split('\n').enumerate() {
            if k > 0 {
                inl.push(Inline::Break);
            }
            if !line.is_empty() {
                inl.push(Inline::Text(line.to_string(), Style::default()));
            }
        }
        out.push(Block::para(inl));
    }
    out
}

pub fn convert(bytes: &[u8]) -> Result<Doc, String> {
    let msg = MessageParser::default()
        .parse(bytes)
        .ok_or("not a readable e-mail message")?;
    let mut d = Doc::new(Page::A4);
    let subject = msg.subject().map(str::trim).filter(|s| !s.is_empty());
    d.title = subject.map(str::to_string);

    d.blocks.push(Block::Heading(
        1,
        vec![Inline::Text(
            subject.unwrap_or("(no subject)").to_string(),
            Style::default(),
        )],
    ));
    let mut rows = Vec::new();
    let label = Style {
        bold: true,
        color: Some([90, 90, 90]),
        ..Default::default()
    };
    let fields = [
        ("From", addresses(msg.from())),
        ("To", addresses(msg.to())),
        ("Cc", addresses(msg.cc())),
        ("Reply-To", addresses(msg.reply_to())),
        ("Date", msg.date().map(|d| d.to_rfc822())),
    ];
    for (k, v) in fields {
        if let Some(v) = v {
            rows.push(vec![
                Cell::text(k, label.clone()),
                Cell::text(v, Style::default()),
            ]);
        }
    }
    if !rows.is_empty() {
        d.blocks.push(Block::Table(Table {
            rows,
            borders: false,
            ..Default::default()
        }));
    }
    d.blocks.push(Block::Rule);

    // parts referenced from the HTML body (cid:…) are not repeated below
    let mut used_cids: Vec<String> = Vec::new();
    if let Some(body) = msg.body_html(0) {
        let parts = &msg.parts;
        let mut fetch = |src: &str| -> Option<Vec<u8>> {
            let cid = src.trim().strip_prefix("cid:")?;
            let cid = html::percent_decode(cid);
            let part = parts.iter().find(|p| {
                p.content_id()
                    .is_some_and(|id| id.trim_matches(['<', '>']) == cid.trim_matches(['<', '>']))
            })?;
            used_cids.push(cid);
            Some(part.contents().to_vec())
        };
        let out = html::convert(&body, &mut d.files, &mut fetch, None);
        d.blocks.extend(out.blocks);
    } else if let Some(text) = msg.body_text(0) {
        d.blocks.extend(text_blocks(&text));
    }

    let mut listed = Vec::new();
    let mut pictures = Vec::new();
    for part in msg.attachments() {
        let inline_used = part.content_id().is_some_and(|id| {
            used_cids
                .iter()
                .any(|c| c.trim_matches(['<', '>']) == id.trim_matches(['<', '>']))
        });
        if inline_used {
            continue;
        }
        let name = part
            .attachment_name()
            .map(str::to_string)
            .unwrap_or_else(|| "(unnamed)".into());
        let ctype = part
            .content_type()
            .map(|c| match c.subtype() {
                Some(s) => format!("{}/{s}", c.ctype()),
                None => c.ctype().to_string(),
            })
            .unwrap_or_default();
        listed.push(format!(
            "{name} — {} ({ctype})",
            human_size(part.contents().len())
        ));
        if ctype.starts_with("image/") {
            pictures.push(part.contents().to_vec());
        }
    }
    if !listed.is_empty() {
        d.blocks.push(Block::Rule);
        d.blocks.push(Block::Heading(
            2,
            vec![Inline::Text("Attachments".into(), Style::default())],
        ));
        d.blocks.push(Block::List {
            ordered: false,
            start: 1,
            items: listed.into_iter().map(|s| vec![Block::plain(s)]).collect(),
        });
        for p in pictures {
            if let Some(img) = d.files.add(p) {
                d.blocks.push(Block::Image(img, super::ir::Align::Left));
            }
        }
    }
    Ok(d)
}
