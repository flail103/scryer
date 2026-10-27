//! Streaming Nab representations of format-independent search results.

use std::borrow::Cow;
use std::io::{self, Write};

use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use scryer_application::IndexerSearchResult;
use scryer_plugin_sdk::{PluginReleaseAttribute, PluginReleaseAttributeOrigin};

const NEWZNAB_NAMESPACE: &str = "http://www.newznab.com/DTD/2010/feeds/attributes/";
const TORZNAB_NAMESPACE: &str = "http://torznab.com/schemas/2015/feed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NabProtocol {
    Newznab,
    Torznab,
}

impl NabProtocol {
    fn prefix(self) -> &'static str {
        match self {
            Self::Newznab => "newznab",
            Self::Torznab => "torznab",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Newznab => "application/x-nzb",
            Self::Torznab => "application/x-bittorrent",
        }
    }
}

/// The caller supplies authorized, stable public identity and retrieval links.
/// Source download/link/GUID values are deliberately never serialized directly.
pub struct NabFeedItem<'a, Link = &'a str> {
    pub result: &'a IndexerSearchResult,
    pub external_guid: Link,
    pub retrieval_url: Link,
}

pub struct NabFeedPage {
    pub offset: usize,
    /// None means unknown, not a fabricated upper bound or upstream sum.
    pub total: Option<usize>,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid Nab feed metadata")
}

fn checked_text(value: &str) -> io::Result<&str> {
    if value.chars().all(|ch| {
        matches!(ch, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}'
        | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
    }) {
        Ok(value)
    } else {
        Err(invalid())
    }
}

// XML 1.0 fifth-edition NCName: a prefix or local name, never markup or a colon.
fn valid_name(value: &str) -> bool {
    let start = |ch| {
        matches!(ch, 'A'..='Z' | '_' | 'a'..='z' | '\u{c0}'..='\u{d6}'
        | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{2ff}' | '\u{370}'..='\u{37d}'
        | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' | '\u{2070}'..='\u{218f}'
        | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}'
        | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
    };
    let mut chars = value.chars();
    chars.next().is_some_and(start) && chars.all(|ch| start(ch)
        || matches!(ch, '-' | '.' | '0'..='9' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}'))
}

fn element(writer: &mut Writer<impl Write>, name: &str, value: &str) -> io::Result<()> {
    writer
        .create_element(name)
        .write_text_content(BytesText::new(checked_text(value)?))?;
    Ok(())
}

fn standard_namespace(prefix: &str) -> Option<&'static str> {
    match prefix {
        "newznab" => Some(NEWZNAB_NAMESPACE),
        "torznab" => Some(TORZNAB_NAMESPACE),
        "xml" => Some("http://www.w3.org/XML/1998/namespace"),
        _ => None,
    }
}

/// Rewrites are decided by field name only, as Prowlarr's serializer does:
/// the GUID becomes the opaque external GUID, download fields carry the Scryer
/// retrieval link, and magnet fields are omitted because they cannot legally
/// hold an HTTP link. Every other attribute, including URL-valued ones such as
/// `comments`, `details` and `infourl`, and the release `password`, passes
/// through verbatim.
fn public_attribute_value<'a>(
    attribute: &'a PluginReleaseAttribute,
    item: &'a NabFeedItem<'_, impl AsRef<str>>,
) -> Option<&'a str> {
    match attribute.name.to_ascii_lowercase().as_str() {
        "magneturl" | "magnet" => None,
        "guid" => Some(item.external_guid.as_ref()),
        "url" | "link" | "downloadurl" | "download_url" | "nzburl" | "torrenturl" => {
            Some(item.retrieval_url.as_ref())
        }
        _ => Some(&attribute.value),
    }
}

// Validate before writing any bytes: malformed provider metadata must never
// truncate a response whose successful headers have already been delivered.
fn writable_attribute(attribute: &PluginReleaseAttribute, value: &str) -> bool {
    if checked_text(&attribute.name).is_err()
        || checked_text(value).is_err()
        || (attribute.origin == PluginReleaseAttributeOrigin::Element
            && !matches!(attribute.name.as_str(), "category" | "attr"))
    {
        return false;
    }
    match attribute.namespace.as_deref() {
        Some(prefix) => {
            if !valid_name(prefix) || prefix == "xmlns" {
                return false;
            }
            let Some(uri) = attribute
                .namespace_uri
                .as_deref()
                .or_else(|| standard_namespace(prefix))
            else {
                return false;
            };
            !uri.is_empty()
                && checked_text(uri).is_ok()
                && ((prefix == "xml") == (Some(uri) == standard_namespace("xml")))
                && uri != "http://www.w3.org/2000/xmlns/"
        }
        None => attribute.namespace_uri.as_deref().is_none_or(|uri| {
            checked_text(uri).is_ok()
                && Some(uri) != standard_namespace("xml")
                && uri != "http://www.w3.org/2000/xmlns/"
        }),
    }
}

fn write_attribute(
    writer: &mut Writer<impl Write>,
    attribute: &PluginReleaseAttribute,
    value: &str,
) -> io::Result<()> {
    let local = match attribute.origin {
        PluginReleaseAttributeOrigin::Attribute => "attr",
        PluginReleaseAttributeOrigin::Element => attribute.name.as_str(),
    };
    if !valid_name(local) {
        return Err(invalid());
    }
    let qualified;
    let declaration;
    let mut tag = if let Some(prefix) = &attribute.namespace {
        if !valid_name(prefix) || prefix == "xmlns" {
            return Err(invalid());
        }
        qualified = format!("{prefix}:{local}");
        BytesStart::new(&qualified)
    } else {
        BytesStart::new(local)
    };
    if let Some(prefix) = &attribute.namespace {
        let namespace = attribute
            .namespace_uri
            .as_deref()
            .or_else(|| standard_namespace(prefix))
            .ok_or_else(invalid)?;
        if namespace.is_empty()
            || ((prefix == "xml") != (Some(namespace) == standard_namespace("xml")))
            || namespace == "http://www.w3.org/2000/xmlns/"
        {
            return Err(invalid());
        }
        declaration = format!("xmlns:{prefix}");
        tag.push_attribute((declaration.as_str(), checked_text(namespace)?));
    } else if let Some(namespace) = &attribute.namespace_uri {
        if Some(namespace.as_str()) == standard_namespace("xml")
            || namespace == "http://www.w3.org/2000/xmlns/"
        {
            return Err(invalid());
        }
        tag.push_attribute(("xmlns", checked_text(namespace)?));
    }
    match attribute.origin {
        PluginReleaseAttributeOrigin::Attribute => {
            tag.push_attribute(("name", checked_text(&attribute.name)?));
            tag.push_attribute(("value", checked_text(value)?));
            writer.write_event(Event::Empty(tag))?;
        }
        PluginReleaseAttributeOrigin::Element => {
            let end = tag.to_end().into_owned();
            writer.write_event(Event::Start(tag))?;
            writer.write_event(Event::Text(BytesText::new(checked_text(value)?)))?;
            writer.write_event(Event::End(end))?;
        }
    }
    Ok(())
}

// Older and non-Nab adapters may expose only normalized source assertions.
// Preserve those when no original collection exists, without interpreting
// arbitrary plugin extras or overriding a losslessly parsed source feed.
fn normalized_attributes(
    result: &IndexerSearchResult,
) -> impl Iterator<Item = (&'static str, Cow<'_, str>)> {
    let attributes = &result.response_attributes;
    [
        ("tvdbid", attributes.tvdb_id.as_deref().map(Cow::Borrowed)),
        ("tmdbid", attributes.tmdb_id.as_deref().map(Cow::Borrowed)),
        ("imdbid", attributes.imdb_id.as_deref().map(Cow::Borrowed)),
        (
            "size",
            result
                .size_bytes
                .filter(|value| *value >= 0)
                .map(|value| Cow::Owned(value.to_string())),
        ),
        (
            "grabs",
            result
                .indexer_grabs
                .filter(|value| *value >= 0)
                .map(|value| Cow::Owned(value.to_string())),
        ),
        (
            "password",
            result.password_hint.as_deref().map(Cow::Borrowed),
        ),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| (name, value)))
    .chain(
        attributes
            .categories
            .iter()
            .filter(|value| numeric_category(value))
            .map(|value| ("category", Cow::Borrowed(value.as_str()))),
    )
    .filter(move |_| attributes.original.is_empty())
}

fn numeric_category(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn normalized_category_names(result: &IndexerSearchResult) -> impl Iterator<Item = &str> {
    result
        .response_attributes
        .categories
        .iter()
        .filter(|value| result.response_attributes.original.is_empty() && !numeric_category(value))
        .map(String::as_str)
}

/// Writes one aggregate page in its existing order. Does not clone results,
/// gather the whole feed, search, follow cursors, or infer an aggregate total.
pub fn write_xml_feed<'a>(
    output: impl Write,
    protocol: NabProtocol,
    page: &NabFeedPage,
    items: impl IntoIterator<Item = NabFeedItem<'a>>,
) -> io::Result<()> {
    try_write_xml_feed(output, protocol, page, items.into_iter().map(Ok))
}

/// Mint and serialize one item's links at a time, propagating a failed mint
/// without retaining every encrypted retrieval reference for the whole page.
pub fn try_write_xml_feed<'a, Link: AsRef<str>>(
    output: impl Write,
    protocol: NabProtocol,
    page: &NabFeedPage,
    items: impl IntoIterator<Item = io::Result<NabFeedItem<'a, Link>>>,
) -> io::Result<()> {
    let mut writer = Writer::new(output);
    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
    let mut rss = BytesStart::new("rss");
    rss.push_attribute(("version", "2.0"));
    rss.push_attribute(("xmlns:newznab", NEWZNAB_NAMESPACE));
    rss.push_attribute(("xmlns:torznab", TORZNAB_NAMESPACE));
    writer.write_event(Event::Start(rss))?;
    writer.write_event(Event::Start(BytesStart::new("channel")))?;
    element(&mut writer, "title", "Scryer")?;
    element(&mut writer, "description", "Indexer search results")?;
    // Omit the response element when total is unknown, matching existing
    // aggregators. A made-up total can prematurely end a consumer's walk.
    if let Some(total) = page.total {
        let name = format!("{}:response", protocol.prefix());
        let mut response = BytesStart::new(&name);
        let offset = page.offset.to_string();
        let total = total.to_string();
        response.push_attribute(("offset", offset.as_str()));
        response.push_attribute(("total", total.as_str()));
        writer.write_event(Event::Empty(response))?;
    }
    let mut dropped_items = 0_u64;
    let mut dropped_attributes = 0_u64;
    for item in items {
        let item = item?;
        if [
            &item.result.title,
            item.external_guid.as_ref(),
            item.retrieval_url.as_ref(),
        ]
        .into_iter()
        .any(|value| checked_text(value).is_err())
        {
            dropped_items += 1;
            continue;
        }
        writer.write_event(Event::Start(BytesStart::new("item")))?;
        element(&mut writer, "title", &item.result.title)?;
        let mut guid = BytesStart::new("guid");
        guid.push_attribute(("isPermaLink", "false"));
        writer.write_event(Event::Start(guid))?;
        writer.write_event(Event::Text(BytesText::new(checked_text(
            item.external_guid.as_ref(),
        )?)))?;
        writer.write_event(Event::End(BytesEnd::new("guid")))?;
        element(&mut writer, "link", item.retrieval_url.as_ref())?;
        if let Some(date) = &item.result.published_at {
            let date = chrono::DateTime::parse_from_rfc3339(date)
                .or_else(|_| chrono::DateTime::parse_from_rfc2822(date))
                .map(|date| date.to_rfc2822())
                .unwrap_or_else(|_| date.clone());
            if checked_text(&date).is_ok() {
                element(&mut writer, "pubDate", &date)?;
            } else {
                dropped_attributes += 1;
            }
        }
        let mut enclosure = BytesStart::new("enclosure");
        let size = item
            .result
            .size_bytes
            .unwrap_or_default()
            .max(0)
            .to_string();
        enclosure.push_attribute(("url", checked_text(item.retrieval_url.as_ref())?));
        enclosure.push_attribute(("length", size.as_str()));
        enclosure.push_attribute(("type", protocol.mime()));
        writer.write_event(Event::Empty(enclosure))?;
        for attribute in &item.result.response_attributes.original {
            if let Some(value) = public_attribute_value(attribute, &item) {
                if writable_attribute(attribute, value) {
                    write_attribute(&mut writer, attribute, value)?;
                } else {
                    dropped_attributes += 1;
                }
            }
        }
        for (name, value) in normalized_attributes(item.result) {
            if checked_text(&value).is_err() {
                dropped_attributes += 1;
                continue;
            }
            let qualified = format!("{}:attr", protocol.prefix());
            let mut attribute = BytesStart::new(&qualified);
            attribute.push_attribute(("name", name));
            attribute.push_attribute(("value", checked_text(&value)?));
            writer.write_event(Event::Empty(attribute))?;
        }
        for category in normalized_category_names(item.result) {
            if checked_text(category).is_ok() {
                element(&mut writer, "category", category)?;
            } else {
                dropped_attributes += 1;
            }
        }
        writer.write_event(Event::End(BytesEnd::new("item")))?;
    }
    if dropped_items != 0 || dropped_attributes != 0 {
        tracing::warn!(
            dropped_items,
            dropped_attributes,
            "Skipped invalid Nab feed metadata"
        );
    }
    writer.write_event(Event::End(BytesEnd::new("channel")))?;
    writer.write_event(Event::End(BytesEnd::new("rss")))
}

/// JSON uses the conventional RSS object representation. Only one item's
/// borrowed attribute groups are held while streaming; result payloads remain
/// owned and charged by the shared raw page.
pub fn write_json_feed<'a>(
    output: impl Write,
    protocol: NabProtocol,
    page: &NabFeedPage,
    items: impl IntoIterator<Item = NabFeedItem<'a>>,
) -> io::Result<()> {
    try_write_json_feed(output, protocol, page, items.into_iter().map(Ok))
}

/// The JSON counterpart of `try_write_xml_feed`, with the same lazy ownership.
pub fn try_write_json_feed<'a, Link: AsRef<str>>(
    mut output: impl Write,
    protocol: NabProtocol,
    page: &NabFeedPage,
    items: impl IntoIterator<Item = io::Result<NabFeedItem<'a, Link>>>,
) -> io::Result<()> {
    use serde::Serialize;
    use std::collections::BTreeMap;

    #[derive(Serialize)]
    struct Attributes<T> {
        #[serde(rename = "@attributes")]
        attributes: T,
    }
    #[derive(Serialize)]
    struct Attribute<'a> {
        name: &'a str,
        value: Cow<'a, str>,
    }
    #[derive(Serialize)]
    struct Enclosure<'a> {
        url: &'a str,
        length: String,
        #[serde(rename = "type")]
        mime: &'a str,
    }
    fn json(output: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
        serde_json::to_writer(output, value).map_err(io::Error::other)
    }
    output.write_all(
        b"{\"channel\":{\"title\":\"Scryer\",\"description\":\"Indexer search results\"",
    )?;
    if let Some(total) = page.total {
        output.write_all(b",")?;
        json(&mut output, &format!("{}:response", protocol.prefix()))?;
        output.write_all(b":")?;
        json(
            &mut output,
            &Attributes {
                attributes: BTreeMap::from([
                    ("offset", page.offset.to_string()),
                    ("total", total.to_string()),
                ]),
            },
        )?;
    }
    output.write_all(b",\"item\":[")?;
    let mut dropped_attributes = 0usize;
    for (index, item) in items.into_iter().enumerate() {
        let item = item?;
        if index != 0 {
            output.write_all(b",")?;
        }
        output.write_all(b"{\"title\":")?;
        json(&mut output, &item.result.title)?;
        output.write_all(b",\"guid\":")?;
        json(&mut output, &item.external_guid.as_ref())?;
        output.write_all(b",\"link\":")?;
        json(&mut output, &item.retrieval_url.as_ref())?;
        if let Some(date) = &item.result.published_at {
            output.write_all(b",\"pubDate\":")?;
            let date = chrono::DateTime::parse_from_rfc3339(date)
                .or_else(|_| chrono::DateTime::parse_from_rfc2822(date))
                .map(|date| date.to_rfc2822())
                .unwrap_or_else(|_| date.clone());
            json(&mut output, &date)?;
        }
        output.write_all(b",\"enclosure\":")?;
        json(
            &mut output,
            &Attributes {
                attributes: Enclosure {
                    url: item.retrieval_url.as_ref(),
                    length: item
                        .result
                        .size_bytes
                        .unwrap_or_default()
                        .max(0)
                        .to_string(),
                    mime: protocol.mime(),
                },
            },
        )?;
        let mut groups = BTreeMap::<String, Vec<Attributes<Attribute<'_>>>>::new();
        let mut elements = BTreeMap::<String, Vec<&str>>::new();
        for attribute in &item.result.response_attributes.original {
            let Some(value) = public_attribute_value(attribute, &item) else {
                continue;
            };
            if !writable_attribute(attribute, value) {
                dropped_attributes += 1;
                continue;
            }
            let local = match attribute.origin {
                PluginReleaseAttributeOrigin::Attribute => "attr",
                PluginReleaseAttributeOrigin::Element => attribute.name.as_str(),
            };
            let key = attribute
                .namespace
                .as_ref()
                .map_or_else(|| local.to_string(), |prefix| format!("{prefix}:{local}"));
            match attribute.origin {
                PluginReleaseAttributeOrigin::Attribute => {
                    groups.entry(key).or_default().push(Attributes {
                        attributes: Attribute {
                            name: &attribute.name,
                            value: Cow::Borrowed(value),
                        },
                    })
                }
                PluginReleaseAttributeOrigin::Element => {
                    // The parser currently preserves plain category elements.
                    // Do not allow plugin metadata to duplicate structural keys.
                    if !matches!(local, "category" | "attr") {
                        continue;
                    }
                    elements.entry(key).or_default().push(value);
                }
            }
        }
        for (name, value) in normalized_attributes(item.result) {
            groups
                .entry(format!("{}:attr", protocol.prefix()))
                .or_default()
                .push(Attributes {
                    attributes: Attribute { name, value },
                });
        }
        for category in normalized_category_names(item.result) {
            elements
                .entry("category".into())
                .or_default()
                .push(category);
        }
        for (key, values) in groups {
            output.write_all(b",")?;
            json(&mut output, &key)?;
            output.write_all(b":")?;
            json(&mut output, &values)?;
        }
        for (key, values) in elements {
            output.write_all(b",")?;
            json(&mut output, &key)?;
            output.write_all(b":")?;
            json(&mut output, &values)?;
        }
        output.write_all(b"}")?;
    }
    if dropped_attributes != 0 {
        tracing::warn!(dropped_attributes, "Skipped invalid Nab feed metadata");
    }
    output.write_all(b"]}}")
}

#[cfg(test)]
#[path = "nab_feed_tests.rs"]
mod tests;
