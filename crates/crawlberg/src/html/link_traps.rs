//! Links a person cannot see, which only a crawler would follow.
//!
//! A crawler trap is a URL that is in the markup but not on the screen: an anchor hidden by an
//! inline style, the `hidden` attribute, an off-screen or zero-size box, a hiding utility class,
//! or a class the page's own `<style>` hides. Anti-bot systems plant them and flag whoever
//! requests one, so [`super::links::extract_links`] drops them, and with them the crawl and map
//! never reach them.
//!
//! Judged from the markup alone, with no layout or stylesheet engine: a link hidden by an
//! external stylesheet or by script is not caught. Prowler applies the same rules to its own
//! fetcher (`app/engines/link_traps.py`); keep the two in step.

use std::collections::HashSet;

use tl::{HTMLTag, VDom};

use super::get_attr;

/// Utility classes that hide an element in the common CSS frameworks (Bootstrap, Tailwind,
/// WordPress, Foundation), plus the literal name traps tend to carry.
static HIDING_CLASSES: &[&str] = &[
    "hidden",
    "d-none",
    "invisible",
    "hide",
    "honeypot",
    "visually-hidden",
    "sr-only",
    "screen-reader-text",
];

/// An offset this far negative puts the box off any screen.
const OFFSCREEN: f64 = 999.0;

/// The number a CSS value starts with: `-9999px` is -9999, `0` is 0, `auto` is none.
fn leading_number(value: &str) -> Option<f64> {
    let value = value.trim();
    let digits = value.strip_prefix('-').unwrap_or(value);
    let int_len = digits.bytes().take_while(u8::is_ascii_digit).count();
    if int_len == 0 {
        return None;
    }
    let mut len = int_len;
    if let Some(frac) = digits[int_len..].strip_prefix('.') {
        let frac_len = frac.bytes().take_while(u8::is_ascii_digit).count();
        if frac_len > 0 {
            len += 1 + frac_len;
        }
    }
    let sign_len = value.len() - digits.len();
    value[..sign_len + len].parse().ok()
}

/// Whether an inline `style`, or a rule body, makes the element invisible.
pub(crate) fn style_hides(style: &str) -> bool {
    let style = style.to_ascii_lowercase();
    // The last declaration of a property wins, as in CSS.
    let mut decls: Vec<(&str, String)> = Vec::new();
    for part in style.split(';') {
        if let Some((name, value)) = part.split_once(':') {
            decls.push((name.trim(), value.replace("!important", "").trim().to_owned()));
        }
    }
    let get = |name: &str| decls.iter().rev().find(|(n, _)| *n == name).map(|(_, v)| v.as_str());
    let is_zero = |name: &str| get(name).and_then(leading_number) == Some(0.0);

    if get("display") == Some("none") || matches!(get("visibility"), Some("hidden" | "collapse")) {
        return true;
    }
    if is_zero("opacity") || is_zero("font-size") {
        return true;
    }
    let offscreen = ["left", "top", "text-indent", "margin-left", "margin-top"]
        .iter()
        .any(|name| get(name).and_then(leading_number).is_some_and(|n| n <= -OFFSCREEN));
    if offscreen {
        return true;
    }
    let squeezed = |name: &str| get(name).map(|v| v.replace(' ', ""));
    if squeezed("clip").is_some_and(|v| v.starts_with("rect(")) {
        return true;
    }
    if squeezed("clip-path").is_some_and(|v| v.starts_with("inset(50%")) {
        return true;
    }
    is_zero("width") && is_zero("height")
}

/// Classes the page's own `<style>` blocks hide, from simple `.name { ... }` rules.
pub(crate) fn hiding_classes_from_styles<'s>(css_texts: impl IntoIterator<Item = &'s str>) -> HashSet<String> {
    let mut hidden = HashSet::new();
    for css in css_texts {
        for chunk in css.split('}') {
            let Some((before, body)) = chunk.rsplit_once('{') else {
                continue;
            };
            if !style_hides(body) {
                continue;
            }
            // Inside an at-rule (`@media x { .a { ... } }`) the selectors follow its `{`.
            let selectors = before.rsplit('{').next().unwrap_or(before);
            for selector in selectors.split(',') {
                let selector = selector.trim();
                if let Some(name) = selector.strip_prefix('.')
                    && !name.is_empty()
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    hidden.insert(name.to_owned());
                }
            }
        }
    }
    hidden
}

/// Whether one element, by its own attributes, hides itself and everything inside it.
fn element_hides(tag: &HTMLTag<'_>, page_classes: &HashSet<String>) -> bool {
    if tag.name().as_bytes() == b"template" || tag.attributes().contains("hidden") {
        return true;
    }
    if get_attr(tag, "style").is_some_and(|style| style_hides(&style)) {
        return true;
    }
    if let Some(mut classes) = tag.attributes().class_iter()
        && classes.any(|c| HIDING_CLASSES.contains(&c) || page_classes.contains(c))
    {
        return true;
    }
    // Hidden from assistive technology *and* out of the tab order: nothing a person reaches,
    // which is the shape a planted link takes.
    get_attr(tag, "aria-hidden").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
        && get_attr(tag, "tabindex").is_some_and(|v| v.trim() == "-1")
}

/// For each node of `dom`, by index, whether a person can see it: an element that hides itself
/// hides its whole subtree.
pub(crate) fn hidden_nodes(dom: &VDom<'_>) -> Vec<bool> {
    let parser = dom.parser();
    let nodes = dom.nodes();
    let style_texts: Vec<String> = nodes
        .iter()
        .filter_map(|n| n.as_tag())
        .filter(|t| t.name().as_bytes() == b"style")
        .map(|t| t.inner_html(parser))
        .collect();
    let page_classes = hiding_classes_from_styles(style_texts.iter().map(String::as_str));
    let mut hidden = vec![false; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        // Already inside a hidden subtree, which was marked whole.
        if hidden[index] {
            continue;
        }
        let Some(tag) = node.as_tag() else {
            continue;
        };
        if !element_hides(tag, &page_classes) {
            continue;
        }
        hidden[index] = true;
        if let Some((start, end)) = tag.children().boundaries(parser) {
            let end = (end as usize).min(nodes.len().saturating_sub(1));
            for flag in &mut hidden[start as usize..=end] {
                *flag = true;
            }
        }
    }
    hidden
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_styles_that_hide() {
        for style in [
            "display:none",
            "DISPLAY: NONE !important",
            "visibility: hidden",
            "opacity:0",
            "font-size: 0px",
            "position:absolute; left:-9999px",
            "text-indent:-10000em",
            "clip: rect(0 0 0 0)",
            "clip-path: inset(50%)",
            "width:0; height:0; overflow:hidden",
        ] {
            assert!(style_hides(style), "{style}");
        }
    }

    #[test]
    fn inline_styles_that_do_not_hide() {
        for style in [
            "",
            "display:block",
            "opacity:0.5",
            "left:-10px",
            "width:0",
            "display:none; display:block",
            "color:red",
        ] {
            assert!(!style_hides(style), "{style}");
        }
    }

    #[test]
    fn classes_hidden_by_the_page_stylesheet() {
        let css = ".trap{display:none} .shown{color:red} @media screen { .x, .y { visibility:hidden } }";
        let classes = hiding_classes_from_styles([css]);
        let mut got: Vec<_> = classes.into_iter().collect();
        got.sort();
        assert_eq!(got, ["trap", "x", "y"]);
    }

    #[test]
    fn hidden_subtrees_are_marked_whole() {
        let html = r#"<html><head><style>.trap-link{display:none}</style></head><body>
            <a href="/visible">v</a>
            <a href="/a" style="display:none">x</a>
            <a href="/b" hidden>x</a>
            <div style="visibility:hidden"><p><a href="/c">x</a></p></div>
            <a href="/d" class="trap-link">x</a>
            <a href="/e" class="btn sr-only">x</a>
            <a href="/f" aria-hidden="true" tabindex="-1">x</a>
            <a href="/g" aria-hidden="true">icon</a>
            </body></html>"#;
        let dom = crate::html::parse_html(html).expect("parses");
        let hidden = hidden_nodes(&dom);
        let parser = dom.parser();
        let mut visible = Vec::new();
        for handle in dom.query_selector("a[href]").expect("selector") {
            let tag = handle.get(parser).and_then(|n| n.as_tag()).expect("tag");
            if !hidden[handle.get_inner() as usize] {
                visible.push(get_attr(tag, "href").expect("href").into_owned());
            }
        }
        assert_eq!(visible, ["/visible", "/g"]);
    }
}
