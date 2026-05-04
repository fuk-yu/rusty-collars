use std::time::Duration;

use anyhow::Result;
use log::{error, info};
use picoserve::extract::FromRequestParts;
use picoserve::io::Write as PicoWrite;
use picoserve::request::RequestParts;
use picoserve::response::ws;
use picoserve::routing::{get, get_service, post_service};
use rusty_collars_core::http::accepts_gzip;

use super::ConnectionState;

struct WsClientMeta {
    forwarded_for: Option<String>,
    user_agent: Option<String>,
}

impl<'r, S> FromRequestParts<'r, S> for WsClientMeta {
    type Rejection = core::convert::Infallible;

    async fn from_request_parts(
        _state: &'r S,
        request_parts: &RequestParts<'r>,
    ) -> Result<Self, Self::Rejection> {
        let header = |name: &str| {
            request_parts
                .headers()
                .get(name)
                .and_then(|v| v.as_str().ok().map(str::to_owned))
        };
        Ok(Self {
            forwarded_for: header("x-forwarded-for"),
            user_agent: header("user-agent"),
        })
    }
}

const FRONTEND_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/frontend.html.gz"));
const FRONTEND_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/frontend.html"));
const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#1a1a2e"/><text x="16" y="24" font-size="22" text-anchor="middle">&#x26A1;</text></svg>"##;

// Body wrapper that lets us set Content-Type on the body itself instead of
// adding a `with_header("Content-Type", ...)` after the fact. picoserve's
// `Response::new` always emits a Content-Type derived from the body's
// `Content::content_type()`; layering a second `with_header` on top would
// chain a second value rather than replace it, producing two Content-Type
// fields on the wire (RFC 9110 §8.3 violation).
struct TypedBody {
    mime: &'static str,
    bytes: &'static [u8],
}

impl picoserve::response::Content for TypedBody {
    fn content_type(&self) -> &'static str {
        self.mime
    }

    fn content_length(&self) -> usize {
        self.bytes.len()
    }

    async fn write_content<W: PicoWrite>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.bytes).await
    }
}

pub(super) fn make_app(
) -> picoserve::Router<impl picoserve::routing::PathRouter<ConnectionState>, ConnectionState> {
    picoserve::Router::new()
        .route("/", get_service(FrontendService))
        .route("/favicon.ico", get_service(FaviconService))
        .route("/ota", post_service(OtaService))
        .route(
            "/ws",
            get(
                |meta: WsClientMeta, upgrade: ws::WebSocketUpgrade| async move {
                    upgrade.on_upgrade_using_state(super::ws::WsHandler {
                        forwarded_for: meta.forwarded_for,
                        user_agent: meta.user_agent,
                    })
                },
            ),
        )
}

struct FrontendService;

impl picoserve::routing::RequestHandlerService<ConnectionState> for FrontendService {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &ConnectionState,
        _path_parameters: (),
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let accept_encoding = request
            .parts
            .headers()
            .get("accept-encoding")
            .and_then(|v| v.as_str().ok());
        let serve_gzip = accepts_gzip(accept_encoding);
        let connection = request.body_connection.finalize().await?;
        let body = TypedBody {
            mime: "text/html; charset=utf-8",
            bytes: if serve_gzip { FRONTEND_HTML_GZ } else { FRONTEND_HTML },
        };
        let content_encoding = serve_gzip.then_some(("Content-Encoding", "gzip"));
        response_writer
            .write_response(
                connection,
                picoserve::response::Response::new(picoserve::response::StatusCode::OK, body)
                    .with_header("Cache-Control", "no-store")
                    .with_header("Vary", "Accept-Encoding")
                    .with_headers(content_encoding),
            )
            .await
    }
}

struct FaviconService;

impl picoserve::routing::RequestHandlerService<ConnectionState> for FaviconService {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &ConnectionState,
        _path_parameters: (),
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let connection = request.body_connection.finalize().await?;
        let body = TypedBody {
            mime: "image/svg+xml",
            bytes: FAVICON_SVG.as_bytes(),
        };
        response_writer
            .write_response(
                connection,
                picoserve::response::Response::new(picoserve::response::StatusCode::OK, body)
                    .with_header("Cache-Control", "max-age=86400"),
            )
            .await
    }
}

struct OtaService;

impl picoserve::routing::RequestHandlerService<ConnectionState> for OtaService {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &ConnectionState,
        _path_parameters: (),
        mut request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let content_length = request.body_connection.content_length();
        if content_length == 0 {
            let connection = request.body_connection.finalize().await?;
            return response_writer
                .write_response(
                    connection,
                    picoserve::response::Response::new(
                        picoserve::response::StatusCode::BAD_REQUEST,
                        "Content-Length required",
                    ),
                )
                .await;
        }

        info!("OTA upload: {content_length} bytes");

        let result = {
            let body = request.body_connection.body();
            let mut reader =
                body.reader()
                    .with_different_timeout_signal(Box::pin(async_io::Timer::after(
                        Duration::from_secs(120),
                    )));
            super::admin::perform_ota_update(content_length, &mut reader).await
        };

        let connection = request.body_connection.finalize().await?;
        match result {
            Ok(written) => {
                super::admin::schedule_reboot(Duration::from_millis(500));
                response_writer
                    .write_response(
                        connection,
                        picoserve::response::Response::new(
                            picoserve::response::StatusCode::OK,
                            format!("OTA OK: {written} bytes written, rebooting..."),
                        ),
                    )
                    .await
            }
            Err(err) => {
                error!("OTA failed: {err:#}");
                response_writer
                    .write_response(
                        connection,
                        picoserve::response::Response::new(
                            picoserve::response::StatusCode::INTERNAL_SERVER_ERROR,
                            format!("OTA failed: {err:#}"),
                        ),
                    )
                    .await
            }
        }
    }
}
