use super::tests::{
    completed, initiated, mock, mock_with_headers, part_response, streaming_remote,
};
use super::*;

#[tokio::test]
async fn sdk_preserves_literal_keys_and_empty_object_bodies() {
    let source = tempfile::NamedTempFile::new().unwrap();
    for (key, wire) in [
        (
            "a//雪 +%2F?#/end/",
            "/files/a//%E9%9B%AA%20%2B%252F%3F%23/end/",
        ),
        ("/leading/", "/files//leading/"),
        (" ", "/files/%20"),
        ("\t\n", "/files/%09%0A"),
    ] {
        let (gateway, requests, task) = mock(vec![
            (200, vec![], String::new()),
            (204, vec![], String::new()),
        ])
        .await;
        let path = format!("s3://files/{key}");
        gateway
            .copy(source.path().to_str().unwrap(), &path, false, false)
            .await
            .unwrap();
        gateway
            .delete(&Remote::parse(&path).unwrap().unwrap())
            .await
            .unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests[0], ("PUT".into(), wire.into(), Vec::new()));
        assert_eq!(requests[1].1, wire);
        task.abort();
    }
}

#[tokio::test]
async fn sdk_serializes_generic_metadata_and_completion_xml() {
    let headers: Arc<std::sync::Mutex<Vec<reqwest::header::HeaderMap>>> = Arc::default();
    let marker = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(marker.path(), b"commit\n").unwrap();
    let (gateway, requests, task) = mock_with_headers(
        vec![initiated(), part_response(1), completed()],
        headers.clone(),
    )
    .await;
    gateway
        .upload_stream(
            &b""[..],
            &streaming_remote(),
            marker.path(),
            &["Project=example".into(), "x-amz-meta-source=cli".into()],
            std::future::pending(),
        )
        .await
        .unwrap();
    let headers = headers.lock().unwrap();
    assert_eq!(headers[0]["x-amz-meta-project"], "example");
    assert_eq!(headers[0]["x-amz-meta-x-amz-meta-source"], "cli");
    assert!(!headers[0].contains_key("x-amz-meta-source"));
    assert_eq!(headers[0]["content-type"], "application/octet-stream");
    assert_eq!(headers[2]["content-type"], "application/xml");
    let requests = requests.lock().unwrap();
    assert!(requests[1].2.is_empty());
    assert_eq!(
        requests[1].1,
        "/files/object.bin?partNumber=1&uploadId=opaque%2B%2F%3D%26%252F"
    );
    task.abort();
}

#[tokio::test]
async fn sdk_completion_wire_supports_10000_ordered_escaped_parts() {
    use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
    let (gateway, requests, task) = mock(vec![completed()]).await;
    gateway
        .sdk
        .complete_multipart_upload()
        .bucket("files")
        .key("object.bin")
        .upload_id("opaque+/=&%2F")
        .multipart_upload(
            CompletedMultipartUpload::builder()
                .set_parts(Some(
                    (1..=10000)
                        .map(|number| {
                            CompletedPart::builder()
                                .part_number(number)
                                .e_tag("\"opaque&etag\"")
                                .build()
                        })
                        .collect(),
                ))
                .build(),
        )
        .send()
        .await
        .unwrap();
    let requests = requests.lock().unwrap();
    let xml = std::str::from_utf8(&requests[0].2).unwrap();
    assert!(xml.len() < multipart::MAX_COMPLETION);
    assert!(xml.contains("opaque&amp;etag"));
    assert_eq!(xml.matches("<PartNumber>").count(), 10000);
    assert!(
        xml.find("<PartNumber>1</PartNumber>").unwrap()
            < xml.find("<PartNumber>10000</PartNumber>").unwrap()
    );
    task.abort();
}

#[tokio::test]
async fn sdk_rejects_http200_put_error_without_deleting_move_source() {
    let source = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"payload").unwrap();
    let (gateway, requests, task) = mock(vec![(
        200,
        vec![],
        "<Error><Code>InternalError</Code></Error>".into(),
    )])
    .await;
    assert!(
        gateway
            .copy(
                source.path().to_str().unwrap(),
                "s3://files/object",
                true,
                false
            )
            .await
            .is_err()
    );
    assert!(source.path().exists());
    assert_eq!(requests.lock().unwrap().len(), 1);
    task.abort();
}

#[tokio::test]
async fn sdk_errors_never_include_response_secrets_or_redirect_locations() {
    let secret = "https://provider.invalid/object?signature=not-for-errors";
    for (status, headers, body) in [
        (307, vec![("location", secret.into())], String::new()),
        (
            403,
            vec![],
            format!("<Error><Code>{secret}</Code><Message>test-key</Message></Error>"),
        ),
        (
            200,
            vec![],
            format!(
                "<ListAllMyBucketsResult><Buckets><Bucket><CreationDate>{secret}</CreationDate></Bucket></Buckets></ListAllMyBucketsResult>"
            ),
        ),
    ] {
        let (gateway, requests, task) = mock(vec![(status, headers, body)]).await;
        let error = gateway.list(None).await.unwrap_err();
        let error = format!("{error:?}");
        assert!(!error.contains(secret));
        assert!(!error.contains("test-key"));
        assert_eq!(requests.lock().unwrap().len(), 1);
        task.abort();
    }
    let (gateway, _, task) = mock(vec![(
        403,
        vec![],
        format!("<Error><Code>{secret}</Code></Error>"),
    )])
    .await;
    let error = gateway.download(&streaming_remote()).await.unwrap_err();
    assert!(!format!("{error:?}").contains(secret));
    task.abort();
}

#[tokio::test]
async fn sdk_rejects_invalid_listing_fields() {
    for xml in [
        "<ListBucketResult><Contents><Size>1</Size></Contents></ListBucketResult>",
        "<ListBucketResult><Contents><Key>a</Key></Contents></ListBucketResult>",
        "<ListBucketResult><Contents><Key>a</Key><Size>-1</Size></Contents></ListBucketResult>",
        "<ListBucketResult><EncodingType>unknown</EncodingType></ListBucketResult>",
    ] {
        let (gateway, _, task) = mock(vec![(200, vec![], xml.into())]).await;
        assert!(gateway.list(Some("s3://files/")).await.is_err(), "{xml}");
        task.abort();
    }
}

#[tokio::test]
async fn sdk_bounds_multipart_bodies_and_aborts_failed_completion() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(marker.path(), b"commit\n").unwrap();
    let (gateway, requests, task) = mock(vec![
        initiated(),
        part_response(1),
        (200, vec![], " ".repeat(multipart::MAX_COMPLETION + 1)),
        (204, vec![], String::new()),
    ])
    .await;
    let error = gateway
        .upload_stream(
            &b"payload"[..],
            &streaming_remote(),
            marker.path(),
            &[],
            std::future::pending(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("uncertain"));
    assert_eq!(requests.lock().unwrap().last().unwrap().0, "DELETE");
    task.abort();
}

use std::sync::Arc;

#[tokio::test]
async fn sdk_multipart_preserves_whitespace_keys_on_success_and_abort() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(marker.path(), b"commit\n").unwrap();
    for success in [true, false] {
        let responses = vec![
            (200, vec![], "<InitiateMultipartUploadResult><Bucket>files</Bucket><Key> </Key><UploadId>opaque</UploadId></InitiateMultipartUploadResult>".into()),
            if success { part_response(1) } else { (500, vec![], String::new()) },
            if success { (200, vec![], "<CompleteMultipartUploadResult><Bucket>files</Bucket><Key> </Key><ETag>\"done\"</ETag></CompleteMultipartUploadResult>".into()) } else { (204, vec![], String::new()) },
        ];
        let (gateway, requests, task) = mock(responses).await;
        let result = gateway
            .upload_stream(
                &b"payload"[..],
                &Remote::parse("s3://files/ ").unwrap().unwrap(),
                marker.path(),
                &[],
                std::future::pending(),
            )
            .await;
        assert_eq!(result.is_ok(), success, "{result:?}");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|request| request.1.starts_with("/files/%20?"))
        );
        assert_eq!(requests[2].0, if success { "POST" } else { "DELETE" });
        task.abort();
    }
}

#[tokio::test]
async fn sdk_completion_uses_sdk_response_parsing() {
    for (xml, accepted) in [
        (
            "<Error><Code>InternalError</Code></Error>".to_owned(),
            false,
        ),
        (completed().2, true),
    ] {
        let (gateway, _, task) = mock(vec![(200, vec![], xml)]).await;
        let result = gateway
            .sdk
            .complete_multipart_upload()
            .bucket("files")
            .key("object.bin")
            .upload_id("opaque")
            .send()
            .await;
        assert_eq!(result.is_ok(), accepted, "{result:?}");
        task.abort();
    }
}

#[tokio::test]
async fn sdk_uses_pod_hostname_and_path_style_logical_buckets() {
    let headers: Arc<std::sync::Mutex<Vec<reqwest::header::HeaderMap>>> = Arc::default();
    let (mock_gateway, requests, task) = mock_with_headers(vec![
        (200, vec![], "<ListAllMyBucketsResult><Buckets><Bucket><Name>files</Name></Bucket></Buckets></ListAllMyBucketsResult>".into()),
        (200, vec![], "<ListBucketResult/>".into()),
        (204, vec![], String::new()),
    ], headers.clone()).await;
    let address: std::net::SocketAddr = mock_gateway.origin.socket_addrs(|| None).unwrap()[0];
    let mut gateway = Gateway::new(
        &format!("http://s3.example.test:{}", address.port()),
        "my-pod",
        "test-key",
    )
    .unwrap();
    let http = crate::http_client_builder()
        .unwrap()
        .no_proxy()
        .resolve("my-pod.s3.example.test", address)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    gateway.sdk = sdk::client(http, &gateway.origin, gateway.token).unwrap();
    gateway.list(None).await.unwrap();
    gateway
        .list(Some("s3://UPPER.Files/a// +%2F/"))
        .await
        .unwrap();
    gateway
        .delete(&Remote::parse("s3://a/object").unwrap().unwrap())
        .await
        .unwrap();
    let headers = headers.lock().unwrap();
    assert!(
        headers
            .iter()
            .all(|headers| headers["host"] == format!("my-pod.s3.example.test:{}", address.port()))
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests[0].1, "/");
    let url = Url::parse(&format!("http://localhost{}", requests[1].1)).unwrap();
    assert_eq!(url.path(), "/UPPER.Files/");
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(pairs["list-type"], "2");
    assert_eq!(pairs["encoding-type"], "url");
    assert_eq!(pairs["max-keys"], "1000");
    assert_eq!(pairs["delimiter"], "/");
    assert_eq!(pairs["prefix"], "a// +%2F/");
    assert_eq!(requests[2].1, "/a/object");
    task.abort();
}

#[tokio::test]
async fn sdk_rejects_oversized_completion_manifests_before_sending() {
    use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
    let (gateway, requests, task) = mock(vec![]).await;
    let result = gateway
        .sdk
        .complete_multipart_upload()
        .bucket("files")
        .key("object.bin")
        .upload_id("opaque")
        .multipart_upload(
            CompletedMultipartUpload::builder()
                .set_parts(Some(
                    (1..=10000)
                        .map(|number| {
                            CompletedPart::builder()
                                .part_number(number)
                                .e_tag(format!("\"{}\"", "&".repeat(254)))
                                .build()
                        })
                        .collect(),
                ))
                .build(),
        )
        .send()
        .await;
    assert!(
        sdk::error(result.unwrap_err())
            .to_string()
            .contains("limit")
    );
    assert!(requests.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn sdk_writes_match_strict_gateway_header_contract() {
    let headers: Arc<std::sync::Mutex<Vec<reqwest::header::HeaderMap>>> = Arc::default();
    let (gateway, requests, task) = mock_with_headers(
        vec![
            (200, vec![], String::new()),
            (204, vec![], String::new()),
            initiated(),
            part_response(1),
            completed(),
            initiated(),
            part_response(1),
            (204, vec![], String::new()),
        ],
        headers.clone(),
    )
    .await;
    let source = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"payload").unwrap();
    gateway
        .copy(
            source.path().to_str().unwrap(),
            "s3://files/object.bin",
            false,
            false,
        )
        .await
        .unwrap();
    gateway.delete(&streaming_remote()).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("success");
    std::fs::write(&marker, b"commit\n").unwrap();
    gateway
        .upload_stream(
            &b"payload"[..],
            &streaming_remote(),
            &marker,
            &["project=example".into()],
            std::future::pending(),
        )
        .await
        .unwrap();
    std::fs::remove_file(&marker).unwrap();
    assert!(
        gateway
            .upload_stream(
                &b"payload"[..],
                &streaming_remote(),
                &marker,
                &["project=example".into()],
                std::future::pending()
            )
            .await
            .is_err()
    );
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.0.as_str())
            .collect::<Vec<_>>(),
        [
            "PUT", "DELETE", "POST", "PUT", "POST", "POST", "PUT", "DELETE"
        ]
    );
    let headers = headers.lock().unwrap();
    assert_eq!(headers.len(), 8);
    for (index, headers) in headers.iter().enumerate() {
        assert!(!headers.contains_key("x-amz-user-agent"));
        let metadata = headers
            .keys()
            .filter(|name| name.as_str().starts_with("x-amz-"))
            .map(|name| name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            metadata,
            if [2, 5].contains(&index) {
                vec!["x-amz-meta-project"]
            } else {
                Vec::new()
            }
        );
        if [2, 5].contains(&index) {
            assert_eq!(headers["x-amz-meta-project"], "example");
        }
    }
    task.abort();
}

#[tokio::test]
async fn sdk_put_delete_and_gateway_get_encode_literal_keys_identically() {
    let source = tempfile::NamedTempFile::new().unwrap();
    for (key, wire) in [
        ("café +%.bin", "/files/caf%C3%A9%20%2B%25.bin"),
        (
            "a//雪 +%2F?#/end/",
            "/files/a//%E9%9B%AA%20%2B%252F%3F%23/end/",
        ),
        (
            "reserved/:?#[]@!$&'()*+,;=%",
            "/files/reserved/%3A%3F%23%5B%5D%40%21%24%26%27%28%29%2A%2B%2C%3B%3D%25",
        ),
        ("raw/%2F%252F/%2E", "/files/raw/%252F%25252F/%252E"),
        ("unreserved/-._~AZaz09", "/files/unreserved/-._~AZaz09"),
        ("/leading//trailing/", "/files//leading//trailing/"),
        (" \t\n", "/files/%20%09%0A"),
    ] {
        let (gateway, requests, task) = mock(vec![
            (200, vec![], String::new()),
            (404, vec![], String::new()),
            (204, vec![], String::new()),
        ])
        .await;
        let path = format!("s3://files/{key}");
        let remote = Remote::parse(&path).unwrap().unwrap();
        gateway
            .copy(source.path().to_str().unwrap(), &path, false, false)
            .await
            .unwrap();
        assert!(gateway.download(&remote).await.is_err());
        gateway.delete(&remote).await.unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.0.as_str())
                .collect::<Vec<_>>(),
            ["PUT", "GET", "DELETE"]
        );
        for request in requests.iter() {
            assert_eq!(request.1, wire);
        }
        task.abort();
    }
}
