use std::time::Duration;

use anyhow::Result;
use log::{error, info};
use picoserve::extract::FromRequestParts;
use picoserve::request::RequestParts;
use picoserve::response::ws;
use picoserve::routing::{get, get_service, post_service};

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
const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#1a1a2e"/><text x="16" y="24" font-size="22" text-anchor="middle">&#x26A1;</text></svg>"##;

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
        let connection = request.body_connection.finalize().await?;
        response_writer
            .write_response(
                connection,
                picoserve::response::Response::new(
                    picoserve::response::StatusCode::OK,
                    FRONTEND_HTML_GZ,
                )
                .with_header("Content-Type", "text/html; charset=utf-8")
                .with_header("Content-Encoding", "gzip")
                .with_header("Cache-Control", "no-store"),
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
        response_writer
            .write_response(
                connection,
                picoserve::response::Response::new(
                    picoserve::response::StatusCode::OK,
                    FAVICON_SVG,
                )
                .with_header("Cache-Control", "max-age=86400")
                .with_header("Content-Type", "image/svg+xml"),
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
