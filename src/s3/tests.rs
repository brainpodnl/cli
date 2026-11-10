use std::sync::{Arc, Mutex};

use axum::{Router, body::Body, extract::State, http::Request, response::Response, routing::any};
use clap::Parser as _;

use super::*;

type MockResponse = (u16, Vec<(&'static str, String)>, String);

type Requests = Arc<Mutex<Vec<(String, String, Vec<u8>)>>>;

pub(super) async fn mock(
    responses: Vec<MockResponse>,
) -> (Gateway<'static>, Requests, tokio::task::JoinHandle<()>) {
    mock_with_headers(responses, Arc::default()).await
}

type RequestHeaders = Arc<Mutex<Vec<reqwest::header::HeaderMap>>>;

pub(super) async fn mock_with_headers(
    responses: Vec<MockResponse>,
    headers: RequestHeaders,
) -> (Gateway<'static>, Requests, tokio::task::JoinHandle<()>) {
    let requests: Requests = Arc::default();
    let responses = Arc::new(Mutex::new(std::collections::VecDeque::from(responses)));
    let router = Router::new()
        .fallback(any({
            let responses = responses.clone();
            move |State(requests): State<Requests>, request: Request<Body>| {
                let responses = responses.clone();
                let headers = headers.clone();
                async move {
                    assert_eq!(request.headers()["authorization"], "Bearer test-key");
                    for (name, value) in request.headers() {
                        assert!(!name.as_str().starts_with("x-amz-checksum-"));
                        assert!(
                            ![
                                "x-amz-sdk-checksum-algorithm",
                                "x-amz-trailer",
                                "transfer-encoding",
                                "x-amz-content-sha256",
                                "x-amz-security-token",
                                "x-amz-date"
                            ]
                            .contains(&name.as_str())
                        );
                        assert!(
                            !value
                                .as_bytes()
                                .windows(11)
                                .any(|part| part == b"aws-chunked")
                        );
                    }
                    assert!(
                        !request
                            .uri()
                            .query()
                            .unwrap_or_default()
                            .split('&')
                            .any(|pair| pair.starts_with("x-id="))
                    );
                    headers.lock().unwrap().push(request.headers().clone());
                    let method = request.method().to_string();
                    let initiation = method == "POST"
                        && request
                            .uri()
                            .query()
                            .is_some_and(|query| query.split('&').any(|pair| pair == "uploads"));
                    if method != "GET"
                        && request.headers().keys().any(|name| {
                            name.as_str().starts_with("x-amz-")
                                && !["x-amz-date", "x-amz-content-sha256"].contains(&name.as_str())
                                && !(initiation && name.as_str().starts_with("x-amz-meta-"))
                        })
                    {
                        let mut names = request
                            .headers()
                            .keys()
                            .map(|name| name.as_str())
                            .collect::<Vec<_>>();
                        names.sort_unstable();
                        eprintln!("strict gateway rejected request header names: {names:?}");
                        return Response::builder().status(501).body(Body::empty()).unwrap();
                    }
                    let uri = request.uri().to_string();
                    let length = request
                        .headers()
                        .get("content-length")
                        .map(|v| v.to_str().unwrap().parse::<usize>().unwrap());
                    let body = axum::body::to_bytes(request.into_body(), MAX_OBJECT + 1)
                        .await
                        .unwrap()
                        .to_vec();
                    if method == "PUT" {
                        assert_eq!(length, Some(body.len()));
                    }
                    requests.lock().unwrap().push((method, uri, body));
                    let (status, headers, body) = responses
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected request");
                    let mut response = Response::builder().status(status);
                    for (name, value) in headers {
                        response = response.header(name, value);
                    }
                    response.body(Body::from(body)).unwrap()
                }
            }
        }))
        .with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let gateway = Gateway {
        sdk: sdk::client(
            crate::http_client_builder()
                .unwrap()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            &origin,
            "test-key",
        )
        .unwrap(),
        origin,
        token: "test-key",
        http: crate::http_client_builder()
            .unwrap()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    };
    (gateway, requests, task)
}

#[test]
fn parses_commands_and_contract() {
    for args in [
        vec!["s3", "ls"],
        vec!["s3", "ls", "s3://files/a/"],
        vec!["s3", "cp", "a", "s3://files/a"],
        vec!["s3", "mv", "s3://files/a", "a"],
        vec!["s3", "upload", "a", "s3://files/a"],
        vec![
            "s3",
            "upload",
            "-",
            "s3://files/a",
            "--require-success-file",
            "/tmp/success",
            "--metadata",
            "project=example",
        ],
        vec!["s3", "download", "s3://files/a", "-"],
        vec!["s3", "rm", "s3://files/a"],
    ] {
        let opts = crate::Opts::try_parse_from(std::iter::once("brainpod").chain(args)).unwrap();
        assert!(crate::cmd::needs_api_token(&opts.command));
        assert!(!crate::cmd::needs_client(&opts.command));
    }
    assert!(
        crate::Opts::try_parse_from(["brainpod", "s3", "cp", "--recursive", "a", "s3://files/a"])
            .is_err()
    );
}

#[test]
fn preserves_literal_keys_and_rejects_normalization() {
    let gateway = Gateway::new("https://s3.example.test:443", "my-pod", "key").unwrap();
    let remote = Remote::parse("s3://files/a//雪 +%2F?#/end/")
        .unwrap()
        .unwrap();
    let url = gateway.url(Some(&remote)).unwrap();
    assert_eq!(url.host_str(), Some("my-pod.s3.example.test"));
    assert_eq!(url.path(), "/files/a//%E9%9B%AA%20%2B%252F%3F%23/end/");
    assert!(url.query().is_none());
    for path in ["s3://files/a/../b", "s3://files/./a", "s3://files/"] {
        assert!(Remote::parse(path).unwrap().unwrap().object().is_err());
    }
    for endpoint in [
        "https://user:pass@example.test",
        "https://example.test/path",
        "https://example.test/?x=1",
        "file:///tmp/x",
    ] {
        assert!(Gateway::new(endpoint, "pod", "key").is_err());
    }
    assert!(Gateway::new("https://example.test", "evil.example", "key").is_err());
}

#[test]
fn decodes_listing_fields_once_using_form_semantics() {
    assert_eq!(
        decode_field("a+b%20c%2B%252F%E9%9B%AA", true).unwrap(),
        "a b c+%2F雪"
    );
    assert_eq!(decode_field("a+%2F", false).unwrap(), "a+%2F");
    for field in ["%", "%ZZ", "%FF"] {
        assert!(decode_field(field, true).is_err());
    }
}

#[test]
fn only_accepts_safe_provider_redirects() {
    for location in [
        "http://provider.test/a",
        "/relative",
        "https://user:pass@provider.test/a",
        "https://provider.test/a#fragment",
        "file:///tmp/a",
    ] {
        assert!(provider_url(location).is_err());
    }
    let gateway = Gateway::new("https://s3.example.test", "pod", "key").unwrap();
    let request = gateway
        .http
        .get(provider_url("https://provider.test/key?signature=secret").unwrap())
        .build()
        .unwrap();
    assert!(!request.headers().contains_key("authorization"));
}

#[tokio::test]
async fn follows_object_pages_with_opaque_tokens_without_reusing_echoed_token() {
    let page1 = r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><EncodingType>url</EncodingType><IsTruncated>true</IsTruncated><NextContinuationToken>a+/=&amp;%2F</NextContinuationToken><Contents><Key>a%2B%252F</Key><Size>42</Size></Contents><CommonPrefixes><Prefix>dir%2F</Prefix></CommonPrefixes></ListBucketResult>"#;
    let page2 = r#"<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated><ContinuationToken>a+/=&amp;%2F</ContinuationToken><Contents><Key>last+file</Key><Size>0</Size></Contents></ListBucketResult>"#;
    let (gateway, requests, task) = mock(vec![
        (200, vec![], page1.into()),
        (200, vec![], page2.into()),
    ])
    .await;
    let result = gateway.list(Some("s3://files/dot/../+%/")).await.unwrap();
    assert_eq!(result["items"][0]["key"], "a+%2F");
    assert_eq!(result["items"][1]["key"], "dir/");
    assert_eq!(result["items"][2]["key"], "last file");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let url = Url::parse(&format!("http://localhost{}", requests[1].1)).unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(pairs["continuation-token"], "a+/=&%2F");
    assert_eq!(pairs["prefix"], "dot/../+%/");
    task.abort();
}

#[tokio::test]
async fn rejects_broken_and_repeated_pagination() {
    for xml in [
        "<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>",
        "<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>same</NextContinuationToken></ListBucketResult>",
        "<broken>",
        "<Error><Code>AccessDenied</Code></Error>",
    ] {
        let (gateway, _, task) =
            mock(vec![(200, vec![], xml.into()), (200, vec![], xml.into())]).await;
        assert!(gateway.list(Some("s3://files/")).await.is_err());
        task.abort();
    }
}

#[tokio::test]
async fn lists_buckets_with_optional_pagination() {
    let (gateway, requests, task) = mock(vec![
        (200, vec![], "<ListAllMyBucketsResult><Buckets><Bucket><Name>one</Name></Bucket></Buckets><ContinuationToken>next</ContinuationToken></ListAllMyBucketsResult>".into()),
        (200, vec![], "<ListAllMyBucketsResult><Buckets><Bucket><Name>two</Name></Bucket></Buckets></ListAllMyBucketsResult>".into()),
    ]).await;
    let result = gateway.list(None).await.unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 2);
    assert_eq!(requests.lock().unwrap().len(), 2);
    task.abort();
}

#[tokio::test]
async fn upload_and_move_only_remove_local_source_after_success() {
    for (status, moving) in [(200, false), (200, true), (403, true)] {
        let source = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(source.path(), b"hello").unwrap();
        let (gateway, requests, task) = mock(vec![(
            status,
            vec![],
            if status == 200 {
                String::new()
            } else {
                "<Error><Code>AccessDenied</Code></Error>".into()
            },
        )])
        .await;
        let result = gateway
            .copy(
                source.path().to_str().unwrap(),
                "s3://files/a//b%?",
                moving,
                false,
            )
            .await;
        assert_eq!(result.is_ok(), status == 200);
        assert_eq!(source.path().exists(), !(moving && status == 200));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0],
            ("PUT".into(), "/files/a//b%25%3F".into(), b"hello".to_vec())
        );
        task.abort();
    }
}

#[tokio::test]
async fn rejects_oversized_and_unsupported_copies_before_mutation() {
    let source = tempfile::NamedTempFile::new().unwrap();
    source.as_file().set_len((MAX_OBJECT + 1) as u64).unwrap();
    let (gateway, requests, task) = mock(vec![]).await;
    assert!(
        gateway
            .copy(source.path().to_str().unwrap(), "s3://files/a", true, false)
            .await
            .is_err()
    );
    assert!(source.path().exists());
    assert!(
        gateway
            .copy("s3://files/a", "s3://files/a", true, false)
            .await
            .is_err()
    );
    assert!(
        gateway
            .copy("s3://files/a", "s3://files/b", false, true)
            .await
            .is_err()
    );
    assert!(gateway.copy("a", "b", false, false).await.is_err());
    assert!(requests.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn failed_download_never_deletes_source_or_creates_destination() {
    for (status, headers) in [
        (
            307,
            vec![("location", "http://untrusted.invalid/object".into())],
        ),
        (404, vec![]),
        (
            302,
            vec![("location", "https://untrusted.invalid/object".into())],
        ),
    ] {
        let destination = tempfile::tempdir().unwrap().path().join("download");
        let (gateway, requests, task) = mock(vec![(status, headers, String::new())]).await;
        assert!(
            gateway
                .copy("s3://files/a", destination.to_str().unwrap(), true, false)
                .await
                .is_err()
        );
        assert!(!destination.exists());
        assert_eq!(requests.lock().unwrap().len(), 1);
        task.abort();
    }
}

#[tokio::test]
async fn deletion_is_exact_and_status_checked() {
    for status in [204, 500] {
        let (gateway, requests, task) = mock(vec![(status, vec![], String::new())]).await;
        assert_eq!(
            gateway
                .delete(&Remote::parse("s3://files/dir//%2F/").unwrap().unwrap())
                .await
                .is_ok(),
            status == 204
        );
        assert_eq!(requests.lock().unwrap()[0].1, "/files/dir//%252F/");
        task.abort();
    }
}

#[tokio::test]
async fn bounds_listing_responses() {
    let (gateway, _, task) = mock(vec![(200, vec![], "x".repeat(MAX_XML + 1))]).await;
    assert!(
        gateway
            .list(None)
            .await
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
    task.abort();
}

fn provider(body: &[u8], declared_length: usize) -> (String, std::thread::JoinHandle<String>) {
    let body = body.to_vec();
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
    crate::install_crypto_provider().unwrap();
    let certificate =
        CertificateDer::from_pem_slice(include_bytes!("test-certificate.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("test-private-key.pem")).unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "https://localhost:{}/object?signature=test",
        listener.local_addr().unwrap().port()
    );
    let thread = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = rustls::StreamOwned::new(connection, socket);
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {declared_length}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        for chunk in body.chunks(16 * 1024) {
            stream.write_all(chunk).unwrap();
        }
        stream.flush().unwrap();
        String::from_utf8(request).unwrap()
    });
    (url, thread)
}

fn trust_provider(gateway: &mut Gateway<'_>) {
    let certificate =
        reqwest::Certificate::from_pem(include_bytes!("test-certificate.pem")).unwrap();
    gateway.http = crate::http_client_builder()
        .unwrap()
        .no_proxy()
        .tls_certs_only([certificate])
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(5))
        .build()
        .unwrap();
}

#[tokio::test]
async fn downloads_and_gateway_copies_without_forwarding_bearer_and_moves_after_success() {
    for (local, moving, put_status, delete_status) in [
        (true, false, 200, 204),
        (true, true, 200, 204),
        (false, false, 200, 204),
        (false, true, 200, 204),
        (false, true, 500, 204),
        (false, true, 200, 500),
    ] {
        let (url, provider) = provider(b"payload", 7);
        let mut responses = vec![(307, vec![("location", url)], String::new())];
        if !local {
            responses.push((put_status, vec![], String::new()));
        }
        if moving && (local || put_status == 200) {
            responses.push((delete_status, vec![], String::new()));
        }
        let (mut gateway, requests, task) = mock(responses).await;
        trust_provider(&mut gateway);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("download");
        let destination = if local {
            path.to_str().unwrap()
        } else {
            "s3://archive/copied"
        };
        let result = gateway
            .copy("s3://files/original", destination, moving, false)
            .await;
        assert_eq!(result.is_ok(), put_status == 200 && delete_status == 204);
        if local {
            assert_eq!(std::fs::read(&path).unwrap(), b"payload");
        }
        let request = provider.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /object?signature=test "));
        assert!(!request.contains("authorization:"));
        assert!(!request.contains("test-key"));
        let requests = requests.lock().unwrap();
        assert_eq!(requests[0].0, "GET");
        if !local {
            assert_eq!(
                requests[1],
                ("PUT".into(), "/archive/copied".into(), b"payload".to_vec())
            );
        }
        if moving && (local || put_status == 200) {
            assert_eq!(requests.last().unwrap().0, "DELETE");
            assert_eq!(requests.last().unwrap().1, "/files/original");
        } else {
            assert!(!requests.iter().any(|request| request.0 == "DELETE"));
        }
        task.abort();
    }
}

#[tokio::test]
async fn oversized_provider_download_preserves_source_and_destination() {
    let (url, provider) = provider(b"", MAX_OBJECT + 1);
    let (mut gateway, requests, task) =
        mock(vec![(307, vec![("location", url)], String::new())]).await;
    trust_provider(&mut gateway);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("download");
    assert!(
        gateway
            .copy("s3://files/original", path.to_str().unwrap(), true, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
    assert!(!path.exists());
    assert_eq!(requests.lock().unwrap().len(), 1);
    provider.join().unwrap();
    task.abort();
}

#[tokio::test]
async fn bounds_bodies_even_without_content_length() {
    let response = axum::http::Response::builder()
        .status(200)
        .body(vec![0; 17])
        .unwrap();
    assert!(
        bounded_body(reqwest::Response::from(response), 16)
            .await
            .is_err()
    );
    let response = axum::http::Response::builder()
        .status(200)
        .body(vec![0; 16])
        .unwrap();
    assert_eq!(
        bounded_body(reqwest::Response::from(response), 16)
            .await
            .unwrap()
            .len(),
        16
    );
}

#[test]
fn configuration_and_machine_description_include_s3() {
    use clap::CommandFactory as _;
    let config: crate::config::Config =
        toml::from_str("s3_endpoint = 'https://s3.example.test'").unwrap();
    assert_eq!(
        config.s3_endpoint.as_deref(),
        Some("https://s3.example.test")
    );
    let description =
        crate::describe::generate(crate::Opts::command(), &["s3".into(), "ls".into()]).unwrap();
    assert_eq!(description["command"]["requirements"]["pod"], true);
    assert_eq!(description["command"]["requirements"]["apiToken"], true);
    assert_eq!(description["command"]["effect"], "read");
}

pub(super) fn initiated() -> MockResponse {
    (200, vec![], "<InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>files</Bucket><Key>object.bin</Key><UploadId>opaque+/=&amp;%2F</UploadId></InitiateMultipartUploadResult>".into())
}

pub(super) fn part_response(number: usize) -> MockResponse {
    (
        200,
        vec![("etag", format!("\"part-{number}\""))],
        String::new(),
    )
}

pub(super) fn completed() -> MockResponse {
    (200, vec![], "<CompleteMultipartUploadResult><Bucket>files</Bucket><Key>object.bin</Key><ETag>\"complete\"</ETag></CompleteMultipartUploadResult>".into())
}

pub(super) fn streaming_remote() -> Remote {
    Remote::parse("s3://files/object.bin").unwrap().unwrap()
}

#[tokio::test]
async fn multipart_streams_more_than_25mib_with_ordered_etags_and_opaque_id() {
    use tokio::io::AsyncReadExt as _;
    let marker = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(marker.path(), b"commit\n").unwrap();
    let size = 26 * 1024 * 1024 + 17;
    let (gateway, requests, task) = mock(vec![
        initiated(),
        part_response(1),
        part_response(2),
        part_response(3),
        part_response(4),
        completed(),
    ])
    .await;
    let input = tokio::io::repeat(b'x').take(size as u64);
    let result = gateway
        .upload_stream(
            input,
            &streaming_remote(),
            marker.path(),
            &["project=example".into()],
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(result["size"], size);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[0].1, "/files/object.bin?uploads");
    for (index, request) in requests[1..5].iter().enumerate() {
        assert_eq!(request.0, "PUT");
        let url = Url::parse(&format!("http://localhost{}", request.1)).unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(pairs["uploadId"], "opaque+/=&%2F");
        assert_eq!(pairs["partNumber"], (index + 1).to_string());
        assert_eq!(
            request.2.len(),
            if index < 3 {
                8 * 1024 * 1024
            } else {
                2 * 1024 * 1024 + 17
            }
        );
        assert!(request.2.iter().all(|b| *b == b'x'));
    }
    let xml = String::from_utf8(requests[5].2.clone()).unwrap();
    let first = xml.find("<PartNumber>1</PartNumber>").unwrap();
    let last = xml.find("<PartNumber>4</PartNumber>").unwrap();
    assert!(first < last);
    assert!(xml.contains("part-1"));
    assert_eq!(requests[5].0, "POST");
    task.abort();
}

#[tokio::test]
async fn multipart_never_completes_without_valid_producer_commit() {
    for marker_contents in [None, Some(""), Some("commit"), Some("commit\nextra")] {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("success");
        if let Some(contents) = marker_contents {
            std::fs::write(&marker, contents).unwrap();
        }
        let (gateway, requests, task) = mock(vec![
            initiated(),
            part_response(1),
            (204, vec![], String::new()),
        ])
        .await;
        assert!(
            gateway
                .upload_stream(
                    &b"partial producer output"[..],
                    &streaming_remote(),
                    &marker,
                    &[],
                    std::future::pending()
                )
                .await
                .is_err()
        );
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            ["POST", "PUT", "DELETE"]
        );
        task.abort();
    }
}

struct FailingInput {
    remaining: usize,
}

impl tokio::io::AsyncRead for FailingInput {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.remaining == 0 {
            return std::task::Poll::Ready(Err(std::io::Error::other("producer read failed")));
        }
        let count = self.remaining.min(buffer.remaining());
        buffer.initialize_unfilled()[..count].fill(b'x');
        buffer.advance(count);
        self.remaining -= count;
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn multipart_aborts_on_input_error_even_with_a_marker() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(marker.path(), b"commit\n").unwrap();
    let (gateway, requests, task) = mock(vec![
        initiated(),
        part_response(1),
        (204, vec![], String::new()),
    ])
    .await;
    let input = FailingInput {
        remaining: 8 * 1024 * 1024 + 23,
    };
    assert!(
        gateway
            .upload_stream(
                input,
                &streaming_remote(),
                marker.path(),
                &[],
                std::future::pending()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("input")
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.0.as_str())
            .collect::<Vec<_>>(),
        ["POST", "PUT", "DELETE"]
    );
    task.abort();
}

#[tokio::test]
async fn multipart_aborts_failed_parts_and_error_or_malformed_completions() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(marker.path(), b"commit\n").unwrap();
    let failed_parts = [
        (500, vec![], String::new()),
        (200, vec![], String::new()),
        (200, vec![("etag", "unquoted".into())], String::new()),
        (
            200,
            vec![("etag", format!("\"{}\"", "a".repeat(255)))],
            String::new(),
        ),
    ];
    for failed in failed_parts {
        let (gateway, requests, task) =
            mock(vec![initiated(), failed, (204, vec![], String::new())]).await;
        assert!(
            gateway
                .upload_stream(
                    &b"raw bytes"[..],
                    &streaming_remote(),
                    marker.path(),
                    &[],
                    std::future::pending()
                )
                .await
                .is_err()
        );
        assert_eq!(requests.lock().unwrap().last().unwrap().0, "DELETE");
        task.abort();
    }
    for xml in [
        "<Error><Code>InternalError</Code></Error>",
        "<CompleteMultipartUploadResult><Bucket>files</Bucket><Key>object.bin</Key></CompleteMultipartUploadResult>",
        "<CompleteMultipartUploadResult><Bucket>other</Bucket><Key>object.bin</Key><ETag>\"x\"</ETag></CompleteMultipartUploadResult>",
        "not XML",
    ] {
        let (gateway, requests, task) = mock(vec![
            initiated(),
            part_response(1),
            (200, vec![], xml.into()),
            (204, vec![], String::new()),
        ])
        .await;
        assert!(
            gateway
                .upload_stream(
                    &b"raw bytes"[..],
                    &streaming_remote(),
                    marker.path(),
                    &[],
                    std::future::pending()
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("uncertain")
        );
        assert_eq!(requests.lock().unwrap().last().unwrap().0, "DELETE");
        task.abort();
    }
}

struct WaitingInput {
    read_started: Arc<tokio::sync::Notify>,
}

impl tokio::io::AsyncRead for WaitingInput {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.read_started.notify_one();
        std::task::Poll::Pending
    }
}

#[tokio::test]
async fn multipart_aborts_when_cancelled_while_waiting_for_input() {
    let read_started = Arc::new(tokio::sync::Notify::new());
    let (gateway, requests, task) = mock(vec![initiated(), (204, vec![], String::new())]).await;
    let input = WaitingInput {
        read_started: read_started.clone(),
    };
    assert!(
        gateway
            .upload_stream(
                input,
                &streaming_remote(),
                Path::new("/unused"),
                &[],
                read_started.notified()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.0.as_str())
            .collect::<Vec<_>>(),
        ["POST", "DELETE"]
    );
    task.abort();
}

#[tokio::test]
async fn streaming_download_exceeds_small_copy_limit_without_credentials_or_summary() {
    let body = vec![b'z'; 26 * 1024 * 1024];
    let (url, provider) = provider(&body, body.len());
    let (mut gateway, requests, task) =
        mock(vec![(307, vec![("location", url)], String::new())]).await;
    trust_provider(&mut gateway);
    let mut output = tokio::io::sink();
    let result = gateway
        .download_stream(&streaming_remote(), &mut output)
        .await;
    let provider_result = tokio::task::spawn_blocking(move || provider.join())
        .await
        .unwrap();
    assert_eq!(result.unwrap(), body.len() as u64);
    let request = provider_result.unwrap().to_ascii_lowercase();
    assert!(!request.contains("authorization:"));
    assert!(!request.contains("test-key"));
    assert_eq!(requests.lock().unwrap().len(), 1);
    let mut bytes = Vec::new();
    let (url, provider) = self::provider(b"raw encrypted bytes", 19);
    let (mut gateway, _, second_task) =
        mock(vec![(307, vec![("location", url)], String::new())]).await;
    trust_provider(&mut gateway);
    assert_eq!(
        gateway
            .download_stream(&streaming_remote(), &mut bytes)
            .await
            .unwrap(),
        19
    );
    assert_eq!(bytes, b"raw encrypted bytes");
    provider.join().unwrap();
    task.abort();
    second_task.abort();
}
