//! Turning a page into something a model can read.
//!
//! Most of what a browser does is irrelevant here. The model needs the article
//! text, the title and the links; it does not need layout, fonts or a paint
//! pipeline. Stripping HTML down to that is a parsing job, not a rendering one,
//! and it costs kilobytes instead of hundreds of megabytes.
//!
//! What this cannot do is honest and worth repeating: a page whose content is
//! assembled by JavaScript after load arrives here as an empty shell. That gap
//! is reported rather than hidden, because a model handed an empty page will
//! confidently summarise nothing.

use serde::Serialize;

/// Tags whose contents are never readable text.
const DROPPED: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "canvas", "iframe",
    "head", "nav", "footer", "form",
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub text: String,
    pub links: Vec<Link>,
    /// True when the page looks like an empty shell waiting for JavaScript.
    pub needs_javascript: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Link {
    pub text: String,
    pub href: String,
}

impl Page {
    pub fn word_count(&self) -> usize {
        self.text.split_whitespace().count()
    }
}

/// Extract readable content from HTML.
pub fn extract(url: &str, html: &str) -> Page {
    let title = first_tag_text(html, "title").unwrap_or_default();
    let body = strip_dropped_tags(html);
    let text = tags_to_text(&body);
    let links = collect_links(html, url);

    // A page with a handful of words but plenty of script tags is a shell.
    // Judged on both counts because a genuinely short page is not a failure.
    let script_count = count_occurrences(&html.to_lowercase(), "<script");
    let words = text.split_whitespace().count();
    let needs_javascript = words < 40 && script_count >= 2;

    Page { url: url.to_string(), title, text, links, needs_javascript }
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// Text of the first `<tag>...</tag>`, decoded.
fn first_tag_text(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start = lower.find(&open)?;
    let content_start = start + lower[start..].find('>')? + 1;
    let end = lower[content_start..].find(&close)? + content_start;
    Some(decode_entities(html[content_start..end].trim()))
}

/// Remove tags whose contents are not readable, with their contents.
fn strip_dropped_tags(html: &str) -> String {
    let mut out = html.to_string();
    for tag in DROPPED {
        loop {
            let lower = out.to_lowercase();
            let open = format!("<{tag}");
            let close = format!("</{tag}>");
            let Some(start) = lower.find(&open) else { break };
            match lower[start..].find(&close) {
                Some(rel) => {
                    let end = start + rel + close.len();
                    out.replace_range(start..end, " ");
                }
                // An unclosed dropped tag means the rest of the document is
                // inside it. Truncating is safer than keeping script source.
                None => {
                    out.truncate(start);
                    break;
                }
            }
        }
    }
    out
}

/// Replace remaining tags with spacing and collapse whitespace.
///
/// Block-level tags become line breaks so paragraphs survive; everything else
/// becomes a space, so `a<b>b</b>c` does not become `abc`.
fn tags_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        let mut tag = String::new();
        for t in chars.by_ref() {
            if t == '>' {
                break;
            }
            tag.push(t);
        }
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        out.push(match name.as_str() {
            "p" | "div" | "br" | "li" | "tr" | "h1" | "h2" | "h3" | "h4"
            | "h5" | "h6" | "section" | "article" | "blockquote" => '\n',
            _ => ' ',
        });
    }

    let decoded = decode_entities(&out);
    // Collapse runs of blank lines, and runs of spaces, without losing the
    // paragraph structure that makes the text readable.
    decoded
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect_links(html: &str, base: &str) -> Vec<Link> {
    let mut links = Vec::new();
    let lower = html.to_lowercase();
    let mut pos = 0;

    while let Some(rel) = lower[pos..].find("<a ") {
        let start = pos + rel;
        let Some(tag_end) = lower[start..].find('>') else { break };
        let tag = &html[start..start + tag_end];

        if let Some(href) = attribute(tag, "href") {
            let close = lower[start..].find("</a>").map(|r| start + r);
            let text = close
                .map(|c| tags_to_text(&html[start + tag_end + 1..c]))
                .unwrap_or_default();
            let resolved = resolve(base, &href);
            if !resolved.is_empty() {
                links.push(Link { text: text.trim().to_string(), href: resolved });
            }
        }
        pos = start + tag_end + 1;
    }
    links
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_lowercase();
    let at = lower.find(&format!("{name}="))?;
    let rest = &tag[at + name.len() + 1..];
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let end = rest[1..].find(quote)? + 1;
        Some(decode_entities(&rest[1..end]))
    } else {
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        Some(decode_entities(&rest[..end]))
    }
}

/// Turn a possibly relative href into an absolute URL.
///
/// Deliberately conservative: anything it cannot resolve confidently becomes
/// empty and is dropped, because a wrong URL handed to a model is worse than
/// a missing one.
fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
        return String::new();
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let Some(scheme_end) = base.find("://") else { return String::new() };
    let after = &base[scheme_end + 3..];
    let host_end = after.find('/').map(|i| scheme_end + 3 + i).unwrap_or(base.len());
    let origin = &base[..host_end];

    if href.starts_with("//") {
        return format!("{}:{href}", &base[..scheme_end]);
    }
    if href.starts_with('/') {
        return format!("{origin}{href}");
    }
    // Relative to the current directory.
    let dir_end = base.rfind('/').filter(|i| *i > host_end).unwrap_or(host_end);
    format!("{}/{href}", &base[..dir_end])
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: &str = r#"<!doctype html><html><head>
      <title>Riyadh Season 2026</title>
      <style>body{color:red}</style>
      <script>var tracking = 1;</script>
    </head><body>
      <nav><a href="/home">Home</a></nav>
      <article>
        <h1>The headline</h1>
        <p>First paragraph with <b>bold</b> text.</p>
        <p>Second paragraph &amp; an entity.</p>
        <a href="/next">Next page</a>
        <a href="https://example.test/abs">Absolute</a>
      </article>
      <footer><a href="/privacy">Privacy</a></footer>
    </body></html>"#;

    #[test]
    fn the_title_is_extracted() {
        let p = extract("https://site.test/a/b", ARTICLE);
        assert_eq!(p.title, "Riyadh Season 2026");
    }

    #[test]
    fn script_and_style_contents_never_reach_the_text() {
        let p = extract("https://site.test/", ARTICLE);
        assert!(!p.text.contains("tracking"), "script body leaked: {}", p.text);
        assert!(!p.text.contains("color:red"), "style body leaked");
    }

    #[test]
    fn navigation_and_footer_are_dropped_as_chrome() {
        let p = extract("https://site.test/", ARTICLE);
        assert!(!p.text.contains("Home"));
        assert!(!p.text.contains("Privacy"));
        assert!(p.text.contains("First paragraph"));
    }

    #[test]
    fn inline_tags_do_not_glue_words_together() {
        // `with <b>bold</b> text` must not become `withboldtext`.
        let p = extract("https://site.test/", ARTICLE);
        assert!(p.text.contains("bold"), "{}", p.text);
        assert!(!p.text.contains("withbold"), "{}", p.text);
    }

    #[test]
    fn paragraphs_survive_as_separate_lines() {
        let p = extract("https://site.test/", ARTICLE);
        let lines: Vec<&str> = p.text.lines().collect();
        assert!(lines.len() >= 3, "paragraph structure lost: {:?}", lines);
    }

    #[test]
    fn entities_are_decoded() {
        let p = extract("https://site.test/", ARTICLE);
        assert!(p.text.contains("& an entity"), "{}", p.text);
    }

    #[test]
    fn relative_links_resolve_against_the_page() {
        let p = extract("https://site.test/a/b", ARTICLE);
        let hrefs: Vec<&str> = p.links.iter().map(|l| l.href.as_str()).collect();
        assert!(hrefs.contains(&"https://site.test/next"), "{hrefs:?}");
        assert!(hrefs.contains(&"https://example.test/abs"), "{hrefs:?}");
    }

    #[test]
    fn anchors_and_javascript_hrefs_are_dropped() {
        // A link a model cannot follow is worse than no link.
        // Needs a longer delimiter: `href="#top"` contains `"#`.
        let html = r##"<a href="#top">Top</a><a href="javascript:void(0)">JS</a>"##;
        assert!(extract("https://site.test/", html).links.is_empty());
    }

    #[test]
    fn arabic_text_survives_intact() {
        let html = "<html><body><p>مرحباً بكم في كاربون فلو</p></body></html>";
        let p = extract("https://site.test/", html);
        assert!(p.text.contains("مرحباً بكم في كاربون فلو"), "{}", p.text);
    }

    #[test]
    fn a_javascript_shell_is_flagged_not_returned_as_empty() {
        // The honest failure mode: say the page needs a browser rather than
        // hand a model an empty document to summarise.
        let shell = r#"<html><head><title>App</title></head><body>
            <div id="root"></div>
            <script src="/a.js"></script>
            <script src="/b.js"></script>
        </body></html>"#;
        let p = extract("https://app.test/", shell);
        assert!(p.needs_javascript, "an empty shell must be flagged");
        assert!(p.word_count() < 40);
    }

    #[test]
    fn a_genuinely_short_page_is_not_mistaken_for_a_shell() {
        let short = "<html><body><p>Not found.</p></body></html>";
        assert!(!extract("https://site.test/", short).needs_javascript);
    }

    #[test]
    fn an_unclosed_script_truncates_rather_than_leaking_source() {
        let broken = "<html><body><p>Visible</p><script>secret_key = 'abc'";
        let p = extract("https://site.test/", broken);
        assert!(p.text.contains("Visible"));
        assert!(!p.text.contains("secret_key"), "leaked: {}", p.text);
    }

    #[test]
    fn malformed_html_does_not_panic() {
        for junk in ["", "<", "<<<>>>", "<a href=", "</p></p>", "<html"] {
            let _ = extract("https://site.test/", junk);
        }
    }
}
