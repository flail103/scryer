use super::*;
use quick_xml::NsReader;
use quick_xml::name::ResolveResult;
use scryer_application::IndexerResponseAttributes;

fn result(attributes: Vec<PluginReleaseAttribute>) -> IndexerSearchResult {
    IndexerSearchResult {
        indexer_id: Some("source".into()),
        source: "source".into(),
        title: "Software & tools <edition>".into(),
        link: Some("https://source.example/get?apikey=upstream-secret".into()),
        download_url: Some("https://source.example/get?apikey=upstream-secret".into()),
        source_kind: None,
        size_bytes: Some(123),
        published_at: Some("2026-10-01T00:00:00Z".into()),
        thumbs_up: None,
        thumbs_down: None,
        indexer_languages: None,
        indexer_subtitles: None,
        indexer_grabs: None,
        password_hint: None,
        parsed_release_metadata: None,
        quality_profile_decision: None,
        extra: Default::default(),
        response_attributes: IndexerResponseAttributes {
            original: attributes,
            ..Default::default()
        },
        guid: Some("https://source.example/get?apikey=upstream-secret".into()),
        info_url: None,
        provenance: None,
        candidate_token: None,
        queue_scope: None,
        coverage_scope: None,
        auto_eligible: None,
        auto_decision_code: None,
        auto_decision_summary: None,
        release_listing_json: None,
        http: None,
    }
}

fn attribute(namespace: Option<&str>, name: &str, value: &str) -> PluginReleaseAttribute {
    PluginReleaseAttribute {
        origin: PluginReleaseAttributeOrigin::Attribute,
        namespace: namespace.map(str::to_owned),
        namespace_uri: None,
        name: name.into(),
        value: value.into(),
    }
}

fn xml(result: &IndexerSearchResult, total: Option<usize>) -> String {
    let mut output = Vec::new();
    write_xml_feed(
        &mut output,
        NabProtocol::Torznab,
        &NabFeedPage { offset: 100, total },
        [NabFeedItem {
            result,
            external_guid: "opaque-source-release",
            retrieval_url: "https://scryer.example/torznab/api?t=get&id=reference",
        }],
    )
    .unwrap();
    String::from_utf8(output).unwrap()
}

fn parsed_attributes(xml: &str) -> Vec<(Option<String>, String, String)> {
    let mut reader = NsReader::from_str(xml);
    let mut actual = Vec::new();
    loop {
        match reader.read_resolved_event().unwrap() {
            (namespace, Event::Empty(event)) if event.local_name().as_ref() == "attr" => {
                let namespace = match namespace {
                    ResolveResult::Bound(namespace) => Some(namespace.0.to_string()),
                    ResolveResult::Unbound => None,
                    ResolveResult::Unknown(_) => panic!("unbound prefix in generated XML"),
                };
                let attrs = event
                    .attributes()
                    .map(|value| {
                        let value = value.unwrap();
                        (
                            value.key.as_ref().to_string(),
                            value
                                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                .unwrap()
                                .into_owned(),
                        )
                    })
                    .collect::<std::collections::HashMap<_, _>>();
                actual.push((namespace, attrs["name"].clone(), attrs["value"].clone()));
            }
            (_, Event::Eof) => break,
            _ => {}
        }
    }
    actual
}

#[test]
fn json_wire_metadata_serializes_as_xml_without_losing_lexical_or_repeated_values() {
    // The plugin boundary is JSON even when an upstream uses XML. This proves
    // the writer consumes structured results and has no source-format coupling.
    let wire = r#"[
        {"origin":"attribute","namespace":"newznab","name":"category","value":"6000"},
        {"origin":"attribute","namespace":"newznab","name":"category","value":"6005"},
        {"origin":"attribute","namespace":"torznab","name":"seeders","value":"007"},
        {"origin":"attribute","name":"custom","value":"001.00"},
        {"origin":"attribute","name":"custom","value":"second & <third> \"quoted\""},
        {"origin":"attribute","name":"password","value":"release&password"},
        {"origin":"element","name":"category","value":"Software & tools"}
    ]"#;
    let attributes: Vec<PluginReleaseAttribute> = serde_json::from_str(wire).unwrap();
    let output = xml(&result(attributes), None);
    assert_eq!(
        parsed_attributes(&output),
        vec![
            (
                Some(NEWZNAB_NAMESPACE.into()),
                "category".into(),
                "6000".into()
            ),
            (
                Some(NEWZNAB_NAMESPACE.into()),
                "category".into(),
                "6005".into()
            ),
            (
                Some(TORZNAB_NAMESPACE.into()),
                "seeders".into(),
                "007".into()
            ),
            (None, "custom".into(), "001.00".into()),
            (None, "custom".into(), "second & <third> \"quoted\"".into()),
            (None, "password".into(), "release&password".into()),
        ]
    );
    assert!(output.contains("<category>Software &amp; tools</category>"));
    assert!(output.contains("<title>Software &amp; tools &lt;edition&gt;</title>"));
    assert!(
        !output.contains(":response"),
        "unknown total must stay unknown"
    );
    assert!(!output.contains("upstream-secret"));
}

#[test]
fn namespace_rebinding_is_preserved_per_attribute() {
    let mut first = attribute(Some("custom"), "tag", "one");
    first.namespace_uri = Some("urn:outer".into());
    let mut second = first.clone();
    second.namespace_uri = Some("urn:inner".into());
    let output = xml(&result(vec![first, second]), Some(101));
    assert_eq!(
        parsed_attributes(&output)
            .iter()
            .map(|value| value.0.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("urn:outer"), Some("urn:inner")]
    );
    assert!(output.contains("offset=\"100\" total=\"101\""));
}

#[test]
fn only_download_or_credential_urls_are_rewritten_and_password_metadata_survives() {
    let output = xml(
        &result(vec![
            attribute(None, "magneturl", "magnet:?xt=urn:btih:123"),
            attribute(None, "downloadurl", "https://source.example/get"),
            attribute(None, "guid", "https://source.example/get?apikey=secret"),
            attribute(
                None,
                "comments",
                "https://source.example/comments?api_key=secret",
            ),
            attribute(
                None,
                "password",
                "https://release.example/?token=release-password",
            ),
            attribute(None, "custom", "keep"),
            attribute(None, "infohash", "012345"),
        ]),
        None,
    );
    let values = parsed_attributes(&output);
    assert_eq!(values.len(), 6);
    assert_eq!(
        values[0].2,
        "https://scryer.example/torznab/api?t=get&id=reference"
    );
    assert_eq!(values[1].2, "opaque-source-release");
    assert_eq!(values[2].2, values[0].2);
    assert_eq!(
        values[3].2,
        "https://release.example/?token=release-password"
    );
    assert_eq!(values[4].2, "keep");
    assert_eq!(values[5].2, "012345");
}

#[test]
fn invalid_names_and_xml_control_characters_are_skipped_without_truncating_the_feed() {
    for attributes in [
        vec![attribute(Some("bad\"/><injected"), "tag", "value")],
        vec![attribute(Some("undeclared"), "tag", "value")],
        vec![attribute(None, "tag", "bad\0value")],
        vec![PluginReleaseAttribute {
            origin: PluginReleaseAttributeOrigin::Element,
            namespace: None,
            namespace_uri: None,
            name: "category><injected".into(),
            value: "value".into(),
        }],
    ] {
        let result = result(attributes);
        let output = xml(&result, None);
        assert!(parsed_attributes(&output).is_empty());
        assert!(output.ends_with("</channel></rss>"));
        assert!(!output.contains("injected"));
    }
}

#[test]
fn output_failure_is_propagated_without_consuming_the_rest_of_the_page() {
    struct Disconnected;
    impl Write for Disconnected {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let items = std::iter::from_fn(|| -> Option<NabFeedItem<'static>> {
        panic!("output disconnected before rows")
    });
    assert_eq!(
        write_xml_feed(
            Disconnected,
            NabProtocol::Newznab,
            &NabFeedPage {
                offset: 0,
                total: None
            },
            items
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[test]
fn lazy_owned_links_preserve_output_and_stop_after_a_reference_failure() {
    let row = result(Vec::new());
    let page = NabFeedPage {
        offset: 0,
        total: None,
    };
    for json in [false, true] {
        let mut visited = 0;
        let items = std::iter::from_fn(|| {
            visited += 1;
            match visited {
                1 => Some(Ok(NabFeedItem {
                    result: &row,
                    external_guid: "public-identity".to_owned(),
                    retrieval_url: "https://scryer.example/newznab/api?t=get&id=opaque".to_owned(),
                })),
                2 => Some(Err(io::Error::from(io::ErrorKind::PermissionDenied))),
                _ => panic!("failed reference must stop page serialization"),
            }
        });
        let mut output = Vec::new();
        let error = if json {
            try_write_json_feed(&mut output, NabProtocol::Newznab, &page, items)
        } else {
            try_write_xml_feed(&mut output, NabProtocol::Newznab, &page, items)
        }
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(visited, 2);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("public-identity"));
        assert!(output.contains("id=opaque"));
        assert!(!output.contains("upstream-secret"));
    }
}

#[test]
fn json_output_keeps_repeated_attributes_categories_and_unknown_total() {
    let result = result(vec![
        attribute(Some("newznab"), "category", "6000"),
        attribute(Some("newznab"), "category", "6005"),
        attribute(Some("torznab"), "seeders", "007"),
        attribute(None, "password", "release&password"),
        PluginReleaseAttribute {
            origin: PluginReleaseAttributeOrigin::Element,
            namespace: None,
            namespace_uri: None,
            name: "category".into(),
            value: "Software & tools".into(),
        },
    ]);
    let mut output = Vec::new();
    write_json_feed(
        &mut output,
        NabProtocol::Newznab,
        &NabFeedPage {
            offset: 100,
            total: None,
        },
        [NabFeedItem {
            result: &result,
            external_guid: "public-id",
            retrieval_url: "https://scryer.example/get",
        }],
    )
    .unwrap();
    let decoded: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let channel = &decoded["channel"];
    assert!(channel.get("newznab:response").is_none());
    let item = &channel["item"][0];
    assert_eq!(item["newznab:attr"][0]["@attributes"]["value"], "6000");
    assert_eq!(item["newznab:attr"][1]["@attributes"]["value"], "6005");
    assert_eq!(item["torznab:attr"][0]["@attributes"]["value"], "007");
    assert_eq!(item["attr"][0]["@attributes"]["value"], "release&password");
    assert_eq!(item["category"][0], "Software & tools");
    assert_eq!(
        item["enclosure"]["@attributes"]["type"],
        "application/x-nzb"
    );
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("upstream-secret")
    );
}

#[test]
fn non_nab_adapters_emit_normalized_claims_without_inventing_extended_attributes() {
    let mut result = result(Vec::new());
    result.response_attributes.categories = vec![
        "4000".into(),
        "6000".into(),
        "100123".into(),
        "Software & tools".into(),
    ];
    result.response_attributes.imdb_id = Some("tt0012345".into());
    result.password_hint = Some("release<&password".into());
    result.indexer_grabs = Some(4);
    result.extra.insert(
        "private-provider-context".into(),
        serde_json::json!("do-not-serialize"),
    );
    let output = xml(&result, Some(1));
    let attributes = parsed_attributes(&output);
    assert_eq!(
        attributes
            .iter()
            .map(|(_, name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("imdbid", "tt0012345"),
            ("size", "123"),
            ("grabs", "4"),
            ("password", "release<&password"),
            ("category", "4000"),
            ("category", "6000"),
            ("category", "100123")
        ]
    );
    assert!(output.contains("<category>Software &amp; tools</category>"));
    assert!(!output.contains("do-not-serialize"));
    let mut output = Vec::new();
    write_json_feed(
        &mut output,
        NabProtocol::Torznab,
        &NabFeedPage {
            offset: 0,
            total: Some(1),
        },
        [NabFeedItem {
            result: &result,
            external_guid: "public-id",
            retrieval_url: "https://scryer.example/get",
        }],
    )
    .unwrap();
    let decoded: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let item = &decoded["channel"]["item"][0];
    assert_eq!(item["torznab:attr"].as_array().unwrap().len(), 7);
    assert_eq!(
        item["torznab:attr"][3]["@attributes"]["value"],
        "release<&password"
    );
    assert_eq!(item["category"][0], "Software & tools");
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("do-not-serialize")
    );
}

#[test]
fn malformed_item_does_not_hide_later_items_or_duplicate_structural_fields() {
    let mut bad = result(vec![]);
    bad.title = "bad\0title".into();
    let good = result(vec![PluginReleaseAttribute {
        origin: PluginReleaseAttributeOrigin::Element,
        namespace: None,
        namespace_uri: None,
        name: "enclosure".into(),
        value: "https://source.example/private".into(),
    }]);
    let mut output = Vec::new();
    write_xml_feed(
        &mut output,
        NabProtocol::Newznab,
        &NabFeedPage {
            offset: 0,
            total: None,
        },
        [&bad, &good].into_iter().map(|result| NabFeedItem {
            result,
            external_guid: "safe",
            retrieval_url: "https://scryer.example/get",
        }),
    )
    .unwrap();
    let xml = String::from_utf8(output).unwrap();
    assert_eq!(xml.matches("<item>").count(), 1);
    assert_eq!(xml.matches("<enclosure ").count(), 1);
    assert!(xml.ends_with("</channel></rss>"));
    assert!(parsed_attributes(&xml).is_empty());
    let mut output = Vec::new();
    write_json_feed(
        &mut output,
        NabProtocol::Newznab,
        &NabFeedPage {
            offset: 0,
            total: None,
        },
        [NabFeedItem {
            result: &good,
            external_guid: "safe",
            retrieval_url: "https://scryer.example/get",
        }],
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        json["channel"]["item"][0]["enclosure"]["@attributes"]["url"],
        "https://scryer.example/get"
    );
}

#[test]
fn credential_aliases_paths_and_source_host_urls_are_rewritten() {
    for value in [
        "https://other.example/file?r=secret",
        "https://other.example/file?i=secret",
        "https://other.example/api_key/secret/file",
        "https://other.example/key/secret",
        "https://source.example/download/rotating-secret",
    ] {
        let output = xml(
            &result(vec![attribute(Some("newznab"), "comments", value)]),
            None,
        );
        let attributes = parsed_attributes(&output);
        assert_eq!(attributes[0].0.as_deref(), Some(NEWZNAB_NAMESPACE));
        assert_eq!(
            attributes[0].2,
            "https://scryer.example/torznab/api?t=get&id=reference"
        );
        assert!(!output.contains("secret"));
    }
}
