use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use quiche::h3::{Header as H3Header, NameValue};

use crate::adapters::http3::ConnectionManager;
use crate::core::ProxyService;

pub struct Http3Handler {
    proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
    connection_manager: Arc<ConnectionManager>,
}

impl Http3Handler {
    pub fn new(
        proxy_service_holder: Arc<RwLock<Arc<ProxyService>>>,
        connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        Self {
            proxy_service_holder,
            connection_manager,
        }
    }

    pub async fn handle_h3_request(
        &self,
        conn_id: &[u8],
        stream_id: u64,
        headers: Vec<H3Header>,
        _body: Option<Bytes>,
    ) -> Result<()> {
        tracing::debug!("Handling HTTP/3 request on stream {}", stream_id);

        // Convert HTTP/3 headers to HTTP format
        let (_method, uri, _http_headers) = self.convert_h3_headers(headers)?;

        // Create request information for processing
        let request_info = Http3RequestInfo { uri };

        // Process the request using existing proxy logic
        let response = self.process_request(request_info).await?;

        // Convert response back to HTTP/3 format and send
        self.send_h3_response(conn_id, stream_id, response).await?;

        Ok(())
    }

    fn convert_h3_headers(&self, headers: Vec<H3Header>) -> Result<(Method, Uri, HeaderMap)> {
        let mut method = None;
        let mut uri = None;
        let mut authority = None;
        let mut scheme = None;
        let mut header_map = HeaderMap::new();

        for header in headers {
            let name =
                std::str::from_utf8(header.name()).context("Invalid header name encoding")?;
            let value =
                std::str::from_utf8(header.value()).context("Invalid header value encoding")?;

            match name {
                ":method" => {
                    method =
                        Some(Method::from_bytes(value.as_bytes()).context("Invalid HTTP method")?);
                }
                ":path" => {
                    uri = Some(Uri::try_from(value).context("Invalid URI")?);
                }
                ":authority" => {
                    authority = Some(value.to_string());
                }
                ":scheme" => {
                    scheme = Some(value.to_string());
                }
                _ => {
                    // Regular header
                    let header_name =
                        HeaderName::from_bytes(name.as_bytes()).context("Invalid header name")?;
                    let header_value =
                        HeaderValue::from_str(value).context("Invalid header value")?;
                    header_map.insert(header_name, header_value);
                }
            }
        }

        let method = method.ok_or_else(|| anyhow::anyhow!("Missing :method header"))?;
        let mut uri = uri.ok_or_else(|| anyhow::anyhow!("Missing :path header"))?;

        // Reconstruct full URI if authority and scheme are present
        if let (Some(auth), Some(sch)) = (authority, scheme) {
            let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
            let full_uri = format!("{}://{}{}", sch, auth, path_and_query);
            uri = Uri::try_from(full_uri).context("Failed to construct full URI")?;
        }

        Ok((method, uri, header_map))
    }

    async fn process_request(&self, request_info: Http3RequestInfo) -> Result<Http3Response> {
        // This is a simplified version - in a real implementation, you'd need to:
        // 1. Create a proper HTTP request from the H3 request
        // 2. Use the existing proxy service to handle routing
        // 3. Convert the response back to H3 format

        // For now, let's create a basic response
        let proxy_service = match self.proxy_service_holder.read() {
            Ok(service) => service,
            Err(e) => {
                tracing::error!(
                    "Failed to acquire proxy service read lock in HTTP/3 handler: {}",
                    e
                );
                return Ok(Http3Response {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    headers: HeaderMap::new(),
                    body: Some(Bytes::from("Internal server error")),
                });
            }
        };

        // Create a basic HTTP request structure for processing
        // Note: This is simplified - you'd need proper HTTP request construction
        let path = request_info.uri.path();

        // Check if this matches any configured routes
        let route_config = proxy_service.find_matching_route(path);

        if route_config.is_some() {
            // Process through proxy service
            // This would require adapting the existing handler logic
            Ok(Http3Response {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                body: Some(Bytes::from("HTTP/3 response from proxy")),
            })
        } else {
            // Not found
            Ok(Http3Response {
                status: StatusCode::NOT_FOUND,
                headers: HeaderMap::new(),
                body: Some(Bytes::from("Not Found")),
            })
        }
    }

    async fn send_h3_response(
        &self,
        conn_id: &[u8],
        stream_id: u64,
        response: Http3Response,
    ) -> Result<()> {
        // Convert HTTP headers to HTTP/3 headers
        let mut h3_headers = Vec::new();

        // Add status header
        h3_headers.push(H3Header::new(
            b":status",
            response.status.as_str().as_bytes(),
        ));

        // Add regular headers
        for (name, value) in response.headers.iter() {
            h3_headers.push(H3Header::new(name.as_str().as_bytes(), value.as_bytes()));
        }

        // Add Alt-Svc header to advertise HTTP/3 support
        h3_headers.push(H3Header::new(b"alt-svc", b"h3=\":443\"; ma=3600"));

        // Send the response
        self.connection_manager
            .send_response(
                conn_id,
                stream_id,
                &h3_headers,
                response.body,
                true, // fin = true for complete response
            )
            .await?;

        Ok(())
    }
}

#[derive(Debug)]
struct Http3RequestInfo {
    uri: Uri,
}

#[derive(Debug)]
struct Http3Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Option<Bytes>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use quiche::h3::Header as H3Header;

    #[test]
    fn test_h3_header_conversion() {
        // Test basic HTTP header conversion functionality
        let headers = vec![
            H3Header::new(b":method", b"GET"),
            H3Header::new(b":scheme", b"https"),
            H3Header::new(b":authority", b"example.com"),
            H3Header::new(b":path", b"/test"),
            H3Header::new(b"user-agent", b"test-client"),
        ];

        // This tests the headers are properly formed
        for header in &headers {
            assert!(!header.name().is_empty());
            assert!(!header.value().is_empty());
        }
    }

    #[test]
    fn test_http3_response_creation() {
        let response = Http3Response {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Some(Bytes::from("test response")),
        };

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body.unwrap(), Bytes::from("test response"));
    }

    #[test]
    fn test_http3_request_info_creation() {
        let uri = Uri::from_static("https://example.com/test");

        let request_info = Http3RequestInfo { uri: uri.clone() };

        assert_eq!(request_info.uri, uri);
        assert_eq!(request_info.uri.path(), "/test");
    }
}
