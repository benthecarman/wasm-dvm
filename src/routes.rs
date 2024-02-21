use crate::models::create_zap;
use crate::State;
use anyhow::anyhow;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::{Extension, Json};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::ThirtyTwoByteHash;
use lightning_invoice::Bolt11Invoice;
use lnurl::pay::PayResponse;
use lnurl::Tag;
use nostr::nips::nip57;
use nostr::{Event, JsonUtil};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::str::FromStr;
use tonic_openssl_lnd::lnrpc;

pub(crate) async fn get_invoice_impl(
    state: State,
    hash: String,
    amount_msats: u64,
    zap_request: Option<Event>,
) -> anyhow::Result<String> {
    let mut lnd = state.lnd.clone();
    let desc_hash = match zap_request.as_ref() {
        None => sha256::Hash::from_str(&hash)?,
        Some(event) => {
            if event.kind != nostr::Kind::ZapRequest {
                return Err(anyhow!("Invalid zap request"));
            }
            sha256::Hash::hash(event.as_json().as_bytes())
        }
    };

    let request = lnrpc::Invoice {
        value_msat: amount_msats as i64,
        description_hash: desc_hash.into_32().to_vec(),
        expiry: 86_400,
        ..Default::default()
    };

    let resp = lnd.add_invoice(request).await?.into_inner();

    if let Some(zap_request) = zap_request {
        let invoice = Bolt11Invoice::from_str(&resp.payment_request)?;

        // handle private zaps, fixme this won't actually work yet
        let private_zap = nip57::decrypt_private_zap_message(
            state.keys.secret_key().unwrap(),
            &zap_request.pubkey,
            &zap_request,
        )
        .ok()
        .map(|e| e.pubkey);

        // if it is a private zap, use that npub, otherwise use the pubkey from the zap request
        let for_npub = private_zap.unwrap_or(zap_request.pubkey);

        let mut conn = state.db_pool.get()?;
        create_zap(&mut conn, &invoice, &zap_request, for_npub)?;
    }

    Ok(resp.payment_request)
}

pub async fn get_invoice(
    Path(hash): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    Extension(state): Extension<State>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (amount_msats, zap_request) = match params.get("amount").and_then(|a| a.parse::<u64>().ok())
    {
        None => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "ERROR",
                "reason": "Missing amount parameter",
            })),
        )),
        Some(amount_msats) => {
            let zap_request = params.get("nostr").map_or_else(
                || Ok(None),
                |event_str| {
                    Event::from_json(event_str)
                        .map_err(|_| {
                            (
                                StatusCode::BAD_REQUEST,
                                Json(json!({
                                    "status": "ERROR",
                                    "reason": "Invalid zap request",
                                })),
                            )
                        })
                        .map(Some)
                },
            )?;

            Ok((amount_msats, zap_request))
        }
    }?;

    match get_invoice_impl(state, hash, amount_msats, zap_request).await {
        Ok(invoice) => Ok(Json(json!({
            "pr": invoice,
            "routers": []
        }))),
        Err(e) => Err(handle_anyhow_error(e)),
    }
}

pub async fn get_lnurl_pay(
    Path(name): Path<String>,
    Extension(state): Extension<State>,
) -> Result<Json<PayResponse>, (StatusCode, Json<Value>)> {
    let metadata = format!(
        "[[\"text/identifier\",\"{name}@{}\"],[\"text/plain\",\"Sats for {name}\"]]",
        state.domain,
    );

    let hash = sha256::Hash::hash(metadata.as_bytes());
    let callback = format!("https://{}/get-invoice/{hash}", state.domain);

    let resp = PayResponse {
        callback,
        min_sendable: 1_000,
        max_sendable: 11_000_000_000,
        tag: Tag::PayRequest,
        metadata,
        comment_allowed: None,
        allows_nostr: Some(true),
        nostr_pubkey: Some(*state.keys.public_key()),
    };

    Ok(Json(resp))
}

pub async fn get_nip05(
    Extension(state): Extension<State>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let npub = state.keys.public_key().to_string();
    let json = json!({
    "names": {
        "_": npub,
      },
    "relays": {
        npub: state.relays,
      },
    });

    Ok(Json(json))
}

pub(crate) fn handle_anyhow_error(err: anyhow::Error) -> (StatusCode, Json<Value>) {
    let err = json!({
        "status": "ERROR",
        "reason": format!("{err}"),
    });
    (StatusCode::BAD_REQUEST, Json(err))
}
