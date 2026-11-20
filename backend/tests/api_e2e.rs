use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use keepdays_server::{build_app_state, config::AppConfig, create_router};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send_json(
    app: &axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {}", t));
    }
    let req_body = if let Some(b) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&b).unwrap())
    } else {
        Body::empty()
    };

    let resp = app.clone().oneshot(builder.body(req_body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|_| json!({}));
    (status, val)
}

#[tokio::test]
async fn full_keepdays_e2e_lifecycle() {
    let mut cfg = AppConfig::from_env();
    cfg.database_url = "sqlite::memory:".to_string();
    cfg.redis_url = "memory://".to_string();

    let state = build_app_state(cfg).await.expect("state init");
    let app = create_router(state);

    // 1. Health & Beijing Ambient Lighting + Snowflake Dictionary
    let (st, health) = send_json(&app, Method::GET, "/api/v1/system/health", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(health["ambient"]["timezone"], "Asia/Shanghai");

    let (st, dict) = send_json(&app, Method::GET, "/api/v1/system/dict", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!dict["items"].as_array().unwrap().is_empty());

    // 2. Register Partner A (Boyfriend) & Partner B (Girlfriend) + Intruder C
    let (st, reg_a) = send_json(
        &app,
        Method::POST,
        "/api/v1/auth/register",
        None,
        Some(json!({
            "username": "boyfriend",
            "password": "StrongPassword123!",
            "nickname": "阿瑞"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let token_a = reg_a["access_token"].as_str().unwrap().to_string();
    let personal_a = reg_a["default_personal_space_id"].as_str().unwrap().to_string();

    let (st, reg_b) = send_json(
        &app,
        Method::POST,
        "/api/v1/auth/register",
        None,
        Some(json!({
            "username": "girlfriend",
            "password": "SweetPassword123!",
            "nickname": "宝宝"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let token_b = reg_b["access_token"].as_str().unwrap().to_string();
    let doorplate_b = reg_b["user"]["doorplate_no"].as_str().unwrap().to_string();

    // 3. Tenant Isolation Check: Girlfriend CANNOT access Boyfriend's Personal Space (returns 404 anti-enum!)
    let (st, _) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{}/photos", personal_a),
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // 4. Personal Space Pet Restriction Check (Pets belong ONLY to Shared Spaces)
    let (st, _) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{}/pet", personal_a),
        Some(&token_a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // 5. Create Shared Space & Invite Girlfriend by `#doorplate_no` -> Girlfriend Accepts!
    let (st, shared_sp) = send_json(
        &app,
        Method::POST,
        "/api/v1/spaces",
        Some(&token_a),
        Some(json!({ "name": "302号二人小窝", "alias_name": "大厨" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let shared_id = shared_sp["id"].as_str().unwrap().to_string();

    let (st, inv) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/invitations", shared_id),
        Some(&token_a),
        Some(json!({ "target_doorplate_no": format!("#{}", doorplate_b) })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let invite_code = inv["invite_code"].as_str().unwrap().to_string();

    // Before accepting, Girlfriend still gets 404 on Shared Space
    let (st, _) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{}/fridge/bundle", shared_id),
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Girlfriend checks pending inbox & accepts!
    let (st, pending) = send_json(
        &app,
        Method::GET,
        "/api/v1/invitations/pending",
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(pending["invitations"].as_array().unwrap().len(), 1);

    let (st, _) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/invitations/{}/respond", invite_code),
        Some(&token_b),
        Some(json!({ "accept": true, "alias_name": "点菜员" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // 6. Silent Co-Presence ("同屋微感知"): Girlfriend opens Fridge -> Boyfriend sees `partner_in_fridge = true`!
    let (st, _) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/spaces/{}/presence", shared_id),
        Some(&token_b),
        Some(json!({ "focus": "FRIDGE" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, pres_a) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{}/presence", shared_id),
        Some(&token_a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(pres_a["partner_in_fridge"], true);

    // 7. Photo Upload in Personal Space -> Zero-Copy Projection to Shared Space -> Moments Voice Comment!
    let (st, up_img) = send_json(
        &app,
        Method::POST,
        "/api/v1/media/presign-upload",
        Some(&token_a),
        Some(json!({
            "kind_id": 4001,
            "mime_type": "image/webp",
            "size_bytes": 420000,
            "has_thumb": true
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let img_asset_id = up_img["asset_id"].as_str().unwrap().to_string();

    // Post in Personal Space first ("暗房筛选")
    let (st, p_post) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/photos", personal_a),
        Some(&token_a),
        Some(json!({
            "asset_id": img_asset_id,
            "width": 3024,
            "height": 4032,
            "caption": "个人底片草稿",
            "exif": { "location": "青岛" }
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let personal_photo_id = p_post["photo"]["id"].as_str().unwrap().to_string();

    // Zero-Copy Project to Shared Space (`ref_count` becomes 2!)
    let (st, s_post) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/photos/{}/project", personal_a, personal_photo_id),
        Some(&token_a),
        Some(json!({
            "target_space_id": shared_id,
            "override_caption": "和你一起看的海边日落"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let shared_photo_id = s_post["photo"]["id"].as_str().unwrap().to_string();

    // Delete Personal copy -> Shared copy's S3 asset MUST remain intact (`ref_count` drops 2 -> 1)!
    let (st, _) = send_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/spaces/{}/photos/{}", personal_a, personal_photo_id),
        Some(&token_a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Girlfriend posts a 15-sec Voice Waveform Comment on the Shared Photo!
    let (st, up_voice) = send_json(
        &app,
        Method::POST,
        "/api/v1/media/presign-upload",
        Some(&token_b),
        Some(json!({
            "kind_id": 4003,
            "mime_type": "audio/webm",
            "size_bytes": 48000,
            "has_thumb": false
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let voice_asset_id = up_voice["asset_id"].as_str().unwrap().to_string();

    let (st, commented) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/photos/{}/comments", shared_id, shared_photo_id),
        Some(&token_b),
        Some(json!({
            "audio_asset_id": voice_asset_id,
            "duration_sec": 6,
            "waveform": [15, 42, 88, 60, 35, 92, 50, 20]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(commented["photo"]["post"]["comments"].as_array().unwrap().len(), 1);
    assert!(commented["asset_urls"].get(&img_asset_id).is_some());
    assert!(commented["asset_urls"].get(&voice_asset_id).is_some());

    // 8. Fridge Menu + Max-5 Slips Cap + Slip-to-Grocery + F12-Safe Frosted Time Capsule!
    let (st, bundle0) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{}/fridge/bundle", shared_id),
        Some(&token_a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let menu_v = bundle0["menu"]["version"].as_i64().unwrap();

    let (st, bundle1) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/spaces/{}/fridge/menu", shared_id),
        Some(&token_a),
        Some(json!({
            "expected_version": menu_v,
            "categories": ["荤菜", "素菜", "汤羹"],
            "dishes": [
                { "name": "可乐鸡翅", "category": "荤菜", "ingredients": ["鸡翅中", "可乐", "生姜"] },
                { "name": "蒜蓉油麦菜", "category": "素菜", "ingredients": ["油麦菜", "大蒜"] },
                { "name": "番茄鸡蛋汤", "category": "汤羹", "ingredients": ["番茄", "鸡蛋"] }
            ]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let dish0_id = bundle1["menu"]["data"]["dishes"][0]["id"].as_str().unwrap().to_string();

    // Pin 6 menu slips -> Fridge Door MUST strictly retain only the latest 5!
    let mut latest_slip_id = String::new();
    for i in 1..=6 {
        let (st, b) = send_json(
            &app,
            Method::POST,
            &format!("/api/v1/spaces/{}/fridge/slips", shared_id),
            Some(&token_b),
            Some(json!({
                "dish_ids": [dish0_id],
                "note": format!("第 {} 次点菜", i)
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let slips = b["fridge"]["data"]["recent_slips"].as_array().unwrap();
        assert!(slips.len() <= 5);
        latest_slip_id = slips[0]["id"].as_str().unwrap().to_string();
    }

    // One-click convert latest slip's dish ingredients into Board Grocery Todos!
    let (st, b_groc) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/fridge/slips/{}/to-grocery", shared_id, latest_slip_id),
        Some(&token_a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b_groc["board"]["data"]["todos"].as_array().unwrap().len(), 3);

    // Boyfriend writes a Frosted Glass Time-Capsule Note unlocking in Year 2099
    let board_v = b_groc["board"]["version"].as_i64().unwrap();
    let future_unlock = 4_070_908_800_000_i64;
    let (st, _) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/spaces/{}/fridge/board", shared_id),
        Some(&token_a),
        Some(json!({
            "expected_version": board_v,
            "notes": [{
                "text": "惊喜礼物藏在衣柜顶层！",
                "color": "blush",
                "pinned": true,
                "unlock_at": future_unlock
            }],
            "todos": b_groc["board"]["data"]["todos"]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Girlfriend reads Board -> `locked` MUST be true and `text` MUST be redacted to "" (F12-proof)!
    let (st, gf_board) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{}/fridge/bundle", shared_id),
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(gf_board["board"]["data"]["notes"][0]["locked"], true);
    assert_eq!(gf_board["board"]["data"]["notes"][0]["text"], "");

    // 9. Shared Pet Adoption, 5-Min Co-op Sweet Resonance & Paw-Pick Today's Menu!
    let (st, adopted) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/pet/adopt", shared_id),
        Some(&token_b),
        Some(json!({ "name": "团子", "species": "BLACK_CAT" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(adopted["pet"]["adopted"], true);

    // Girlfriend strokes pet first, Boyfriend strokes within 5 mins -> Triggers Sweet Resonance!
    let (st, r1) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/pet/interact", shared_id),
        Some(&token_b),
        Some(json!({ "action": "STROKE" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r1["sweet_resonance_triggered"], false);

    let (st, r2) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/pet/interact", shared_id),
        Some(&token_a),
        Some(json!({ "action": "STROKE" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r2["sweet_resonance_triggered"], true);

    // Pet stretches paw to pick Today's Menu from Family Menu!
    let (st, paw_pick) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/pet/paw-pick-menu", shared_id),
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(paw_pick["picked_dishes"].as_array().unwrap().len(), 3);

    // 10. LinkedIn-style Modular Profile Update + Twitter Hot Timeline Verification!
    let (st, _) = send_json(
        &app,
        Method::PUT,
        "/api/v1/profile/me",
        Some(&token_b),
        Some(json!({
            "nickname": "小宝",
            "signature": "今天想喝番茄汤",
            "sections": [
                { "id": "s1", "title": "饮食忌口", "items": ["不吃香菜", "微辣"] }
            ]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, tl) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{}/timeline?limit=10", shared_id),
        Some(&token_a),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(!tl["items"].as_array().unwrap().is_empty());
    // Updated nickname "小宝" is reflected immediately across hydrated `users` map!
    let uid_b = reg_b["user"]["id"].as_str().unwrap();
    assert_eq!(tl["users"][uid_b]["profile"]["nickname"], "小宝");

    // 11. Built-in Zero-Nginx SPA Static Host Verification (serves compiled frontend/dist/index.html!)
    let spa_resp = app
        .clone()
        .oneshot(Request::builder().method(Method::GET).uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(spa_resp.status(), StatusCode::OK);
    let html_bytes = spa_resp.into_body().collect().await.unwrap().to_bytes();
    let html_str = String::from_utf8_lossy(&html_bytes);
    assert!(html_str.contains("KeepDays"));
}

#[tokio::test]
async fn adversarial_security_and_regression_suite() {
    let tmp_dir = std::env::temp_dir().join(format!(
        "keepdays_test_s3_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut cfg = AppConfig::from_env();
    cfg.database_url = "sqlite::memory:".to_string();
    cfg.redis_url = "memory://".to_string();
    cfg.s3_endpoint = "/local-s3".to_string();
    cfg.local_s3_dir = tmp_dir.to_string_lossy().to_string();
    cfg.admin_api_key = None;

    let state = build_app_state(cfg.clone()).await.expect("state init");
    let app = create_router(state.clone());

    // 1. Zombie JWT (valid HMAC for non-existent user) MUST return 401, NEVER 500 FK failure!
    let zombie_claims = keepdays_server::middleware::guards::JwtClaims {
        sub: "99999999999999999".to_string(),
        iat: 1_700_000_000,
        exp: 4_000_000_000,
    };
    let zombie_jwt = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &zombie_claims,
        &jsonwebtoken::EncodingKey::from_secret(cfg.jwt_secret.as_bytes()),
    )
    .unwrap();
    let (st, _) = send_json(
        &app,
        Method::POST,
        "/api/v1/spaces",
        Some(&zombie_jwt),
        Some(json!({ "name": "GhostSpace" })),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // 2. Register Alice, Bob, and Intruder Eve (capturing Alice's HttpOnly refresh cookie)
    let reg_alice_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/auth/register")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "username": "alice_sec",
                        "password": "AlicePassword123!",
                        "nickname": "Alice"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reg_alice_resp.status(), StatusCode::OK);
    let set_cookie = reg_alice_resp
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let cookie_kv = set_cookie.split(';').next().unwrap().to_string();
    let alice_json: Value = serde_json::from_slice(
        &reg_alice_resp.into_body().collect().await.unwrap().to_bytes(),
    )
    .unwrap();
    let token_alice = alice_json["access_token"].as_str().unwrap().to_string();
    let personal_alice = alice_json["default_personal_space_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Concurrent refresh within 10s grace window MUST both succeed without logging user out!
    for _ in 0..2 {
        let r_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/auth/refresh")
                    .header(header::COOKIE, &cookie_kv)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_resp.status(), StatusCode::OK);
    }

    let (_, bob_json) = send_json(
        &app,
        Method::POST,
        "/api/v1/auth/register",
        None,
        Some(json!({ "username": "bob_sec", "password": "BobPassword123!", "nickname": "Bob" })),
    )
    .await;
    let token_bob = bob_json["access_token"].as_str().unwrap().to_string();
    let uid_bob = bob_json["user"]["id"].as_str().unwrap().to_string();
    let dp_bob = bob_json["user"]["doorplate_no"].as_str().unwrap().to_string();

    let (_, eve_json) = send_json(
        &app,
        Method::POST,
        "/api/v1/auth/register",
        None,
        Some(json!({ "username": "eve_sec", "password": "EvePassword123!", "nickname": "Eve" })),
    )
    .await;
    let token_eve = eve_json["access_token"].as_str().unwrap().to_string();

    // 3. Privilege Escalation Check: Eve cannot mutate global Snowflake dictionary
    let (st, _) = send_json(
        &app,
        Method::POST,
        "/api/v1/system/dict/items",
        Some(&token_eve),
        Some(json!({ "id": 4001, "domain_id": 40, "code": "PHOTO", "label": "Hacked" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // 4. Unaccepted Invite Profile Privacy Check: Eve invites Bob by doorplate, CANNOT read Bob's profile before Bob consents!
    let (_, eve_sp) = send_json(
        &app,
        Method::POST,
        "/api/v1/spaces",
        Some(&token_eve),
        Some(json!({ "name": "EveTrap" })),
    )
    .await;
    let eve_sp_id = eve_sp["id"].as_str().unwrap();
    let (st, _) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/invitations", eve_sp_id),
        Some(&token_eve),
        Some(json!({ "target_doorplate_no": dp_bob })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/profile/{}", uid_bob),
        Some(&token_eve),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // 5. Cryptographic `/local-s3` Verification + HTTP Range `206` + Cross-Tenant Asset IDOR Protection
    let (st, presign) = send_json(
        &app,
        Method::POST,
        "/api/v1/media/presign-upload",
        Some(&token_alice),
        Some(json!({
            "kind_id": 4001,
            "mime_type": "image/webp",
            "size_bytes": 12,
            "has_thumb": false
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let alice_aid = presign["asset_id"].as_str().unwrap().to_string();
    let put_url = presign["put_url"].as_str().unwrap().to_string();
    assert!(put_url.starts_with("/local-s3/"));

    // Forged signature MUST be 403 Forbidden
    let forged_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/local-s3/keepdays-private/photos/123.webp?X-Amz-Signature=forged")
                .body(Body::from("hacked"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forged_resp.status(), StatusCode::FORBIDDEN);

    // Valid signed PUT succeeds
    let real_put = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(&put_url)
                .body(Body::from("0123456789AB"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(real_put.status(), StatusCode::OK);

    // Replaying PUT presigned URL as GET or DELETE MUST be 403 Forbidden!
    let wrong_method = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(&put_url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong_method.status(), StatusCode::FORBIDDEN);

    // Eve CANNOT hijack Alice's asset_id into Eve's space or hydrate Alice's asset_id!
    let (st, _) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/photos", eve_sp_id),
        Some(&token_eve),
        Some(json!({ "asset_id": alice_aid, "width": 800, "height": 600 })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (st, eve_hyd) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/media/hydrate?ids={}", alice_aid),
        Some(&token_eve),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(eve_hyd.as_object().unwrap().is_empty());

    // Alice binds photo & verifies HTTP Range `206 Partial Content` on presigned GET URL
    let (st, alice_photo) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/photos", personal_alice),
        Some(&token_alice),
        Some(json!({ "asset_id": alice_aid, "width": 800, "height": 600 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let get_url = alice_photo["asset_urls"][&alice_aid]["full_url"]
        .as_str()
        .unwrap();
    let range_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(get_url)
                .header(header::RANGE, "bytes=2-5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(range_resp.status(), StatusCode::PARTIAL_CONTENT);
    let slice_bytes = range_resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&slice_bytes[..], b"2345");

    // 6. Shared Space: Chrome `audio/webm;codecs=opus` Voice Comment + Comment Deletion + Post-Unlock Time-Capsule Safety + Pet Resonance Anti-Farming & Adventure FK
    let (_, shared_ab) = send_json(
        &app,
        Method::POST,
        "/api/v1/spaces",
        Some(&token_alice),
        Some(json!({ "name": "Alice & Bob" })),
    )
    .await;
    let ab_id = shared_ab["id"].as_str().unwrap().to_string();
    let (_, inv_ab) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/invitations", ab_id),
        Some(&token_alice),
        Some(json!({ "target_doorplate_no": dp_bob })),
    )
    .await;
    let _ = send_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/invitations/{}/respond",
            inv_ab["invite_code"].as_str().unwrap()
        ),
        Some(&token_bob),
        Some(json!({ "accept": true })),
    )
    .await;

    // Chrome `audio/webm;codecs=opus` presign MUST succeed!
    let (st, v_pre) = send_json(
        &app,
        Method::POST,
        "/api/v1/media/presign-upload",
        Some(&token_bob),
        Some(json!({
            "kind_id": 4003,
            "mime_type": "audio/webm;codecs=opus",
            "size_bytes": 4096,
            "has_thumb": false
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let voice_id = v_pre["asset_id"].as_str().unwrap().to_string();

    let (_, proj_p) = send_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/spaces/{}/photos/{}/project",
            personal_alice,
            alice_photo["photo"]["id"].as_str().unwrap()
        ),
        Some(&token_alice),
        Some(json!({ "target_space_id": ab_id })),
    )
    .await;
    let ab_photo_id = proj_p["photo"]["id"].as_str().unwrap().to_string();

    let (st, with_c) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/photos/{}/comments", ab_id, ab_photo_id),
        Some(&token_bob),
        Some(json!({ "audio_asset_id": voice_id, "duration_sec": 4, "waveform": [30, 70, 50] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let cid = with_c["photo"]["post"]["comments"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (st, after_del) = send_json(
        &app,
        Method::DELETE,
        &format!(
            "/api/v1/spaces/{}/photos/{}/comments/{}",
            ab_id, ab_photo_id, cid
        ),
        Some(&token_bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(after_del["photo"]["post"]["comments"].as_array().unwrap().is_empty());

    // Time-Capsule Post-Unlock Redaction Safety:
    let (_, b0) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{}/fridge/bundle", ab_id),
        Some(&token_alice),
        None,
    )
    .await;
    let unlock_soon = keepdays_server::time_bj::now_ms() + 120;
    let (st, _) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/spaces/{}/fridge/board", ab_id),
        Some(&token_alice),
        Some(json!({
            "expected_version": b0["board"]["version"],
            "notes": [{ "text": "周年快乐！", "color": "oat", "pinned": false, "unlock_at": unlock_soon }],
            "todos": []
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Bob fetches while still locked (`text` == "")
    let (_, bob_locked) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{}/fridge/bundle", ab_id),
        Some(&token_bob),
        None,
    )
    .await;
    assert_eq!(bob_locked["board"]["data"]["notes"][0]["text"], "");

    // Wait until AFTER unlock_at, then Bob submits his stale `notes` array (holding `text: ""`) while adding a todo!
    tokio::time::sleep(std::time::Duration::from_millis(160)).await;
    let (st, bob_mut) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/spaces/{}/fridge/board", ab_id),
        Some(&token_bob),
        Some(json!({
            "expected_version": bob_locked["board"]["version"],
            "notes": bob_locked["board"]["data"]["notes"],
            "todos": [{ "text": "买蛋糕", "done": false }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // Alice's time-capsule message MUST NOT have been wiped to ""!
    assert_eq!(bob_mut["board"]["data"]["notes"][0]["text"], "周年快乐！");

    // Pet unowned accessory equip blocked + Resonance single-fire cooldown + Adventure FK check
    let _ = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/pet/adopt", ab_id),
        Some(&token_alice),
        Some(json!({ "name": "煤球", "species": "BLACK_CAT" })),
    )
    .await;

    let (st, _) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/pet/equip", ab_id),
        Some(&token_alice),
        Some(json!({ "accessory": "red_scarf" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let _ = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/pet/interact", ab_id),
        Some(&token_alice),
        Some(json!({ "action": "STROKE" })),
    )
    .await;
    let (_, res_hit) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/pet/interact", ab_id),
        Some(&token_bob),
        Some(json!({ "action": "STROKE" })),
    )
    .await;
    assert_eq!(res_hit["sweet_resonance_triggered"], true);

    // Immediate second stroke MUST NOT trigger infinite resonance farming!
    let (_, res_spam) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{}/pet/interact", ab_id),
        Some(&token_bob),
        Some(json!({ "action": "STROKE" })),
    )
    .await;
    assert_eq!(res_spam["sweet_resonance_triggered"], false);

    // Simulate completed adventure settlement -> must insert timeline event cleanly with zero FK error!
    let ab_sid_i64: i64 = ab_id.parse().unwrap();
    let mut pet_doc = keepdays_server::domains::pet::model::PetStateDoc::unadopted();
    pet_doc.adopted = true;
    pet_doc.name = "煤球".to_string();
    pet_doc.last_tick_at = keepdays_server::time_bj::now_ms() - 1_000;
    pet_doc.current.behavior = "ADVENTURING".to_string();
    pet_doc.current.adventure_return_at = Some(keepdays_server::time_bj::now_ms() - 10);
    let settled =
        keepdays_server::domains::pet::engine::evaluate_pet_lazy_tick(&state, ab_sid_i64, &mut pet_doc)
            .await;
    assert!(settled);
    assert_eq!(pet_doc.souvenirs.len(), 1);

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[tokio::test]
async fn pet_agent_kernel_e2e_and_isolation_suite() {
    use keepdays_server::agent::{
        memory::{
            apply_ltm_git_commit, extract_sealed_session_to_stm, recall_stm_segments, user_scope,
            LtmDiffOp,
        },
        sandbox::VirtualFs,
        tools::{AgentTool, BoxFuture, ToolExecContext, ToolExecOutput},
    };
    use std::sync::Arc;

    let tmp_dir = std::env::temp_dir().join(format!(
        "keepdays_agent_test_{}",
        keepdays_server::time_bj::now_ms()
    ));
    let s3_dir = tmp_dir.join("s3");
    let fs_dir = tmp_dir.join("agent_fs");

    let mut cfg = AppConfig::from_env();
    cfg.database_url = "sqlite::memory:".to_string();
    cfg.redis_url = "memory://".to_string();
    cfg.s3_endpoint = "/local-s3".to_string();
    cfg.local_s3_dir = s3_dir.to_string_lossy().to_string();
    cfg.agent_fs_dir = fs_dir.to_string_lossy().to_string();
    cfg.jwt_secret = "super_secret_jwt_must_never_leak_into_bash_env_998877".to_string();

    let state = build_app_state(cfg).await.expect("state init");

    // 1. Plug-and-Play Custom Tool Registration (`Mode A` Trait + `search_tool` + `use_tool`)
    struct AnniversaryTool;
    impl AgentTool for AnniversaryTool {
        fn name(&self) -> &'static str {
            "custom__anniversary_countdown"
        }
        fn domain(&self) -> &'static str {
            "custom"
        }
        fn description(&self) -> &'static str {
            "计算恋爱纪念日倒计时天数"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }
        fn run<'a>(
            &'a self,
            _ctx: &'a ToolExecContext,
            _args: Value,
        ) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
            Box::pin(async move {
                Ok(ToolExecOutput {
                    display_summary: "已计算纪念日倒计时".to_string(),
                    model_text: "距离周年纪念日还有 12 天".to_string(),
                    emitted_surface: None,
                })
            })
        }
    }
    state.agent.registry.register(Arc::new(AnniversaryTool));

    let app = create_router(state.clone());

    // Register Partner A & Partner B and join Shared Space
    let (_, reg_a) = send_json(
        &app,
        Method::POST,
        "/api/v1/auth/register",
        None,
        Some(json!({ "username": "agent_a", "password": "Password123!", "nickname": "阿瑞" })),
    )
    .await;
    let token_a = reg_a["access_token"].as_str().unwrap().to_string();
    let uid_a: i64 = reg_a["user"]["id"].as_str().unwrap().parse().unwrap();

    let (_, reg_b) = send_json(
        &app,
        Method::POST,
        "/api/v1/auth/register",
        None,
        Some(json!({ "username": "agent_b", "password": "Password123!", "nickname": "小满" })),
    )
    .await;
    let token_b = reg_b["access_token"].as_str().unwrap().to_string();
    let uid_b: i64 = reg_b["user"]["id"].as_str().unwrap().parse().unwrap();
    let door_b = reg_b["user"]["doorplate_no"].as_str().unwrap().to_string();

    let (_, sp) = send_json(
        &app,
        Method::POST,
        "/api/v1/spaces",
        Some(&token_a),
        Some(json!({ "name": "宠物Agent实验室" })),
    )
    .await;
    let space_id_str = sp["id"].as_str().unwrap().to_string();
    let space_id: i64 = space_id_str.parse().unwrap();

    let (_, inv) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{space_id_str}/invitations"),
        Some(&token_a),
        Some(json!({ "target_doorplate_no": door_b })),
    )
    .await;
    send_json(
        &app,
        Method::POST,
        &format!("/api/v1/invitations/{}/respond", inv["invite_code"].as_str().unwrap()),
        Some(&token_b),
        Some(json!({ "accept": true })),
    )
    .await;

    send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{space_id_str}/pet/adopt"),
        Some(&token_a),
        Some(json!({ "name": "团子", "species": "BLACK_CAT" })),
    )
    .await;

    // 2. VirtualFs & Bash Sandbox Security Verification
    let vfs_a = VirtualFs::ensure_for(&state.cfg.agent_fs_dir, space_id, uid_a)
        .await
        .unwrap();
    assert!(vfs_a.resolve_virtual_path("../../etc/passwd").is_err());
    assert!(vfs_a.resolve_virtual_path("/etc/keepdays/keepdays.env").is_err());
    assert!(vfs_a.resolve_virtual_path("~/private/secret_plan.md").is_ok());
    assert!(vfs_a.resolve_virtual_path("~/shared/grocery.md").is_ok());

    let bash_out = vfs_a.run_bash("pwd && echo \"JWT=$JWT_SECRET\"").await.unwrap();
    assert!(bash_out.contains("~/private"), "host path must be scrubbed to ~/private: {bash_out}");
    assert!(!bash_out.contains("super_secret_jwt"), "env_clear must strip parent secrets!");
    assert!(bash_out.contains("[cwd: ~/private | shared: ~/shared]"));

    let tctx = ToolExecContext {
        state: state.clone(),
        space_id,
        user_id: uid_a,
        session_id: "test_sess".to_string(),
        vfs: vfs_a,
    };
    let searched = state
        .agent
        .registry
        .dispatch(&tctx, "search_tool", json!({ "query": "纪念日 anniversary" }))
        .await
        .unwrap();
    assert!(searched.model_text.contains("custom__anniversary_countdown"));
    let invoked = state
        .agent
        .registry
        .dispatch(
            &tctx,
            "use_tool",
            json!({ "tool_name": "custom__anniversary_countdown", "arguments": {} }),
        )
        .await
        .unwrap();
    assert!(invoked.model_text.contains("12 天"));

    // 3. Streaming Chat + Google A2UI Dynamic Surface + Closed-Loop `ui_action` Auto-Pinning Fridge Menu
    let req_stream = Request::builder()
        .method(Method::POST)
        .uri(&format!("/api/v1/spaces/{space_id_str}/pet/agent/chat/stream"))
        .header(header::AUTHORIZATION, format!("Bearer {token_a}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({ "text": "今晚菜单吃什么？秘密备忘：下周买胶片相机送小满" }))
                .unwrap(),
        ))
        .unwrap();
    let resp_stream = app.clone().oneshot(req_stream).await.unwrap();
    assert_eq!(resp_stream.status(), StatusCode::OK);
    let sse_raw = String::from_utf8(
        resp_stream.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
    .unwrap();
    assert!(sse_raw.contains("event: a2ui_surface"), "must stream Google A2UI surface: {sse_raw}");
    assert!(sse_raw.contains("CheckboxGroup"));

    // Verify Partner A's session is stored in S3 & STRICTLY invisible to Partner B!
    let (_, a_boot) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/sessions"),
        Some(&token_a),
        None,
    )
    .await;
    let sess_a = a_boot["active_session_id"].as_str().unwrap().to_string();
    assert_eq!(a_boot["turns"].as_array().unwrap().len(), 2);

    // Partner B tries to read Partner A's session -> MUST return 404!
    let (st_forbid, _) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/sessions/{sess_a}"),
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(st_forbid, StatusCode::NOT_FOUND);

    // Partner A checks dishes on A2UI checklist -> auto-sends `ui_action` -> pins today's menu onto Fridge!
    let surface_id = a_boot["turns"][1]["surfaces"][0]["surface_id"]
        .as_str()
        .unwrap()
        .to_string();
    let req_act = Request::builder()
        .method(Method::POST)
        .uri(&format!("/api/v1/spaces/{space_id_str}/pet/agent/chat/stream"))
        .header(header::AUTHORIZATION, format!("Bearer {token_a}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "session_id": sess_a,
                "ui_action": {
                    "surface_id": surface_id,
                    "action_name": "confirm_dinner_menu",
                    "data_model": {
                        "dishes": ["可乐鸡翅", "清炒时蔬"],
                        "less_spicy": true,
                        "note": "七点半开饭"
                    },
                    "summary_label": "已选：可乐鸡翅、清炒时蔬"
                }
            }))
            .unwrap(),
        ))
        .unwrap();
    let resp_act = app.clone().oneshot(req_act).await.unwrap();
    assert_eq!(resp_act.status(), StatusCode::OK);
    let _ = resp_act.into_body().collect().await.unwrap();

    // Verify Fridge now actually has ["可乐鸡翅", "清炒时蔬"] pinned!
    let (_, f_bundle) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{space_id_str}/fridge/bundle"),
        Some(&token_b),
        None,
    )
    .await;
    let slips = f_bundle["fridge"]["data"]["recent_slips"].as_array().unwrap();
    assert_eq!(slips.len(), 1);
    assert_eq!(slips[0]["dishes"], json!(["可乐鸡翅", "清炒时蔬"]));
    assert_eq!(slips[0]["note"], "少辣 · 七点半开饭");

    // 4. Seal Session -> Extract STM -> Verify Chinese BM25 + MMR Recall & Speaker Privacy Wall
    extract_sealed_session_to_stm(&state, space_id, uid_a, &sess_a)
        .await
        .unwrap();
    let hits_for_a = recall_stm_segments(&state, space_id, uid_a, "胶片相机 可乐鸡翅", 4).await;
    assert!(!hits_for_a.is_empty(), "Partner A must recall own STM via Chinese BM25");
    let hits_for_b = recall_stm_segments(&state, space_id, uid_b, "胶片相机", 4).await;
    assert!(hits_for_b.is_empty(), "Partner B must NEVER recall Partner A's private surprise STM!");

    // 5. Git-Style LTM Commit Evolution (`ADD` -> `REPLACE`) + Privacy Isolation + One-Click `Revert`
    apply_ltm_git_commit(
        &state,
        space_id,
        &user_scope(uid_a),
        &sess_a,
        "记录秘密礼物",
        vec![LtmDiffOp {
            op: "ADD".into(),
            slot_key: "surprise.gift".into(),
            old_val: None,
            new_val: Some("准备送胶片相机".into()),
            reason: "私密惊喜".into(),
        }],
    )
    .await
    .unwrap();

    apply_ltm_git_commit(
        &state,
        space_id,
        "shared",
        &sess_a,
        "初始咖啡偏好",
        vec![LtmDiffOp {
            op: "ADD".into(),
            slot_key: "diet.coffee".into(),
            old_val: None,
            new_val: Some("每天一杯冰美式".into()),
            reason: "日常习惯".into(),
        }],
    )
    .await
    .unwrap();

    let commit_2 = apply_ltm_git_commit(
        &state,
        space_id,
        "shared",
        &sess_a,
        "咖啡习惯演进",
        vec![LtmDiffOp {
            op: "REPLACE".into(),
            slot_key: "diet.coffee".into(),
            old_val: None,
            new_val: Some("近期养胃改喝热燕麦拿铁".into()),
            reason: "状态演进".into(),
        }],
    )
    .await
    .unwrap()
    .unwrap();

    // Partner B inspects memory -> sees `shared` ("热燕麦拿铁"), NEVER sees `user:{uid_a}` ("准备送胶片相机")!
    let (_, mem_b) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/memory"),
        Some(&token_b),
        None,
    )
    .await;
    let mem_b_str = mem_b.to_string();
    assert!(mem_b_str.contains("热燕麦拿铁"));
    assert!(!mem_b_str.contains("胶片相机"));

    // Revert `commit_2` -> `diet.coffee` rolls cleanly back to `"每天一杯冰美式"`!
    let (st_rev, _) = send_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/spaces/{space_id_str}/pet/agent/memory/revert/{}",
            commit_2.rev_id
        ),
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(st_rev, StatusCode::OK);

    let (_, mem_after_rev) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/memory"),
        Some(&token_b),
        None,
    )
    .await;
    assert!(mem_after_rev["ltm_head"].to_string().contains("每天一杯冰美式"));

    // 7. Toolbox Markdown Memory Direct Edit (`SHARED.md` & `PRIVATE.md`) + Speaker Isolation
    let (st_md_shared, md_shared_res) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/memory/markdown"),
        Some(&token_a),
        Some(json!({
            "scope": "shared",
            "markdown": "# 家庭公共记忆\n\n- 都不吃香菜和折耳根\n\n## 纪念日\n- **anniversary**: 每年 11 月 19 日"
        })),
    )
    .await;
    assert_eq!(st_md_shared, StatusCode::OK);
    assert!(md_shared_res["shared_markdown"]
        .as_str()
        .unwrap_or_default()
        .contains("都不吃香菜和折耳根"));

    let (st_md_priv, _) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/memory/markdown"),
        Some(&token_a),
        Some(json!({
            "scope": "private",
            "markdown": "# 我的专属私密记忆\n\n- 下周偷偷买北极光机票"
        })),
    )
    .await;
    assert_eq!(st_md_priv, StatusCode::OK);

    // Partner B reads Markdown -> sees Shared Markdown, NEVER sees Partner A's Private Markdown!
    let (_, b_md_check) = send_json(
        &app,
        Method::GET,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/memory/markdown"),
        Some(&token_b),
        None,
    )
    .await;
    assert!(b_md_check["shared_markdown"]
        .as_str()
        .unwrap_or_default()
        .contains("每年 11 月 19 日"));
    assert!(!b_md_check.to_string().contains("北极光机票"));

    // 8. CC-Switch Zero-URL Vendor Token Configuration & Hot-Pluggable Switch
    let (st_mm, mm_res) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/providers"),
        Some(&token_a),
        Some(json!({
            "vendor": "minimax",
            "name": "MiniMax",
            "api_key": "sk-mm-secret9876",
            "is_chat_primary": true,
            "is_memory_primary": true,
            "enabled": true
        })),
    )
    .await;
    assert_eq!(st_mm, StatusCode::OK);
    let mm_id = mm_res["providers"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        mm_res["providers"][0]["base_url"].as_str().unwrap(),
        "https://api.minimaxi.com/v1"
    );
    assert_eq!(
        mm_res["providers"][0]["chat_model"].as_str().unwrap(),
        "MiniMax-M2.7"
    );

    let (st_kimi, kimi_res) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/providers"),
        Some(&token_a),
        Some(json!({
            "vendor": "kimi",
            "name": "Kimi",
            "api_key": "sk-kimi-secret5432",
            "is_chat_primary": true,
            "is_memory_primary": true,
            "enabled": true
        })),
    )
    .await;
    assert_eq!(st_kimi, StatusCode::OK);
    assert_eq!(kimi_res["providers"][0]["name"].as_str().unwrap(), "Kimi");

    // Hot-switch back to MiniMax in 1 click without re-sending API key or URL!
    let (st_hot, hot_res) = send_json(
        &app,
        Method::POST,
        &format!("/api/v1/spaces/{space_id_str}/pet/agent/providers/{mm_id}/activate"),
        Some(&token_b),
        None,
    )
    .await;
    assert_eq!(st_hot, StatusCode::OK);
    assert_eq!(hot_res["providers"][0]["id"].as_str().unwrap(), mm_id);
    assert!(hot_res["providers"][0]["is_chat_primary"].as_bool().unwrap());

    // Delete test providers so builtin fallback stays active if needed
    for p in hot_res["providers"].as_array().unwrap() {
        let pid = p["id"].as_str().unwrap();
        if !pid.starts_with("env_") {
            let _ = send_json(
                &app,
                Method::DELETE,
                &format!("/api/v1/spaces/{space_id_str}/pet/agent/providers/{pid}"),
                Some(&token_a),
                None,
            )
            .await;
        }
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
