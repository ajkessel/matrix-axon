//! The OpenAPI 3.1 document, assembled by utoipa from the handler signatures.
//!
//! [`ApiDoc::openapi`](utoipa::OpenApi::openapi) builds the spec; the
//! `openapi_spec_is_current` test serializes it and diffs it against the
//! checked-in `openapi/openapi.json`, so drift between handlers and the spec is
//! a failing test. The spec is the source of truth for generated clients.

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Axon API",
        version = "0.1.0",
        description = "Read API for a personal Matrix state layer. Account-scoped \
                       resources nest under /v1/accounts/{account_id}; /v1/rooms is \
                       the cross-account aggregate.",
    ),
    paths(
        crate::routes::accounts::list_accounts,
        crate::routes::accounts::login,
        crate::routes::accounts::get_account,
        crate::routes::rooms::list_rooms,
        crate::routes::rooms::room_timeline,
        crate::routes::events::get_event,
        crate::routes::messages::send_message,
        crate::routes::messages::edit_message,
        crate::routes::messages::redact_event,
        crate::routes::messages::react,
    ),
    components(schemas(
        crate::dto::AccountDto,
        crate::dto::AccountStateDto,
        crate::dto::RoomDto,
        crate::dto::EventDto,
        crate::dto::TimelinePage,
        crate::dto::LoginRequest,
        crate::dto::SendMessageRequest,
        crate::dto::EditRequest,
        crate::dto::ReactRequest,
        crate::dto::SendResultDto,
        crate::response::ErrorBody,
        crate::response::ErrorResponse,
    )),
    tags(
        (name = "accounts", description = "The Matrix accounts this Axon manages"),
        (name = "rooms", description = "Rooms and their timelines"),
        (name = "events", description = "Individual events"),
        (name = "messages", description = "Sending, editing, redacting, and reacting"),
    ),
)]
pub struct ApiDoc;
