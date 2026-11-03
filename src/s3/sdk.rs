use aws_sdk_s3::config::{
    BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    retry::RetryConfig,
};
use aws_smithy_runtime_api::client::{
    auth::AuthSchemeId,
    http::{HttpConnector, HttpConnectorFuture, SharedHttpConnector, http_client_fn},
    orchestrator::{HttpRequest, HttpResponse},
    result::ConnectorError,
};
use aws_smithy_types::body::SdkBody;

use super::*;

#[derive(Clone, Debug)]
struct Transport {
    http: reqwest::Client,
    origin: Url,
    bearer: reqwest::header::HeaderValue,
}

#[derive(Debug)]
struct TransportError(String);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TransportError {}

fn failure(message: impl Into<String>) -> ConnectorError {
    ConnectorError::other(Box::new(TransportError(message.into())), None)
}

pub(super) fn error(error: impl std::error::Error) -> anyhow::Error {
    let mut source: Option<&(dyn std::error::Error + 'static)> = error.source();
    while let Some(current) = source {
        if let Some(safe) = current.downcast_ref::<TransportError>() {
            return anyhow!("{}", safe.0);
        }
        source = current.source();
    }
    anyhow!("S3 SDK request failed")
}

pub(super) fn client(
    http: reqwest::Client,
    origin: &Url,
    token: &str,
) -> Result<aws_sdk_s3::Client> {
    let mut bearer = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| anyhow!("invalid S3 gateway token"))?;
    bearer.set_sensitive(true);
    let transport = SharedHttpConnector::new(Transport {
        http,
        origin: origin.clone(),
        bearer,
    });
    let config = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .endpoint_url(origin.as_str())
        .force_path_style(true)
        .disable_s3_express_session_auth(true)
        .allow_no_auth()
        .auth_scheme_preference([AuthSchemeId::new("noAuth")])
        .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
        .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
        .retry_config(RetryConfig::disabled())
        .http_client(http_client_fn(move |_, _| transport.clone()))
        .build();
    Ok(aws_sdk_s3::Client::from_conf(config))
}

impl HttpConnector for Transport {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let transport = self.clone();
        HttpConnectorFuture::new(async move { transport.send(request).await })
    }
}

impl Transport {
    async fn send(
        &self,
        request: HttpRequest,
    ) -> std::result::Result<HttpResponse, ConnectorError> {
        let mut url = Url::parse(request.uri()).map_err(|_| failure("invalid SDK request URL"))?;
        if url.origin() != self.origin.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(failure("SDK request must target the S3 gateway origin"));
        }
        let query = url.query().map(|query| {
            query
                .split('&')
                .filter(|pair| pair.split('=').next() != Some("x-id"))
                .collect::<Vec<_>>()
                .join("&")
        });
        url.set_query(query.as_deref().filter(|query| !query.is_empty()));
        let method = Method::from_bytes(request.method().as_bytes())
            .map_err(|_| failure("invalid SDK request method"))?;
        let completion =
            method == Method::POST && url.query_pairs().any(|(name, _)| name == "uploadId");
        let max = if completion {
            multipart::MAX_COMPLETION
        } else {
            MAX_XML
        };
        let body = request
            .body()
            .bytes()
            .ok_or_else(|| failure("SDK gateway requests require a buffered body"))?;
        if completion && body.len() > max {
            return Err(failure("multipart completion exceeds its byte limit"));
        }
        let expected = if method == Method::DELETE {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::OK
        };
        let mut outgoing = self
            .http
            .request(method, url)
            .timeout(Duration::from_secs(120));
        for (name, value) in request.headers().iter() {
            if name == "x-amz-user-agent" {
                continue;
            }
            if name == "authorization"
                || name == "transfer-encoding"
                || name == "x-amz-trailer"
                || name.starts_with("x-amz-checksum-")
                || name == "x-amz-sdk-checksum-algorithm"
                || (name == "content-encoding" && value.contains("aws-chunked"))
            {
                return Err(failure(
                    "unsupported SDK gateway authentication or body encoding",
                ));
            }
            outgoing = outgoing.header(name, value);
        }
        let response = outgoing
            .header(reqwest::header::AUTHORIZATION, self.bearer.clone())
            .body(body.to_vec())
            .send()
            .await
            .map_err(|_| failure("S3 gateway request failed"))?;
        let status = response.status();
        if status != expected {
            return Err(failure(format!(
                "S3 request returned HTTP {}",
                status.as_u16()
            )));
        }
        let headers = response.headers().clone();
        let bytes = bounded_body(response, max)
            .await
            .map_err(|_| failure("S3 gateway response failed or exceeds its byte limit"))?;
        let mut response = HttpResponse::new(status.into(), SdkBody::from(bytes));
        for (name, value) in &headers {
            let value = value
                .to_str()
                .map_err(|_| failure("invalid gateway response header"))?;
            response
                .headers_mut()
                .try_insert(name.as_str().to_owned(), value.to_owned())
                .map_err(|_| failure("invalid gateway response header"))?;
        }
        Ok(response)
    }
}

pub(super) fn key(key: &str) -> &str {
    if key.trim().is_empty() { "_" } else { key }
}

#[derive(Debug)]
pub(super) struct LiteralKey(Option<String>);

impl LiteralKey {
    pub(super) fn new(key: &str) -> Self {
        Self(key.trim().is_empty().then(|| key.to_owned()))
    }
}

impl aws_smithy_runtime_api::client::interceptors::Intercept for LiteralKey {
    fn name(&self) -> &'static str {
        "LiteralS3Key"
    }

    fn modify_before_serialization(
        &self,
        context: &mut aws_smithy_runtime_api::client::interceptors::context::BeforeSerializationInterceptorContextMut<'_>,
        _: &aws_smithy_runtime_api::client::runtime_components::RuntimeComponents,
        _: &mut aws_smithy_types::config_bag::ConfigBag,
    ) -> std::result::Result<(), aws_smithy_runtime_api::box_error::BoxError> {
        let Some(key) = &self.0 else {
            return Ok(());
        };
        // SDK endpoint parameters reject whitespace-only keys; restore the literal input before SDK serialization.
        macro_rules! restore {
            ($($input:ty),+ $(,)?) => { $(
                if let Some(input) = context.input_mut().downcast_mut::<$input>() {
                    input.key = Some(key.clone());
                    return Ok(());
                }
            )+ };
        }
        restore!(
            aws_sdk_s3::operation::put_object::PutObjectInput,
            aws_sdk_s3::operation::delete_object::DeleteObjectInput,
            aws_sdk_s3::operation::create_multipart_upload::CreateMultipartUploadInput,
            aws_sdk_s3::operation::upload_part::UploadPartInput,
            aws_sdk_s3::operation::complete_multipart_upload::CompleteMultipartUploadInput,
            aws_sdk_s3::operation::abort_multipart_upload::AbortMultipartUploadInput,
        );
        Err("unsupported literal S3 key operation".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transport_rejects_other_origins_and_unsupported_wire_features_without_sending() {
        let mut bearer = reqwest::header::HeaderValue::from_static("Bearer transport-secret");
        bearer.set_sensitive(true);
        let transport = Transport {
            http: crate::http_client_builder()
                .unwrap()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            origin: Url::parse("http://127.0.0.1:0").unwrap(),
            bearer,
        };
        assert!(!format!("{transport:?}").contains("transport-secret"));
        let error = transport
            .call(HttpRequest::get("https://provider.invalid/object?signature=secret").unwrap())
            .await
            .unwrap_err();
        assert!(!format!("{error:?}").contains("signature=secret"));
        for (name, value) in [
            ("authorization", "AWS4-HMAC-SHA256 forbidden"),
            ("x-amz-checksum-crc32", "AAAAAA=="),
            ("x-amz-sdk-checksum-algorithm", "CRC32"),
            ("x-amz-trailer", "checksum"),
            ("content-encoding", "aws-chunked"),
            ("transfer-encoding", "chunked"),
        ] {
            let mut request = HttpRequest::get("http://127.0.0.1:0/").unwrap();
            request.headers_mut().insert(name, value);
            let error = transport.call(request).await.unwrap_err();
            assert!(super::error(error).to_string().contains("unsupported"));
        }
        let mut request = HttpRequest::get("http://127.0.0.1:0/").unwrap();
        *request.body_mut() = SdkBody::taken();
        assert!(
            super::error(transport.call(request).await.unwrap_err())
                .to_string()
                .contains("buffered")
        );
    }
}
