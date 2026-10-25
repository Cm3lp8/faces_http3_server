use super::*;

const RETRY_CID: [u8; quiche::MAX_CONN_ID_LEN] = [0x42; quiche::MAX_CONN_ID_LEN];
const ISSUED: Duration = Duration::from_millis(123_456);

fn tokens(key: u8) -> RetryTokens {
    RetryTokens {
        key: hmac::Key::new(hmac::HMAC_SHA256, &[key; 32]),
        started: Instant::now(),
    }
}

fn source() -> SocketAddr {
    "127.0.0.1:42000".parse().unwrap()
}

fn mint(tokens: &RetryTokens) -> Vec<u8> {
    tokens
        .mint_at(&[0x35; 16], &RETRY_CID, &source(), ISSUED)
        .unwrap()
}

// Sign malformed bodies with the test key to exercise structural validation
// independently of MAC rejection. No production API permits arbitrary signing.
fn signed_body(tokens: &RetryTokens, mut body: Vec<u8>) -> Vec<u8> {
    let tag = hmac::sign(
        &tokens.key,
        &authentication_input(&body, &RETRY_CID, &source()),
    );
    body.extend_from_slice(tag.as_ref());
    body
}

#[test]
fn valid_tokens_round_trip_ipv4_ipv6_and_all_odcid_lengths() {
    let tokens = tokens(1);
    for source in ["127.0.0.1:42000", "[::1]:42000"] {
        let source = source.parse().unwrap();
        for length in 8..=20 {
            let cid = vec![0x35; length];
            let token = tokens.mint_at(&cid, &RETRY_CID, &source, ISSUED).unwrap();
            assert_eq!(token.len(), 42 + length);
            assert_eq!(
                tokens
                    .validate_at(&token, &RETRY_CID, &source, ISSUED)
                    .unwrap()
                    .as_ref(),
                cid
            );
        }
    }
}

#[test]
fn runtime_clock_and_generated_key_round_trip() {
    let tokens = RetryTokens::new().unwrap();
    let token = tokens.mint(&[7; 8], &RETRY_CID, &source()).unwrap();
    assert_eq!(
        tokens
            .validate(&token, &RETRY_CID, &source())
            .unwrap()
            .as_ref(),
        [7; 8]
    );
}

#[test]
fn forged_tokens_without_server_issuance_are_rejected() {
    let tokens = tokens(1);
    let mut legacy = b"quiche".to_vec();
    legacy.extend_from_slice(&[127, 0, 0, 1]);
    legacy.extend_from_slice(&[0x55; 16]);
    assert!(tokens
        .validate_at(&legacy, &RETRY_CID, &source(), ISSUED)
        .is_none());
    let mut forged = vec![VERSION];
    forged.extend_from_slice(&(ISSUED.as_millis() as u64).to_be_bytes());
    forged.push(16);
    forged.extend_from_slice(&[0x55; 16]);
    forged.extend_from_slice(&[0; TAG_LEN]);
    assert!(tokens
        .validate_at(&forged, &RETRY_CID, &source(), ISSUED)
        .is_none());
}

#[test]
fn every_token_byte_including_odcid_and_tag_is_authenticated() {
    let tokens = tokens(1);
    let token = mint(&tokens);
    for index in 0..token.len() {
        let mut changed = token.clone();
        changed[index] ^= 1;
        assert!(
            tokens
                .validate_at(&changed, &RETRY_CID, &source(), ISSUED)
                .is_none(),
            "byte {index}"
        );
    }
}

#[test]
fn mint_rejects_invalid_original_and_retry_connection_id_lengths() {
    let tokens = tokens(1);
    for length in [0, 1, 7, 21, 255, 1024] {
        assert!(tokens
            .mint_at(&vec![1; length], &RETRY_CID, &source(), ISSUED)
            .is_none());
    }
    for length in [0, 1, 8, 19, 21, 255, 1024] {
        assert!(tokens
            .mint_at(&[1; 16], &vec![2; length], &source(), ISSUED)
            .is_none());
    }
}

#[test]
fn signed_invalid_original_connection_id_lengths_are_rejected() {
    let tokens = tokens(1);
    for length in [0, 1, 7, 21, 255] {
        let mut body = vec![VERSION];
        body.extend_from_slice(&(ISSUED.as_millis() as u64).to_be_bytes());
        body.push(length as u8);
        body.extend(vec![0x35; length]);
        let token = signed_body(&tokens, body);
        assert!(tokens
            .validate_at(&token, &RETRY_CID, &source(), ISSUED)
            .is_none());
    }
    assert!(tokens
        .validate_at(&vec![0; 1024], &RETRY_CID, &source(), ISSUED)
        .is_none());
}

#[test]
fn wrong_retry_connection_id_or_length_is_rejected() {
    let tokens = tokens(1);
    let token = mint(&tokens);
    for length in [0, 1, 8, 19, 21, 255, 1024] {
        assert!(tokens
            .validate_at(&token, &vec![0x42; length], &source(), ISSUED)
            .is_none());
    }
    for index in 0..RETRY_CID.len() {
        let mut other = RETRY_CID;
        other[index] ^= 1;
        assert!(tokens
            .validate_at(&token, &other, &source(), ISSUED)
            .is_none());
    }
}

#[test]
fn changed_ip_port_or_address_family_is_rejected() {
    let tokens = tokens(1);
    for (source, other) in [
        ("127.0.0.1:42000", "127.0.0.2:42000"),
        ("127.0.0.1:42000", "127.0.0.1:42001"),
        ("[::1]:42000", "[::2]:42000"),
        ("[::1]:42000", "[::1]:42001"),
        ("127.0.0.1:42000", "[::ffff:127.0.0.1]:42000"),
    ] {
        let source = source.parse().unwrap();
        let other = other.parse().unwrap();
        let token = tokens
            .mint_at(&[1; 16], &RETRY_CID, &source, ISSUED)
            .unwrap();
        assert!(tokens
            .validate_at(&token, &RETRY_CID, &other, ISSUED)
            .is_none());
    }
}

#[test]
fn every_truncation_suffix_and_oversized_token_is_rejected() {
    let tokens = tokens(1);
    for length in [8, 20] {
        let token = tokens
            .mint_at(&vec![1; length], &RETRY_CID, &source(), ISSUED)
            .unwrap();
        for end in 0..token.len() {
            assert!(tokens
                .validate_at(&token[..end], &RETRY_CID, &source(), ISSUED)
                .is_none());
        }
        let mut extended = token;
        extended.push(0);
        assert!(tokens
            .validate_at(&extended, &RETRY_CID, &source(), ISSUED)
            .is_none());
    }
    assert!(tokens
        .validate_at(&vec![0; 65_535], &RETRY_CID, &source(), ISSUED)
        .is_none());
}

#[test]
fn signed_unknown_version_and_length_mismatch_are_rejected() {
    let tokens = tokens(1);
    let token = mint(&tokens);
    for (index, value) in [(0, 0), (0, 2), (9, 8), (9, 20)] {
        let mut body = token[..token.len() - TAG_LEN].to_vec();
        body[index] = value;
        let changed = signed_body(&tokens, body);
        assert!(tokens
            .validate_at(&changed, &RETRY_CID, &source(), ISSUED)
            .is_none());
    }
}

#[test]
fn expiration_rejects_exact_ttl_future_and_clock_overflow() {
    let tokens = tokens(1);
    let token = mint(&tokens);
    for age in [0, 1, TTL_MILLIS - 1] {
        assert!(tokens
            .validate_at(
                &token,
                &RETRY_CID,
                &source(),
                ISSUED + Duration::from_millis(age)
            )
            .is_some());
    }
    for now in [
        ISSUED - Duration::from_millis(1),
        ISSUED + Duration::from_millis(TTL_MILLIS),
        ISSUED + Duration::from_millis(TTL_MILLIS + 1),
        Duration::MAX,
    ] {
        assert!(tokens
            .validate_at(&token, &RETRY_CID, &source(), now)
            .is_none());
    }
    assert!(tokens
        .mint_at(&[1; 16], &RETRY_CID, &source(), Duration::MAX)
        .is_none());
    let future = tokens
        .mint_at(
            &[1; 16],
            &RETRY_CID,
            &source(),
            Duration::from_millis(u64::MAX),
        )
        .unwrap();
    assert!(tokens
        .validate_at(&future, &RETRY_CID, &source(), ISSUED)
        .is_none());
}

#[test]
fn new_key_invalidates_outstanding_tokens() {
    let first = tokens(1);
    let second = tokens(2);
    let token = mint(&first);
    assert!(second
        .validate_at(&token, &RETRY_CID, &source(), ISSUED)
        .is_none());
}

#[test]
fn valid_token_remains_reusable_within_its_window() {
    let tokens = tokens(1);
    let token = mint(&tokens);
    for age in [0, 0, 1, TTL_MILLIS - 1] {
        assert!(tokens
            .validate_at(
                &token,
                &RETRY_CID,
                &source(),
                ISSUED + Duration::from_millis(age)
            )
            .is_some());
    }
}
